use base64::{Engine, engine::general_purpose::STANDARD};
use hmac::{Hmac, KeyInit, Mac};
use serde::Serialize;
use sha1::Sha1;
use sha2::Sha256;
use tokio::time::Duration;
use tracing::{info, warn};
use uuid::Uuid;

use super::now;

/// a single callback sent (or attempted) by the Sinch mock
#[derive(Clone, Debug, Serialize)]
pub struct WebhookAttempt {
    pub time: i64,
    pub date: String,
    /// callback trigger, e.g. MESSAGE_DELIVERY or REGISTRATION_STATUS_CHANGE
    pub event: String,
    /// status reported by the callback
    pub status: String,
    pub url: Option<String>,
    pub signed: bool,
    pub payload: String,
    pub response_status: Option<u16>,
    pub response_body: Option<String>,
    pub error: Option<String>,
}

/// Conversation API signature: base64(HMAC-SHA256(secret, body.nonce.timestamp))
pub fn conversation_signature(secret: &str, body: &str, nonce: &str, timestamp: &str) -> String {
    let mut mac =
        Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key size");
    mac.update(format!("{body}.{nonce}.{timestamp}").as_bytes());

    STANDARD.encode(mac.finalize().into_bytes())
}

/// Registration API signature: hex(HMAC-SHA1(secret, body))
pub fn registration_signature(secret: &str, body: &str) -> String {
    let mut mac =
        Hmac::<Sha1>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key size");
    mac.update(body.as_bytes());

    hex::encode(mac.finalize().into_bytes())
}

pub enum Signing<'a> {
    Conversation(&'a str),
    Registration(&'a str),
}

/// POST a callback and record the outcome
pub async fn send(
    client: &reqwest::Client,
    url: Option<&str>,
    event: &str,
    status: &str,
    payload: String,
    signing: Signing<'_>,
) -> WebhookAttempt {
    let (time, date) = now();
    let mut attempt = WebhookAttempt {
        time,
        date,
        event: event.to_owned(),
        status: status.to_owned(),
        url: url.map(str::to_owned),
        signed: false,
        payload: payload.clone(),
        response_status: None,
        response_body: None,
        error: None,
    };

    let Some(url) = url else {
        attempt.error = Some("No webhook target configured".to_owned());
        return attempt;
    };

    let mut request = client
        .post(url)
        .timeout(Duration::from_secs(10))
        .header(reqwest::header::CONTENT_TYPE, "application/json");

    match signing {
        Signing::Conversation(secret) if !secret.is_empty() => {
            let nonce = Uuid::new_v4().to_string();
            let timestamp = time.to_string();
            let signature = conversation_signature(secret, &payload, &nonce, &timestamp);
            request = request
                .header("x-sinch-webhook-signature-timestamp", timestamp)
                .header("x-sinch-webhook-signature-nonce", nonce)
                .header("x-sinch-webhook-signature-algorithm", "HmacSHA256")
                .header("x-sinch-webhook-signature", signature);
            attempt.signed = true;
        }
        Signing::Registration(secret) if !secret.is_empty() => {
            request = request.header(
                "X-Sinch-Signature",
                registration_signature(secret, &payload),
            );
            attempt.signed = true;
        }
        _ => {}
    }

    match request.body(payload).send().await {
        Ok(response) => {
            let code = response.status();
            attempt.response_status = Some(code.as_u16());
            let body = response.text().await.unwrap_or_default();
            if !body.is_empty() {
                attempt.response_body = Some(body.chars().take(2000).collect());
            }
            if code.is_success() {
                info!("{event} {status} webhook to {url} returned {code}");
            } else {
                warn!("{event} {status} webhook to {url} returned {code}");
            }
        }
        Err(e) => {
            warn!("{event} {status} webhook to {url} failed: {e}");
            attempt.error = Some(e.to_string());
        }
    }

    attempt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signatures() {
        // reference values computed with node:crypto, as used by the Sinch SDK
        assert_eq!(
            conversation_signature("secret", r#"{"a":1}"#, "nonce", "1700000000"),
            "l7MuOHkZAUomVOoNg53fJ3wAQgeY3yOJ/UDpMOAXJP0="
        );
        assert_eq!(
            registration_signature("secret", r#"{"a":1}"#),
            "f8446672f033e4b2beafc5ca3a71eafcd2cafb6e"
        );
    }
}
