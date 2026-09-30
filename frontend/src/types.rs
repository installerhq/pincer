use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Clone, PartialEq, Eq, Deserialize, Default)]
pub struct Address {
    pub name: Option<String>,
    pub email: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct AttachmentMetadata {
    pub filename: String,
    pub mime: String,
    pub size: String,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct MailMessageMetadata {
    pub id: String,
    pub from: Address,
    pub to: Vec<Address>,
    pub subject: String,
    pub time: u64,
    pub date: String,
    pub size: String,
    pub opened: bool,
    pub attachments: Vec<AttachmentMetadata>,
    pub envelope_from: String,
    pub envelope_recipients: Vec<String>,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct Attachment {
    pub filename: String,
    pub content_id: Option<String>,
    pub mime: String,
    pub size: String,
}

#[derive(Clone, PartialEq, Eq, Deserialize, Default)]
pub struct MailMessage {
    pub id: String,
    pub from: Address,
    pub to: Vec<Address>,
    pub subject: String,
    pub time: u64,
    pub date: String,
    pub size: String,
    pub opened: bool,
    pub text: String,
    pub html: String,
    pub attachments: Vec<Attachment>,
    pub headers: HashMap<String, String>,
    pub envelope_from: String,
    pub envelope_recipients: Vec<String>,
    #[serde(default)]
    pub parse_warnings: Vec<String>,
}

#[derive(Serialize, Debug)]
pub enum Action {
    RemoveAll,
    Remove(String),
    Open(String),
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct WebhookAttempt {
    pub time: u64,
    pub date: String,
    pub event: String,
    pub status: String,
    pub url: Option<String>,
    pub signed: bool,
    pub payload: String,
    pub response_status: Option<u16>,
    pub response_body: Option<String>,
    pub error: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct SmsMessage {
    pub id: String,
    pub message_id: String,
    pub project_id: String,
    pub app_id: String,
    pub from: String,
    pub to: String,
    pub text: String,
    pub time: u64,
    pub date: String,
    pub opened: bool,
    pub status: String,
    pub outcome: String,
    pub rule: Option<String>,
    pub finalized: bool,
    pub parts: usize,
    pub encoding: String,
    pub max_parts: Option<usize>,
    pub metadata: String,
    pub correlation_id: String,
    pub callback_url: Option<String>,
    pub webhooks: Vec<WebhookAttempt>,
    pub request: String,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct RegistrationLog {
    pub create_time: String,
    pub message: String,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct RegistrationAttachment {
    pub attachment_id: String,
    pub filename: Option<String>,
    pub size: usize,
}

#[derive(Clone, PartialEq, Eq, Deserialize)]
pub struct Registration {
    pub id: String,
    pub project_id: String,
    pub policy_id: String,
    pub sender_id: String,
    pub status: String,
    pub time: u64,
    pub date: String,
    pub opened: bool,
    pub eta_date: String,
    pub create_time: String,
    pub update_time: String,
    pub logs: Vec<RegistrationLog>,
    pub attachments: Vec<RegistrationAttachment>,
    pub callback_url: Option<String>,
    pub tags: Vec<String>,
    pub webhooks: Vec<WebhookAttempt>,
    pub request: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct DlrRule {
    pub suffix: String,
    pub outcome: String,
}

/// runtime settings of the Sinch mock
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SinchSettings {
    pub webhook_url: Option<String>,
    pub webhook_secret: String,
    pub registration_webhook_url: Option<String>,
    pub registration_hmac_secret: String,
    pub dlr_delay_ms: u64,
    pub dlr_status: String,
    pub dlr_rules: Vec<DlrRule>,
}

/// configuration of the Sinch mock
#[derive(Clone, PartialEq, Eq, Deserialize, Default)]
pub struct SinchInfo {
    pub port: u16,
    pub settings: SinchSettings,
    pub outcomes: Vec<String>,
    pub final_statuses: Vec<String>,
    pub registration_statuses: Vec<String>,
    pub final_registration_statuses: Vec<String>,
}

/// events pushed by the backend over the websocket
#[derive(Deserialize)]
pub enum Event {
    Mail(MailMessageMetadata),
    Sms(SmsMessage),
    Registration(Registration),
    Removed(String),
    Settings(SinchInfo),
}

/// an entry in the message list
#[derive(Clone, PartialEq, Eq)]
pub enum Item {
    Mail(MailMessageMetadata),
    Sms(SmsMessage),
    Registration(Registration),
}

impl Item {
    pub fn id(&self) -> &str {
        match self {
            Item::Mail(m) => &m.id,
            Item::Sms(s) => &s.id,
            Item::Registration(r) => &r.id,
        }
    }

    pub fn time(&self) -> u64 {
        match self {
            Item::Mail(m) => m.time,
            Item::Sms(s) => s.time,
            Item::Registration(r) => r.time,
        }
    }

    pub fn opened(&self) -> bool {
        match self {
            Item::Mail(m) => m.opened,
            Item::Sms(s) => s.opened,
            Item::Registration(r) => r.opened,
        }
    }

    pub fn open(&mut self) {
        match self {
            Item::Mail(m) => m.opened = true,
            Item::Sms(s) => s.opened = true,
            Item::Registration(r) => r.opened = true,
        }
    }
}
