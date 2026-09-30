use axum::{
    Extension, Json,
    body::{Body, Bytes},
    extract::{Path, Query},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::Arc;
use tracing::info;
use uuid::Uuid;

use super::{
    api_error, now, parse_json, require_auth,
    webhook::{self, Signing, WebhookAttempt},
};
use crate::{AppState, Event};

pub const REGISTRATION_STATUSES: [&str; 8] = [
    "IN_QUEUE",
    "IN_PROGRESS",
    "APPROVED",
    "REJECTED",
    "PENDING",
    "ON_HOLD",
    "PENDING_APPROVAL",
    "PENDING_ATTACHMENTS",
];

/// no status changes follow these
pub const FINAL_REGISTRATION_STATUSES: [&str; 2] = ["APPROVED", "REJECTED"];

#[derive(Debug, PartialEq, Eq)]
pub enum SetStatusError {
    NotFound,
    AlreadyFinal,
}

/// markets offered by the mock, every market resolves to a generic policy
const MARKETS: [(&str, &str); 11] = [
    ("BE", "Belgium"),
    ("DE", "Germany"),
    ("DK", "Denmark"),
    ("ES", "Spain"),
    ("FI", "Finland"),
    ("FR", "France"),
    ("GB", "United Kingdom"),
    ("NL", "Netherlands"),
    ("NO", "Norway"),
    ("PL", "Poland"),
    ("SE", "Sweden"),
];

const POLICY_PREFIX: &str = "pincer-";
const ATTACHMENT_ID: &str = "letter-of-authorization";
static TEMPLATE_PDF: &[u8] = include_bytes!("../../blank.pdf");

#[derive(Clone, Debug, Serialize)]
pub struct RegistrationLog {
    pub create_time: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct RegistrationAttachment {
    pub attachment_id: String,
    pub filename: Option<String>,
    pub size: usize,
}

/// a sender ID registration submitted to the Registrations API
#[derive(Clone, Debug, Serialize)]
pub struct Registration {
    pub id: Uuid,
    pub project_id: String,
    pub policy_id: String,
    pub sender_id: String,
    pub status: String,
    pub time: i64,
    pub date: String,
    pub opened: bool,
    pub eta_date: String,
    /// UTC without offset, like Sinch
    pub create_time: String,
    pub update_time: String,
    pub logs: Vec<RegistrationLog>,
    pub attachments: Vec<RegistrationAttachment>,
    pub callback_url: Option<String>,
    pub tags: Vec<String>,
    pub webhooks: Vec<WebhookAttempt>,
    pub request: String,
}

impl Registration {
    /// the registration as returned by the Registrations API
    fn api_json(&self) -> Value {
        json!({
            "id": self.id,
            "projectId": self.project_id,
            "policyId": self.policy_id,
            "status": self.status,
            "etaDate": self.eta_date,
            "createTime": self.create_time,
            "updateTime": self.update_time,
            "tags": self.tags,
            "callbackUrl": self.callback_url,
            "logs": self.logs.iter().map(|l| json!({
                "createTime": l.create_time,
                "message": l.message,
            })).collect::<Vec<_>>(),
        })
    }
}

fn naive_utc_now() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

/// GET /markets/availability
pub async fn markets_availability_handler(headers: HeaderMap) -> Response {
    if let Err(e) = require_auth(&headers) {
        return e;
    }

    Json(json!({
        "markets": MARKETS
            .iter()
            .map(|(code, name)| json!({ "marketCode": code, "marketName": name }))
            .collect::<Vec<_>>()
    }))
    .into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MarketQuery {
    market_code: Option<String>,
}

/// GET /markets/details, GB asks a question before resolving to a policy
pub async fn market_details_handler(
    headers: HeaderMap,
    Query(query): Query<MarketQuery>,
) -> Response {
    if let Err(e) = require_auth(&headers) {
        return e;
    }

    let code = query.market_code.unwrap_or_default().to_ascii_uppercase();
    if !MARKETS.iter().any(|(c, _)| *c == code) {
        return api_error(StatusCode::NOT_FOUND, format!("Unknown market {code}"));
    }

    let lower = code.to_ascii_lowercase();
    let special_conditions = if code == "GB" {
        json!({
            "prompt": "Is the brand registered in the United Kingdom?",
            "description": "Simulated special condition from Pincer",
            "chainedSpecialConditions": [
                { "answer": "Yes", "policyId": format!("{POLICY_PREFIX}{lower}-local") },
                { "answer": "No", "policyId": format!("{POLICY_PREFIX}{lower}-international") },
            ],
        })
    } else {
        json!({ "policyId": format!("{POLICY_PREFIX}{lower}") })
    };

    Json(json!({
        "marketVersions": [
            { "id": format!("{lower}-v0"), "status": "INACTIVE" },
            { "id": format!("{lower}-v1"), "status": "ACTIVE", "specialConditions": special_conditions },
        ]
    }))
    .into_response()
}

fn requires_attachment(policy_id: &str) -> bool {
    policy_id.starts_with(&format!("{POLICY_PREFIX}gb"))
}

/// GET /policies/{policy_id}
pub async fn policy_handler(
    headers: HeaderMap,
    Path((_, policy_id)): Path<(String, String)>,
) -> Response {
    if let Err(e) = require_auth(&headers) {
        return e;
    }
    if !policy_id.starts_with(POLICY_PREFIX) {
        return api_error(StatusCode::NOT_FOUND, format!("Unknown policy {policy_id}"));
    }

    Json(json!({
        "id": policy_id,
        "policyStatus": "ENABLED",
        "statusDisclaimer": "This is a simulated policy served by Pincer",
        "isOpenPolicy": false,
        "jsonPolicy": {
            "senderIdTemplate": { "properties": [
                {
                    "name": "senderId", "title": "Sender ID", "type": "string", "required": true,
                    "minLength": 3, "maxLength": 11, "pattern": "^[A-Za-z0-9 ._-]*[A-Za-z][A-Za-z0-9 ._-]*$",
                    "x-placeholder": "Acme",
                    "messages": { "pattern": "Use 3-11 letters, digits, spaces, dots, dashes or underscores" },
                },
                {
                    "name": "typeOfTraffic", "title": "Type of traffic", "type": "string", "required": true,
                    "enum": ["Transactional", "Marketing", "OTP"],
                },
                {
                    "name": "sampleMessage", "title": "Sample message", "type": "string", "required": false,
                    "maxLength": 160,
                },
                { "name": "authorized", "title": "I am authorized to use this Sender ID", "type": "boolean", "enum": [null, true] },
            ]},
            "companyDetailsTemplate": { "properties": [
                { "name": "companyName", "title": "Company name", "type": "string", "required": true },
                { "name": "companyUrlWebsite", "title": "Company URL/Website", "type": "string" },
                { "name": "companyAddress", "title": "Company address", "type": "string", "required": true },
            ]},
            "contactPersonTemplate": { "properties": [
                { "name": "firstName", "title": "First name", "type": "string", "required": true },
                { "name": "lastName", "title": "Last name", "type": "string", "required": true },
                { "name": "email", "title": "Email", "type": "string", "required": true },
            ]},
        },
        "attachments": [{
            "id": ATTACHMENT_ID,
            "title": "Letter of Authorization",
            "description": "Download, sign and upload the letter of authorization",
            "mandatory": requires_attachment(&policy_id),
            "template": true,
            "allowedMimeTypes": ["application/pdf"],
        }],
        "price": { "subscription": {
            "currency": "EUR", "frequency": 1, "recurrentFeeAmount": "0.00", "setupFeeAmount": "0.00",
        }},
    }))
    .into_response()
}

/// GET /policies/{policy_id}/attachments/{attachment_id}/template
pub async fn attachment_template_handler(
    headers: HeaderMap,
    Path((_, policy_id, attachment_id)): Path<(String, String, String)>,
) -> Response {
    if let Err(e) = require_auth(&headers) {
        return e;
    }
    if !policy_id.starts_with(POLICY_PREFIX) || attachment_id != ATTACHMENT_ID {
        return api_error(StatusCode::NOT_FOUND, "Unknown attachment");
    }

    Response::builder()
        .header(header::CONTENT_TYPE, "application/pdf")
        .header(
            header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{attachment_id}.pdf\""),
        )
        .body(Body::from(TEMPLATE_PDF))
        .unwrap()
}

/// POST /registrations
pub async fn create_registration_handler(
    Path(project_id): Path<String>,
    headers: HeaderMap,
    Extension(state): Extension<Arc<AppState>>,
    body: Bytes,
) -> Response {
    if let Err(e) = require_auth(&headers) {
        return e;
    }
    let request = match parse_json(&body) {
        Ok(v) => v,
        Err(e) => return e,
    };

    let policy_id = request
        .get("policyId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if !policy_id.starts_with(POLICY_PREFIX) {
        return api_error(
            StatusCode::BAD_REQUEST,
            format!("Unknown policy '{policy_id}'"),
        );
    }

    let sender_id = request
        .pointer("/requestDetails/0/senderIdDetails/0/senderId")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    if sender_id.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "senderId is required");
    }

    let (time, date) = now();
    let create_time = naive_utc_now();
    let status = if requires_attachment(&policy_id) {
        "PENDING_ATTACHMENTS"
    } else {
        "IN_QUEUE"
    };
    let registration = Registration {
        id: Uuid::new_v4(),
        project_id,
        sender_id,
        status: status.to_owned(),
        time,
        date,
        opened: false,
        eta_date: (chrono::Utc::now() + chrono::Duration::days(14))
            .format("%Y-%m-%d")
            .to_string(),
        update_time: create_time.clone(),
        logs: vec![RegistrationLog {
            create_time: create_time.clone(),
            message: format!("Registration received for policy {policy_id}"),
        }],
        create_time,
        policy_id,
        attachments: vec![],
        callback_url: request
            .get("callbackUrl")
            .and_then(Value::as_str)
            .filter(|u| !u.is_empty())
            .map(str::to_owned),
        tags: request
            .get("tags")
            .and_then(Value::as_array)
            .map(|t| {
                t.iter()
                    .filter_map(|t| t.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        webhooks: vec![],
        request: serde_json::to_string_pretty(&request).unwrap_or_default(),
    };

    info!(
        "sender ID registration {} received for {}",
        registration.id, registration.sender_id
    );

    let response = registration.api_json();
    if let Ok(mut storage) = state.registrations.write() {
        storage.insert(registration.id, registration.clone());
    }
    let _ = state.events.send(Event::Registration(registration));

    Json(response).into_response()
}

fn find_registration(state: &AppState, id: &str) -> Result<Registration, Response> {
    Uuid::parse_str(id)
        .ok()
        .and_then(|id| state.registrations.read().ok()?.get(&id).cloned())
        .ok_or_else(|| {
            api_error(
                StatusCode::NOT_FOUND,
                format!("Registration {id} not found"),
            )
        })
}

/// GET /registrations/{registration_id}
pub async fn get_registration_handler(
    headers: HeaderMap,
    Path((_, id)): Path<(String, String)>,
    Extension(state): Extension<Arc<AppState>>,
) -> Response {
    if let Err(e) = require_auth(&headers) {
        return e;
    }

    match find_registration(&state, &id) {
        Ok(registration) => Json(registration.api_json()).into_response(),
        Err(e) => e,
    }
}

/// DELETE /registrations/{registration_id}
pub async fn delete_registration_handler(
    headers: HeaderMap,
    Path((_, id)): Path<(String, String)>,
    Extension(state): Extension<Arc<AppState>>,
) -> Response {
    if let Err(e) = require_auth(&headers) {
        return e;
    }
    let registration = match find_registration(&state, &id) {
        Ok(r) => r,
        Err(e) => return e,
    };

    if !["DRAFT", "IN_QUEUE", "PENDING", "PENDING_ATTACHMENTS"]
        .contains(&registration.status.as_str())
    {
        return api_error(
            StatusCode::CONFLICT,
            format!(
                "A registration with status {} can not be deleted",
                registration.status
            ),
        );
    }

    if let Ok(mut storage) = state.registrations.write() {
        storage.remove(&registration.id);
    }
    let _ = state.events.send(Event::Removed(registration.id));

    StatusCode::NO_CONTENT.into_response()
}

/// extract the file name from a multipart body without a full multipart parser
fn multipart_filename(body: &[u8]) -> Option<String> {
    let head = String::from_utf8_lossy(&body[..body.len().min(4096)]);
    let start = head.find("filename=\"")? + "filename=\"".len();
    let end = head[start..].find('"')?;

    Some(head[start..start + end].to_owned())
}

/// POST /registrations/{registration_id}/attachments/{attachment_id}
pub async fn upload_attachment_handler(
    headers: HeaderMap,
    Path((_, id, attachment_id)): Path<(String, String, String)>,
    Extension(state): Extension<Arc<AppState>>,
    body: Bytes,
) -> Response {
    if let Err(e) = require_auth(&headers) {
        return e;
    }
    let registration = match find_registration(&state, &id) {
        Ok(r) => r,
        Err(e) => return e,
    };

    let attachment = RegistrationAttachment {
        filename: multipart_filename(&body),
        size: body.len(),
        attachment_id,
    };
    let message = format!(
        "Attachment {} uploaded ({})",
        attachment.attachment_id,
        attachment.filename.as_deref().unwrap_or("unnamed")
    );

    let mut notification = None;
    if let Ok(mut storage) = state.registrations.write()
        && let Some(r) = storage.get_mut(&registration.id)
    {
        r.attachments.push(attachment);
        r.logs.push(RegistrationLog {
            create_time: naive_utc_now(),
            message,
        });
        // all attachments present, the registration continues like it would at Sinch
        if r.status == "PENDING_ATTACHMENTS" {
            notification = Some(apply_status(r, "IN_QUEUE"));
        }
        let _ = state.events.send(Event::Registration(r.clone()));
    }

    if let Some((callback_url, payload)) = notification {
        let state = state.clone();
        tokio::spawn(async move {
            let _ = notify(&state, registration.id, "IN_QUEUE", callback_url, payload).await;
        });
    }

    Json(json!({})).into_response()
}

/// change the status in place, returns the callback target and payload to notify
fn apply_status(registration: &mut Registration, status: &str) -> (Option<String>, String) {
    let update_time = naive_utc_now();
    registration.status = status.to_owned();
    registration.logs.push(RegistrationLog {
        create_time: update_time.clone(),
        message: format!("Status changed to {status}"),
    });
    registration.update_time = update_time;

    let payload = json!({
        "eventType": "REGISTRATION_STATUS_CHANGE",
        "resourceId": registration.id,
        "status": status,
    });

    (registration.callback_url.clone(), payload.to_string())
}

/// change the status of a registration and notify the application
pub async fn set_status(
    state: &Arc<AppState>,
    id: Uuid,
    status: &str,
) -> Result<Registration, SetStatusError> {
    let (callback_url, payload) = {
        let mut storage = state
            .registrations
            .write()
            .map_err(|_| SetStatusError::NotFound)?;
        let registration = storage.get_mut(&id).ok_or(SetStatusError::NotFound)?;
        if FINAL_REGISTRATION_STATUSES.contains(&registration.status.as_str()) {
            return Err(SetStatusError::AlreadyFinal);
        }
        apply_status(registration, status)
    };

    notify(state, id, status, callback_url, payload).await
}

/// send the status change callback and record it
async fn notify(
    state: &Arc<AppState>,
    id: Uuid,
    status: &str,
    callback_url: Option<String>,
    payload: String,
) -> Result<Registration, SetStatusError> {
    let settings = state.sinch.settings();
    let url = callback_url
        .as_deref()
        .or(settings.registration_webhook_url.as_deref());
    let attempt = webhook::send(
        &state.http,
        url,
        "REGISTRATION_STATUS_CHANGE",
        status,
        payload,
        Signing::Registration(&settings.registration_hmac_secret),
    )
    .await;

    let updated = {
        let mut storage = state
            .registrations
            .write()
            .map_err(|_| SetStatusError::NotFound)?;
        let registration = storage.get_mut(&id).ok_or(SetStatusError::NotFound)?;
        registration.webhooks.push(attempt);
        registration.clone()
    };
    let _ = state.events.send(Event::Registration(updated.clone()));

    Ok(updated)
}
