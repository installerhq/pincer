use axum::{
    Extension, Json,
    body::Bytes,
    extract::Path,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::Arc;
use tracing::info;
use uuid::Uuid;

use super::{
    PENDING, api_error, now, parse_json, require_auth,
    webhook::{self, Signing, WebhookAttempt},
};
use crate::{AppState, Event};

/// an SMS sent through the Conversation API
#[derive(Clone, Debug, Serialize)]
pub struct SmsMessage {
    /// internal id, shared with mail messages for the UI actions
    pub id: Uuid,
    /// Sinch message id (ULID)
    pub message_id: String,
    pub project_id: String,
    pub app_id: String,
    /// sender as requested in the SMS_SENDER channel property
    pub from: String,
    /// recipient phone number (or contact id)
    pub to: String,
    pub text: String,
    pub time: i64,
    pub date: String,
    pub opened: bool,
    /// latest delivery status, ACCEPTED until the first delivery report
    pub status: String,
    /// the planned final status (or PENDING), decided by the recipient number
    pub outcome: String,
    /// the recipient suffix rule that decided the outcome, `None` for the default
    pub rule: Option<String>,
    /// the QUEUED_ON_CHANNEL report was claimed
    #[serde(skip)]
    pub queued: bool,
    /// a final report was claimed, no further reports are sent
    pub finalized: bool,
    pub parts: usize,
    pub encoding: String,
    pub max_parts: Option<usize>,
    /// echoed back in delivery reports as `message_delivery_report.metadata`
    pub metadata: String,
    pub correlation_id: String,
    pub callback_url: Option<String>,
    pub webhooks: Vec<WebhookAttempt>,
    /// the request body as received
    pub request: String,
}

/// the GSM 03.38 basic character set, characters that fit in one septet
const GSM_BASIC: &str = "@£$¥èéùìòÇ\nØø\rÅåΔ_ΦΓΛΩΠΨΣΘΞÆæßÉ !\"#¤%&'()*+,-./0123456789:;<=>?¡ABCDEFGHIJKLMNOPQRSTUVWXYZÄÖÑÜ§¿abcdefghijklmnopqrstuvwxyzäöñüà";
/// the GSM 03.38 extension table, characters that take two septets
const GSM_EXTENDED: &str = "^{}\\[~]|€\x0c";

/// number of SMS parts and the encoding used for a text
pub fn segments(text: &str) -> (usize, &'static str) {
    let septets = text.chars().try_fold(0usize, |acc, c| {
        if GSM_BASIC.contains(c) {
            Some(acc + 1)
        } else if GSM_EXTENDED.contains(c) {
            Some(acc + 2)
        } else {
            None
        }
    });

    match septets {
        Some(n) if n <= 160 => (1, "GSM"),
        Some(n) => (n.div_ceil(153), "GSM"),
        None => {
            let units = text.encode_utf16().count();
            if units <= 70 {
                (1, "UCS2")
            } else {
                (units.div_ceil(67), "UCS2")
            }
        }
    }
}

fn str_at<'a>(value: &'a Value, pointer: &str) -> Option<&'a str> {
    value.pointer(pointer).and_then(Value::as_str)
}

/// POST /v1/projects/{project_id}/messages:send
pub async fn send_handler(
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

    let app_id = str_at(&request, "/app_id").unwrap_or_default();
    if app_id.is_empty() {
        return api_error(StatusCode::BAD_REQUEST, "app_id is required");
    }

    let identities = request
        .pointer("/recipient/identified_by/channel_identities")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let to = identities
        .iter()
        .find(|i| str_at(i, "/channel") == Some("SMS"))
        .or(identities.first())
        .and_then(|i| str_at(i, "/identity"))
        .or_else(|| str_at(&request, "/recipient/contact_id"));
    let Some(to) = to.map(str::to_owned) else {
        return api_error(
            StatusCode::BAD_REQUEST,
            "recipient must contain contact_id or identified_by.channel_identities",
        );
    };

    let Some(message) = request.get("message").and_then(Value::as_object) else {
        return api_error(StatusCode::BAD_REQUEST, "message is required");
    };
    let text = match str_at(&request, "/message/text_message/text") {
        Some(text) => text.to_owned(),
        None => format!(
            "[{} is not rendered by Pincer]\n{}",
            message
                .keys()
                .next()
                .map(String::as_str)
                .unwrap_or("message"),
            serde_json::to_string_pretty(message).unwrap_or_default()
        ),
    };

    let metadata = str_at(&request, "/message_metadata")
        .unwrap_or_default()
        .to_owned();
    if metadata.chars().count() > 1024 {
        return api_error(
            StatusCode::BAD_REQUEST,
            "message_metadata must not exceed 1024 characters",
        );
    }

    let (parts, encoding) = segments(&text);
    let (outcome, rule) = state.sinch.settings().outcome(&to);
    let (time, date) = now();
    let sms = SmsMessage {
        id: Uuid::new_v4(),
        message_id: ulid::Ulid::generate().to_string(),
        project_id,
        app_id: app_id.to_owned(),
        from: str_at(&request, "/channel_properties/SMS_SENDER")
            .unwrap_or_default()
            .to_owned(),
        to,
        text,
        time,
        date: date.clone(),
        opened: false,
        status: "ACCEPTED".to_owned(),
        outcome,
        rule,
        queued: false,
        finalized: false,
        parts,
        encoding: encoding.to_owned(),
        max_parts: str_at(
            &request,
            "/channel_properties/SMS_MAX_NUMBER_OF_MESSAGE_PARTS",
        )
        .and_then(|v| v.parse().ok()),
        metadata,
        correlation_id: str_at(&request, "/correlation_id")
            .unwrap_or_default()
            .to_owned(),
        callback_url: str_at(&request, "/callback_url")
            .filter(|u| !u.is_empty())
            .map(str::to_owned),
        webhooks: vec![],
        request: serde_json::to_string_pretty(&request).unwrap_or_default(),
    };

    info!("SMS {} accepted for {}", sms.message_id, sms.to);

    let response = json!({ "message_id": sms.message_id, "accepted_time": date });
    let id = sms.id;

    if let Ok(mut storage) = state.sms.write() {
        storage.insert(id, sms.clone());
    }
    let _ = state.events.send(Event::Sms(sms));

    let state = state.clone();
    tokio::spawn(async move {
        tokio::time::sleep(state.sinch.settings().dlr_delay()).await;
        if !queue(&state, id).await {
            return;
        }

        let outcome = match state.sms.read() {
            Ok(storage) => storage.get(&id).map(|sms| sms.outcome.clone()),
            Err(_) => None,
        };
        if let Some(outcome) = outcome.filter(|o| o != PENDING) {
            tokio::time::sleep(state.sinch.settings().dlr_delay()).await;
            // an error means the report was already sent from the UI
            let _ = finalize(&state, id, &outcome).await;
        }
    });

    Json(response).into_response()
}

/// the MESSAGE_DELIVERY callback payload for a status
fn delivery_report(sms: &SmsMessage, status: &str) -> Value {
    let (_, event_time) = now();
    let mut report = json!({
        "message_id": sms.message_id,
        "conversation_id": "",
        "contact_id": "",
        "status": status,
        "channel_identity": { "channel": "SMS", "identity": sms.to, "app_id": "" },
        "metadata": sms.metadata,
        "processing_mode": "DISPATCH",
    });

    match status {
        "FAILED" => {
            report["reason"] = json!({
                "code": "RECIPIENT_NOT_REACHABLE",
                "description": "Simulated failure from Pincer",
                "channel_code": "406",
                "sub_code": "UNSPECIFIED_SUB_CODE",
            })
        }
        "SWITCHING_CHANNEL" => {
            report["reason"] = json!({
                "code": "CHANNEL_FAILURE",
                "description": "Simulated channel switch from Pincer",
                "sub_code": "UNSPECIFIED_SUB_CODE",
            })
        }
        _ => {}
    }

    json!({
        "app_id": sms.app_id,
        "project_id": sms.project_id,
        "accepted_time": sms.date,
        "event_time": event_time,
        "correlation_id": sms.correlation_id,
        "message_delivery_report": report,
        // like Sinch, the part count is only reported when the message carried a limit
        "message_metadata": match sms.max_parts {
            Some(_) => json!({ "number_of_message_parts": sms.parts }).to_string(),
            None => "{}".to_owned(),
        },
    })
}

/// atomically claim a report, so automatic and manual reports never overlap
fn claim(state: &AppState, id: Uuid, claim: impl FnOnce(&mut SmsMessage) -> bool) -> bool {
    state
        .sms
        .write()
        .is_ok_and(|mut storage| storage.get_mut(&id).is_some_and(claim))
}

/// send the QUEUED_ON_CHANNEL report once, returns false when it was already sent
/// or the message is finalized or gone
async fn queue(state: &Arc<AppState>, id: Uuid) -> bool {
    let claimed = claim(state, id, |sms| {
        let first = !sms.queued && !sms.finalized;
        if first {
            sms.queued = true;
        }
        first
    });

    claimed && deliver(state, id, "QUEUED_ON_CHANNEL").await.is_some()
}

#[derive(Debug, PartialEq, Eq)]
pub enum FinalizeError {
    NotFound,
    AlreadyFinal,
}

/// send the one final report of a message, preceded by QUEUED_ON_CHANNEL if that
/// was not sent yet
pub async fn finalize(
    state: &Arc<AppState>,
    id: Uuid,
    status: &str,
) -> Result<SmsMessage, FinalizeError> {
    // claim the final report and QUEUED_ON_CHANNEL together, so an automatic
    // report can not slip in between
    let claimed = {
        let mut storage = state.sms.write().map_err(|_| FinalizeError::NotFound)?;
        let sms = storage.get_mut(&id).ok_or(FinalizeError::NotFound)?;
        if sms.finalized {
            return Err(FinalizeError::AlreadyFinal);
        }
        sms.finalized = true;
        let needs_queue = !std::mem::replace(&mut sms.queued, true);
        (needs_queue, sms.clone())
    };
    let (needs_queue, snapshot) = claimed;
    // the UI hides the actions right away, the reports can take a while
    let _ = state.events.send(Event::Sms(snapshot));

    // a final report always follows QUEUED_ON_CHANNEL, like at Sinch
    if needs_queue {
        deliver(state, id, "QUEUED_ON_CHANNEL").await;
    } else {
        // the automatic QUEUED_ON_CHANNEL report is in flight, keep the order
        for _ in 0..240 {
            let queued = state
                .sms
                .read()
                .is_ok_and(|s| s.get(&id).is_none_or(|sms| sms.status != "ACCEPTED"));
            if queued {
                break;
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(50)).await;
        }
    }

    deliver(state, id, status)
        .await
        .ok_or(FinalizeError::NotFound)
}

/// send a delivery report for a stored SMS, returns the updated message
async fn deliver(state: &Arc<AppState>, id: Uuid, status: &str) -> Option<SmsMessage> {
    let sms = state.sms.read().ok()?.get(&id)?.clone();
    let payload = delivery_report(&sms, status).to_string();
    let settings = state.sinch.settings();
    let url = sms
        .callback_url
        .as_deref()
        .or(settings.webhook_url.as_deref());

    let attempt = webhook::send(
        &state.http,
        url,
        "MESSAGE_DELIVERY",
        status,
        payload,
        Signing::Conversation(&settings.webhook_secret),
    )
    .await;

    let updated = {
        let mut storage = state.sms.write().ok()?;
        // the message might have been deleted in the meantime
        let sms = storage.get_mut(&id)?;
        sms.status = status.to_owned();
        sms.webhooks.push(attempt);
        sms.clone()
    };
    let _ = state.events.send(Event::Sms(updated.clone()));

    Some(updated)
}

#[cfg(test)]
mod tests {
    use super::segments;

    #[test]
    fn segment_count() {
        assert_eq!(segments("Grüße, vi ses på fredag!"), (1, "GSM"));
        assert_eq!(segments(&"a".repeat(160)), (1, "GSM"));
        assert_eq!(segments(&"a".repeat(161)), (2, "GSM"));
        assert_eq!(segments(&"€".repeat(80)), (1, "GSM"));
        assert_eq!(segments(&"€".repeat(81)), (2, "GSM"));
        assert_eq!(segments("Hello 👋"), (1, "UCS2"));
        assert_eq!(segments(&"ł".repeat(71)), (2, "UCS2"));
    }
}
