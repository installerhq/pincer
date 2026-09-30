use crate::{
    api::{send_registration_status, send_sms_status},
    list::status_class,
    types::{Registration, SinchInfo, SmsMessage, WebhookAttempt},
};
use wasm_bindgen_futures::spawn_local;
use web_sys::MouseEvent;
use yew::{Callback, Html, Properties, function_component, html, use_state};

#[derive(Clone, Copy, PartialEq, Eq)]
enum SinchTab {
    Details,
    Webhooks,
    Request,
}

fn tabs(
    active: SinchTab,
    set: Callback<SinchTab>,
    webhooks: usize,
    remove: Callback<MouseEvent>,
) -> Html {
    let tabs = [
        (SinchTab::Details, "Details".to_owned()),
        (SinchTab::Webhooks, format!("Webhooks ({webhooks})")),
        (SinchTab::Request, "Request".to_owned()),
    ];

    html! {
      <ul class="tabs">
        {tabs.into_iter().map(|(tab, label)| {
            let set = set.clone();
            html! {
              <li>
                <button
                  onclick={move |_| set.emit(tab)}
                  class={if active == tab { "active" } else { "" }}
                >
                  {label}
                </button>
              </li>
            }
        }).collect::<Html>()}
        <li class="delete">
          <button onclick={remove}>
            {"Delete"}
          </button>
        </li>
      </ul>
    }
}

/// buttons to trigger a status change, one per status
fn status_actions(label: &str, statuses: &[String], on: Callback<String>) -> Html {
    html! {
      <div class="actions sinch-actions">
        <span class="label">{label}</span>
        {statuses.iter().map(|status| {
            let on = on.clone();
            let value = status.clone();
            html! {
              <button onclick={move |_| on.emit(value.clone())}>
                {status}
              </button>
            }
        }).collect::<Html>()}
      </div>
    }
}

/// explain how the outcome of an SMS was decided
fn outcome_text(sms: &SmsMessage) -> String {
    let reason = match &sms.rule {
        Some(suffix) => format!("recipient ends in {suffix}"),
        None => "default, SINCH_DLR_STATUS".to_owned(),
    };

    if sms.outcome == "PENDING" {
        format!("PENDING ({reason}), stays queued until a final report is sent below")
    } else {
        format!("{} ({reason})", sms.outcome)
    }
}

fn pretty_json(value: &str) -> String {
    js_sys::JSON::parse(value)
        .ok()
        .and_then(|v| {
            js_sys::JSON::stringify_with_replacer_and_space(
                &v,
                &wasm_bindgen::JsValue::NULL,
                &2.into(),
            )
            .ok()
        })
        .and_then(|s| s.as_string())
        .unwrap_or_else(|| value.to_owned())
}

fn webhook_log(webhooks: &[WebhookAttempt]) -> Html {
    if webhooks.is_empty() {
        return html! { <p class="no-webhooks">{"No webhooks sent yet"}</p> };
    }

    html! {
      <ul class="webhooks">
        {webhooks.iter().rev().map(|attempt| {
            let outcome = match (attempt.response_status, &attempt.error) {
                (Some(code), _) => code.to_string(),
                (None, Some(error)) => error.clone(),
                _ => "?".to_owned(),
            };
            let ok = attempt.response_status.is_some_and(|c| (200..300).contains(&c));

            html! {
              <li>
                <div class="webhook-head">
                  <span class={status_class(&attempt.status)}>{&attempt.status}</span>
                  <span class="event">{&attempt.event}</span>
                  <span class="url">{attempt.url.clone().unwrap_or_default()}</span>
                  <span class={if ok { "outcome ok" } else { "outcome failed" }}>{outcome}</span>
                  if attempt.signed {
                    <span class="signed" title="Signed with the configured secret">{"signed"}</span>
                  } else {
                    <span class="unsigned" title="No secret configured">{"unsigned"}</span>
                  }
                  <span class="date">{&attempt.date}</span>
                </div>
                <pre>{pretty_json(&attempt.payload)}</pre>
                if let Some(body) = &attempt.response_body {
                  <pre class="response">{body}</pre>
                }
              </li>
            }
        }).collect::<Html>()}
      </ul>
    }
}

#[derive(Properties, PartialEq)]
pub struct SmsViewProps {
    pub sms: SmsMessage,
    pub info: SinchInfo,
    pub remove: Callback<MouseEvent>,
}

#[function_component(SmsView)]
pub fn sms_view(props: &SmsViewProps) -> Html {
    let tab = use_state(|| SinchTab::Details);
    let sms = &props.sms;

    let send_status = {
        let id = sms.id.clone();
        Callback::from(move |status: String| {
            let id = id.clone();
            spawn_local(async move { send_sms_status(&id, &status).await });
        })
    };
    let set_tab = {
        let tab = tab.clone();
        Callback::from(move |t| tab.set(t))
    };
    let target = sms
        .callback_url
        .clone()
        .or(props.info.settings.webhook_url.clone())
        .unwrap_or_else(|| "none, set SINCH_WEBHOOK_URL".to_owned());

    html! {
      <div class="view-inner">
        {tabs(*tab, set_tab, sms.webhooks.len(), props.remove.clone())}
        <div class="tab-content">
          if *tab == SinchTab::Details {
            <table>
              <tbody>
                <tr><th>{"To"}</th><td>{&sms.to}</td></tr>
                <tr><th>{"Sender"}</th><td>{&sms.from}</td></tr>
                <tr><th>{"Status"}</th><td><span class={status_class(&sms.status)}>{&sms.status}</span></td></tr>
                <tr><th>{"Outcome"}</th><td>{outcome_text(sms)}</td></tr>
                <tr>
                  <th>{"Parts"}</th>
                  <td>
                    {sms.parts}{" ("}{&sms.encoding}{", "}{sms.text.chars().count()}{" characters"}
                    if let Some(max) = sms.max_parts {
                      {", max "}{max}
                    }
                    {")"}
                  </td>
                </tr>
                <tr><th>{"Message ID"}</th><td><code>{&sms.message_id}</code></td></tr>
                <tr><th>{"App / project"}</th><td><code>{&sms.app_id}</code>{" / "}<code>{&sms.project_id}</code></td></tr>
                if !sms.metadata.is_empty() {
                  <tr><th>{"Metadata"}</th><td><code>{&sms.metadata}</code></td></tr>
                }
                <tr><th>{"Webhook target"}</th><td>{target}</td></tr>
              </tbody>
            </table>
            if !sms.finalized {
              {status_actions("Send final report", &props.info.final_statuses, send_status)}
            }
            <div class="sms-body">
              <div class="bubble">{&sms.text}</div>
            </div>
          } else if *tab == SinchTab::Webhooks {
            {webhook_log(&sms.webhooks)}
          } else {
            <pre>{&sms.request}</pre>
          }
        </div>
      </div>
    }
}

#[derive(Properties, PartialEq)]
pub struct RegistrationViewProps {
    pub registration: Registration,
    pub info: SinchInfo,
    pub remove: Callback<MouseEvent>,
}

#[function_component(RegistrationView)]
pub fn registration_view(props: &RegistrationViewProps) -> Html {
    let tab = use_state(|| SinchTab::Details);
    let registration = &props.registration;

    let set_status = {
        let id = registration.id.clone();
        Callback::from(move |status: String| {
            let id = id.clone();
            spawn_local(async move { send_registration_status(&id, &status).await });
        })
    };
    let set_tab = {
        let tab = tab.clone();
        Callback::from(move |t| tab.set(t))
    };
    let target = registration
        .callback_url
        .clone()
        .or(props.info.settings.registration_webhook_url.clone())
        .unwrap_or_else(|| "none, set SINCH_REGISTRATION_WEBHOOK_URL".to_owned());

    html! {
      <div class="view-inner">
        {tabs(*tab, set_tab, registration.webhooks.len(), props.remove.clone())}
        <div class="tab-content">
          if *tab == SinchTab::Details {
            <table>
              <tbody>
                <tr><th>{"Sender ID"}</th><td>{&registration.sender_id}</td></tr>
                <tr><th>{"Status"}</th><td><span class={status_class(&registration.status)}>{&registration.status}</span></td></tr>
                <tr><th>{"Policy"}</th><td><code>{&registration.policy_id}</code></td></tr>
                <tr><th>{"Registration ID"}</th><td><code>{&registration.id}</code></td></tr>
                <tr><th>{"ETA"}</th><td>{&registration.eta_date}</td></tr>
                if !registration.tags.is_empty() {
                  <tr><th>{"Tags"}</th><td>{registration.tags.join(", ")}</td></tr>
                }
                <tr>
                  <th>{"Attachments"}</th>
                  <td>
                    if registration.attachments.is_empty() {
                      {"none"}
                    }
                    {registration.attachments.iter().map(|a| html! {
                      <div>
                        {a.filename.clone().unwrap_or_else(|| a.attachment_id.clone())}
                        <span class="size">{" ("}{&a.attachment_id}{", "}{a.size}{" bytes)"}</span>
                      </div>
                    }).collect::<Html>()}
                  </td>
                </tr>
                <tr><th>{"Webhook target"}</th><td>{target}</td></tr>
              </tbody>
            </table>
            if !props.info.final_registration_statuses.contains(&registration.status) {
              {status_actions(
                  "Set status",
                  &props
                      .info
                      .registration_statuses
                      .iter()
                      .filter(|s| **s != registration.status)
                      .cloned()
                      .collect::<Vec<_>>(),
                  set_status,
              )}
            }
            <table class="logs">
              <tbody>
                {registration.logs.iter().rev().map(|log| html! {
                  <tr><th>{&log.create_time}</th><td>{&log.message}</td></tr>
                }).collect::<Html>()}
              </tbody>
            </table>
          } else if *tab == SinchTab::Webhooks {
            {webhook_log(&registration.webhooks)}
          } else {
            <pre>{&registration.request}</pre>
          }
        </div>
      </div>
    }
}
