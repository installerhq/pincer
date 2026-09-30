//! A mock of the Sinch APIs used for SMS: the OAuth2 token endpoint, the
//! Conversation API `messages:send` endpoint and the Sender ID Registrations
//! API. Messages end up in the Pincer UI next to email, and delivery reports
//! are sent back to the application as signed Sinch webhooks.

// handlers return early with a ready made error response
#![allow(clippy::result_large_err)]

use axum::{
    Extension, Form, Router,
    body::Bytes,
    extract::DefaultBodyLimit,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{delete, get, post},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use mailcrab::{Error, Result as AppResult};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::{Arc, RwLock},
};
use tokio::{net::TcpListener, time::Duration};
use tokio_util::sync::CancellationToken;
use tower_http::trace::{DefaultMakeSpan, TraceLayer};
use tracing::{info, warn};

use crate::{AppState, parse_env_var};

pub mod conversation;
pub mod registration;
pub mod webhook;

pub use conversation::SmsMessage;
pub use registration::Registration;

/// final Conversation API delivery statuses, no reports follow these
pub const FINAL_STATUSES: [&str; 3] = ["DELIVERED", "READ", "FAILED"];
/// outcome of a message that is never finalized automatically
pub const PENDING: &str = "PENDING";
/// default rules, matched against the last digits of the recipient
const DEFAULT_DLR_RULES: &str = "0001=FAILED,0002=PENDING";
/// longest delay between automatic delivery reports
const MAX_DLR_DELAY_MS: u64 = 10 * 60 * 1000;

fn parse_outcome(value: &str) -> Option<String> {
    let value = value.trim().to_ascii_uppercase();

    if FINAL_STATUSES.contains(&value.as_str()) || value == PENDING {
        Some(value)
    } else {
        None
    }
}

/// decides the outcome of messages to recipients ending in `suffix`
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DlrRule {
    pub suffix: String,
    pub outcome: String,
}

/// parse `suffix=OUTCOME` pairs from the environment, skipping invalid entries
fn parse_rules(rules: &str) -> Vec<DlrRule> {
    let mut parsed: Vec<DlrRule> = Vec::new();
    for rule in rules
        .split(',')
        .filter(|rule| !rule.trim().is_empty())
        .filter_map(|rule| {
            let (suffix, outcome) = rule.split_once('=')?;
            let suffix: String = suffix.chars().filter(char::is_ascii_digit).collect();
            match (suffix.is_empty(), parse_outcome(outcome)) {
                (false, Some(outcome)) => Some(DlrRule { suffix, outcome }),
                _ => {
                    warn!("ignoring invalid SINCH_DLR_RULES entry '{rule}'");
                    None
                }
            }
        })
    {
        if parsed.iter().any(|r| r.suffix == rule.suffix) {
            warn!(
                "ignoring duplicate SINCH_DLR_RULES entry for {}",
                rule.suffix
            );
        } else {
            parsed.push(rule);
        }
    }
    // the most specific rule wins
    parsed.sort_by_key(|rule| std::cmp::Reverse(rule.suffix.len()));

    parsed
}

/// normalize a webhook URL, empty means none
fn check_url(name: &str, url: Option<String>) -> Result<Option<String>, String> {
    let Some(url) = url.map(|u| u.trim().to_owned()).filter(|u| !u.is_empty()) else {
        return Ok(None);
    };
    let parsed = reqwest::Url::parse(&url).map_err(|e| format!("{name} {url}: {e}"))?;
    if !["http", "https"].contains(&parsed.scheme()) {
        return Err(format!("{name} must start with http:// or https://"));
    }

    Ok(Some(url))
}

/// an invalid URL from the environment is dropped, instead of failing every webhook
fn env_url(name: &str) -> Option<String> {
    check_url(name, non_empty_env(name)).unwrap_or_else(|e| {
        warn!("{e}, ignoring {name}");
        None
    })
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.trim().is_empty())
}

/// settings of the Sinch mock that can be changed at runtime from the web interface,
/// the environment provides the initial values
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SinchSettings {
    /// default target for Conversation API callbacks, a per message `callback_url` takes precedence
    pub webhook_url: Option<String>,
    /// secret used to sign Conversation API callbacks, callbacks are unsigned when empty
    pub webhook_secret: String,
    /// default target for sender registration callbacks
    pub registration_webhook_url: Option<String>,
    /// secret used to sign sender registration callbacks (hex HMAC-SHA1)
    pub registration_hmac_secret: String,
    /// delay between accepting a message and each automatic delivery report
    pub dlr_delay_ms: u64,
    /// outcome for recipients without a matching rule: a final status or PENDING
    pub dlr_status: String,
    /// outcomes by recipient suffix, longest suffix first
    pub dlr_rules: Vec<DlrRule>,
}

impl SinchSettings {
    pub fn from_env() -> Self {
        let dlr_status = std::env::var("SINCH_DLR_STATUS").unwrap_or_else(|_| "DELIVERED".into());
        let dlr_status = match dlr_status.to_ascii_uppercase().as_str() {
            // NONE was accepted before PENDING existed
            "NONE" => PENDING.to_owned(),
            other => parse_outcome(other).unwrap_or_else(|| {
                warn!("unknown SINCH_DLR_STATUS {other}, using DELIVERED");
                "DELIVERED".to_owned()
            }),
        };

        let settings = SinchSettings {
            webhook_url: env_url("SINCH_WEBHOOK_URL"),
            webhook_secret: std::env::var("SINCH_WEBHOOK_SECRET").unwrap_or_default(),
            registration_webhook_url: env_url("SINCH_REGISTRATION_WEBHOOK_URL"),
            registration_hmac_secret: std::env::var("SINCH_REGISTRATION_HMAC_SECRET")
                .unwrap_or_default(),
            dlr_delay_ms: parse_env_var("SINCH_DLR_DELAY_MS", 1000u64).min(MAX_DLR_DELAY_MS),
            dlr_status,
            dlr_rules: parse_rules(
                &std::env::var("SINCH_DLR_RULES").unwrap_or_else(|_| DEFAULT_DLR_RULES.to_owned()),
            ),
        };

        settings.clone().validate().unwrap_or_else(|e| {
            warn!("invalid Sinch mock configuration: {e}");
            settings
        })
    }

    /// normalize and check settings, e.g. as submitted from the web interface
    pub fn validate(mut self) -> Result<Self, String> {
        self.webhook_url = check_url("Webhook URL", self.webhook_url)?;
        self.registration_webhook_url =
            check_url("Registration webhook URL", self.registration_webhook_url)?;

        self.dlr_status = parse_outcome(&self.dlr_status)
            .ok_or_else(|| format!("Unknown default outcome {}", self.dlr_status))?;
        if self.dlr_delay_ms > MAX_DLR_DELAY_MS {
            return Err(format!("The delay can be at most {MAX_DLR_DELAY_MS} ms"));
        }

        let mut suffixes = std::collections::HashSet::new();
        for rule in &mut self.dlr_rules {
            rule.suffix = rule.suffix.chars().filter(|c| !c.is_whitespace()).collect();
            rule.suffix = rule.suffix.trim_start_matches('+').to_owned();
            if rule.suffix.is_empty() || !rule.suffix.chars().all(|c| c.is_ascii_digit()) {
                return Err(format!("Rule '{}' must contain digits only", rule.suffix));
            }
            rule.outcome = parse_outcome(&rule.outcome).ok_or_else(|| {
                format!("Unknown outcome {} for rule {}", rule.outcome, rule.suffix)
            })?;
            if !suffixes.insert(rule.suffix.clone()) {
                return Err(format!("Rule {} is listed twice", rule.suffix));
            }
        }
        // the most specific rule wins
        self.dlr_rules
            .sort_by_key(|rule| std::cmp::Reverse(rule.suffix.len()));

        Ok(self)
    }

    pub fn dlr_delay(&self) -> Duration {
        Duration::from_millis(self.dlr_delay_ms)
    }

    /// the outcome for a recipient and the rule that decided it
    pub fn outcome(&self, recipient: &str) -> (String, Option<String>) {
        let digits: String = recipient.chars().filter(char::is_ascii_digit).collect();

        match self
            .dlr_rules
            .iter()
            .find(|rule| digits.ends_with(rule.suffix.as_str()))
        {
            Some(rule) => (rule.outcome.clone(), Some(rule.suffix.clone())),
            None => (self.dlr_status.clone(), None),
        }
    }
}

/// configuration of the Sinch mock
#[derive(Debug)]
pub struct SinchConfig {
    pub host: IpAddr,
    pub port: u16,
    settings: RwLock<SinchSettings>,
}

impl SinchConfig {
    pub fn from_env(default_host: IpAddr) -> Self {
        SinchConfig {
            host: parse_env_var("SINCH_HOST", default_host),
            port: parse_env_var("SINCH_PORT", 1090),
            settings: RwLock::new(SinchSettings::from_env()),
        }
    }

    /// a snapshot of the current settings
    pub fn settings(&self) -> SinchSettings {
        match self.settings.read() {
            Ok(settings) => settings.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }

    pub fn set_settings(&self, settings: SinchSettings) {
        match self.settings.write() {
            Ok(mut current) => *current = settings,
            Err(poisoned) => *poisoned.into_inner() = settings,
        }
    }

    /// configuration and possible values, shown and edited in the web interface
    pub fn info(&self) -> SinchInfo {
        SinchInfo {
            port: self.port,
            settings: self.settings(),
            outcomes: vec!["DELIVERED", "READ", "FAILED", PENDING],
            // READ is not reported for SMS, only offer what a phone network reports
            final_statuses: vec!["DELIVERED", "FAILED"],
            registration_statuses: registration::REGISTRATION_STATUSES.to_vec(),
            final_registration_statuses: registration::FINAL_REGISTRATION_STATUSES.to_vec(),
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct SinchInfo {
    pub port: u16,
    pub settings: SinchSettings,
    pub outcomes: Vec<&'static str>,
    pub final_statuses: Vec<&'static str>,
    pub registration_statuses: Vec<&'static str>,
    pub final_registration_statuses: Vec<&'static str>,
}

/// current time as unix timestamp and RFC 3339 string with millisecond precision
pub(crate) fn now() -> (i64, String) {
    let now = chrono::Utc::now();

    (
        now.timestamp(),
        now.to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
    )
}

/// Google style error body, as returned by the Conversation and Registration APIs
pub(crate) fn api_error(code: StatusCode, message: impl Into<String>) -> Response {
    let status = match code {
        StatusCode::BAD_REQUEST => "INVALID_ARGUMENT",
        StatusCode::UNAUTHORIZED => "UNAUTHENTICATED",
        StatusCode::FORBIDDEN => "PERMISSION_DENIED",
        StatusCode::NOT_FOUND => "NOT_FOUND",
        StatusCode::CONFLICT => "FAILED_PRECONDITION",
        _ => "INTERNAL",
    };

    (
        code,
        axum::Json(json!({
            "error": {
                "code": code.as_u16(),
                "message": message.into(),
                "status": status,
                "details": [],
            }
        })),
    )
        .into_response()
}

/// the mock accepts any credentials, but a request without credentials is
/// most likely a misconfiguration, so reject it like Sinch would
pub(crate) fn require_auth(headers: &HeaderMap) -> Result<(), Response> {
    let authorized = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            let v = v.to_ascii_lowercase();
            v.starts_with("bearer ") || v.starts_with("basic ")
        });

    if authorized {
        Ok(())
    } else {
        Err(api_error(
            StatusCode::UNAUTHORIZED,
            "Request had invalid authentication credentials",
        ))
    }
}

pub(crate) fn parse_json(body: &Bytes) -> Result<serde_json::Value, Response> {
    serde_json::from_slice(body).map_err(|e| {
        api_error(
            StatusCode::BAD_REQUEST,
            format!("Invalid JSON payload: {e}"),
        )
    })
}

/// OAuth2 client credentials token endpoint (auth.sinch.com/oauth2/token)
async fn token_handler(headers: HeaderMap, Form(form): Form<HashMap<String, String>>) -> Response {
    let basic = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("basic"))
        .map(|(_, credentials)| credentials);

    let Some(basic) = basic else {
        return (
            StatusCode::UNAUTHORIZED,
            axum::Json(json!({"error": "invalid_client"})),
        )
            .into_response();
    };

    if form.get("grant_type").map(String::as_str) != Some("client_credentials") {
        return (
            StatusCode::BAD_REQUEST,
            axum::Json(json!({"error": "unsupported_grant_type"})),
        )
            .into_response();
    }

    let key_id = base64::engine::general_purpose::STANDARD
        .decode(basic.trim())
        .ok()
        .and_then(|d| String::from_utf8(d).ok())
        .and_then(|d| d.split(':').next().map(str::to_owned))
        .unwrap_or_default();

    // the SDK decodes the token as JWT to read the expiry
    let (now, _) = now();
    let expires_in = 3599;
    let jwt_header = URL_SAFE_NO_PAD.encode(r#"{"alg":"none","typ":"JWT"}"#);
    let claims = URL_SAFE_NO_PAD.encode(
        json!({"iss": "pincer", "sub": key_id, "iat": now, "exp": now + expires_in}).to_string(),
    );
    let signature = URL_SAFE_NO_PAD.encode("pincer");

    axum::Json(json!({
        "access_token": format!("{jwt_header}.{claims}.{signature}"),
        "expires_in": expires_in,
        "scope": "",
        "token_type": "bearer",
    }))
    .into_response()
}

async fn fallback_handler(method: axum::http::Method, uri: axum::http::Uri) -> Response {
    warn!("Sinch mock does not implement {method} {uri}");

    api_error(
        StatusCode::NOT_FOUND,
        format!("Pincer does not mock {method} {}", uri.path()),
    )
}

pub async fn sinch_server(app_state: Arc<AppState>, token: CancellationToken) -> AppResult<()> {
    use conversation::send_handler;
    use registration::*;

    let project = "/v1/projects/{project_id}";
    let app = Router::new()
        .route("/oauth2/token", post(token_handler))
        .route(&format!("{project}/messages:send"), post(send_handler))
        .route(
            &format!("{project}/markets/availability"),
            get(markets_availability_handler),
        )
        .route(
            &format!("{project}/markets/details"),
            get(market_details_handler),
        )
        .route(
            &format!("{project}/policies/{{policy_id}}"),
            get(policy_handler),
        )
        .route(
            &format!("{project}/policies/{{policy_id}}/attachments/{{attachment_id}}/template"),
            get(attachment_template_handler),
        )
        .route(
            &format!("{project}/registrations"),
            post(create_registration_handler),
        )
        .route(
            &format!("{project}/registrations/{{registration_id}}"),
            get(get_registration_handler).merge(delete(delete_registration_handler)),
        )
        .route(
            &format!("{project}/registrations/{{registration_id}}/attachments/{{attachment_id}}"),
            post(upload_attachment_handler),
        )
        .fallback(fallback_handler)
        // registration attachments easily exceed the default 2 MB limit
        .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::default().include_headers(true)),
        )
        .layer(Extension(app_state.clone()));

    let config = &app_state.sinch;
    let addr = SocketAddr::from((config.host, config.port));
    let listener = TcpListener::bind(&addr).await?;

    info!(
        "Sinch mock ready on {addr}, delivery reports to {}",
        config
            .settings()
            .webhook_url
            .as_deref()
            .unwrap_or("(per message callback_url only)")
    );

    axum::serve(listener, app)
        .with_graceful_shutdown(Box::leak(Box::new(token)).cancelled())
        .await
        .map_err(|e| Error::WebServer(e.to_string()))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(rules: &str) -> SinchSettings {
        SinchSettings {
            webhook_url: None,
            webhook_secret: String::new(),
            registration_webhook_url: None,
            registration_hmac_secret: String::new(),
            dlr_delay_ms: 1000,
            dlr_status: "DELIVERED".to_owned(),
            dlr_rules: parse_rules(rules),
        }
        .validate()
        .unwrap()
    }

    #[test]
    fn dlr_rules() {
        let config = settings(DEFAULT_DLR_RULES);
        assert_eq!(
            config.outcome("+4791230001"),
            ("FAILED".into(), Some("0001".into()))
        );
        assert_eq!(
            config.outcome("+47 912 30 002"),
            ("PENDING".into(), Some("0002".into()))
        );
        assert_eq!(config.outcome("+4791234567"), ("DELIVERED".into(), None));

        // rules from the environment are sorted and deduplicated as well
        assert_eq!(
            parse_rules("1=FAILED,0001=PENDING,1=READ")[0].suffix,
            "0001"
        );
        assert_eq!(parse_rules("1=FAILED,0001=PENDING,1=READ").len(), 2);

        // the most specific rule wins, invalid entries are skipped
        let config = settings("1=FAILED, +4791230001=read,x=FAILED,2=bogus");
        assert_eq!(config.dlr_rules.len(), 2);
        assert_eq!(
            config.outcome("+4791230001"),
            ("READ".into(), Some("4791230001".into()))
        );
        assert_eq!(
            config.outcome("+4791239991"),
            ("FAILED".into(), Some("1".into()))
        );
    }

    #[test]
    fn validate_settings() {
        let valid = settings("");
        let with = |f: fn(&mut SinchSettings)| {
            let mut s = valid.clone();
            f(&mut s);
            s.validate()
        };

        let normalized = with(|s| {
            s.webhook_url = Some("  ".into());
            s.dlr_status = "failed".into();
            s.dlr_rules = vec![DlrRule {
                suffix: "+47 0001".into(),
                outcome: "pending".into(),
            }];
        })
        .unwrap();
        assert_eq!(normalized.webhook_url, None);
        assert_eq!(normalized.dlr_status, "FAILED");
        assert_eq!(
            normalized.dlr_rules[0],
            DlrRule {
                suffix: "470001".into(),
                outcome: "PENDING".into()
            }
        );

        assert!(with(|s| s.webhook_url = Some("localhost:8000/x".into())).is_err());
        assert!(with(|s| s.registration_webhook_url = Some("ftp://x".into())).is_err());
        assert!(with(|s| s.dlr_status = "QUEUED_ON_CHANNEL".into()).is_err());
        assert!(with(|s| s.dlr_delay_ms = MAX_DLR_DELAY_MS + 1).is_err());
        assert!(
            with(|s| s.dlr_rules = vec![DlrRule {
                suffix: "12a".into(),
                outcome: "FAILED".into()
            }])
            .is_err()
        );
        assert!(
            with(|s| s.dlr_rules = vec![
                DlrRule {
                    suffix: "1".into(),
                    outcome: "FAILED".into()
                },
                DlrRule {
                    suffix: "1".into(),
                    outcome: "READ".into()
                },
            ])
            .is_err()
        );
    }
}
