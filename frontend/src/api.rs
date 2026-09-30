use gloo_net::http::Request;

use crate::types::{
    MailMessage, MailMessageMetadata, Registration, SinchInfo, SinchSettings, SmsMessage,
};

pub fn get_api_path(path: &str) -> String {
    let mut pathname = web_sys::window()
        .and_then(|w| w.location().pathname().ok())
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_string();

    pathname.push_str("/api/");
    pathname.push_str(path);

    pathname
}

pub async fn fetch_messages_metadata() -> Vec<MailMessageMetadata> {
    let mut messages: Vec<MailMessageMetadata> = Request::get(&get_api_path("messages"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();

    messages.sort_by_key(|a| a.time);

    messages
}

pub async fn fetch_message(id: &str) -> MailMessage {
    let mut url = get_api_path("message/");
    url.push_str(id);

    Request::get(&url)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

pub async fn fetch_raw(id: &str) -> String {
    let url = get_api_path(&format!("message/{}/raw", id));

    let response = match Request::get(&url).send().await {
        Ok(r) => r,
        Err(e) => return format!("Failed to load raw message: {e}"),
    };

    response
        .text()
        .await
        .unwrap_or_else(|e| format!("Failed to read raw message: {e}"))
}

pub async fn fetch_sms() -> Vec<SmsMessage> {
    match Request::get(&get_api_path("sms")).send().await {
        Ok(response) => response.json().await.unwrap_or_default(),
        Err(_) => vec![],
    }
}

pub async fn fetch_registrations() -> Vec<Registration> {
    match Request::get(&get_api_path("registrations")).send().await {
        Ok(response) => response.json().await.unwrap_or_default(),
        Err(_) => vec![],
    }
}

pub async fn fetch_sinch_info() -> Option<SinchInfo> {
    Request::get(&get_api_path("sinch"))
        .send()
        .await
        .ok()?
        .json()
        .await
        .ok()
}

/// post a status change, the update itself arrives over the websocket
async fn post_status(url: &str) {
    match Request::post(url).send().await {
        Ok(response) if !response.ok() => {
            gloo_console::error!(format!("{url} failed with {}", response.status()));
        }
        Err(e) => gloo_console::error!(format!("{url} failed: {e}")),
        _ => {}
    }
}

/// ask the Sinch mock to send the final delivery report of a pending SMS
pub async fn send_sms_status(id: &str, status: &str) {
    post_status(&get_api_path(&format!("sms/{id}/status/{status}"))).await;
}

/// change a sender ID registration status
pub async fn send_registration_status(id: &str, status: &str) {
    post_status(&get_api_path(&format!("registration/{id}/status/{status}"))).await;
}

/// save the Sinch mock settings, returns the validation error on failure
pub async fn save_sinch_settings(settings: &SinchSettings) -> Result<SinchInfo, String> {
    let response = Request::put(&get_api_path("sinch/settings"))
        .json(settings)
        .map_err(|e| e.to_string())?
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if response.ok() {
        response.json().await.map_err(|e| e.to_string())
    } else {
        Err(response
            .text()
            .await
            .unwrap_or_else(|_| format!("Saving failed with {}", response.status())))
    }
}
