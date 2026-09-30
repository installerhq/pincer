use crate::types::{Item, MailMessageMetadata, Registration, SmsMessage};
use js_sys::Date;
use timeago::Formatter;
use yew::{Callback, Html, Properties, function_component, html, html_nested, use_state};
use yew_hooks::use_interval;

#[derive(Properties, PartialEq)]
pub struct MessageListProps {
    pub messages: Vec<Item>,
    pub selected: String,
    pub select: Callback<String>,
}

fn get_now() -> u64 {
    (Date::now() / 1000.0) as u64
}

/// css class for a Sinch status, e.g. `status-delivered`
pub fn status_class(status: &str) -> String {
    format!("status status-{}", status.to_lowercase().replace('_', "-"))
}

fn mail_item(message: &MailMessageMetadata, date: String) -> Html {
    html! {
      <>
        <span class="head">
          <span class="from">
            <span class="name">
              {message.from.clone().name.unwrap_or_default()}
            </span>
            <span class="email">
              {&message.from.clone().email.unwrap_or_default()}
            </span>
          </span>
          <span class="date" title={message.date.clone()}>
            {date}
          </span>
        </span>
        <span class="preview">
          <span class="subject">
            {&message.subject}
          </span>
          <span class="size">
            {&message.size}
          </span>
        </span>
          <span class="recipients">
            <span class="label">
              if message.envelope_recipients.len() > 1 {
                {"Recipients: "}
              } else {
                {"Recipient: "}
              }
            </span>
            {for message.envelope_recipients.clone().into_iter().take(2).map(|addr| html_nested! {
              <span class="email">{addr}</span>
            })}
            if message.envelope_recipients.len() > 2 {
              <span class="etc">{", \u{2026}"}</span>
            }
          </span>
      </>
    }
}

fn sms_item(sms: &SmsMessage, date: String) -> Html {
    html! {
      <>
        <span class="head">
          <span class="from">
            <span class="kind">{"SMS"}</span>
            <span class="name">{&sms.to}</span>
          </span>
          <span class="date" title={sms.date.clone()}>
            {date}
          </span>
        </span>
        <span class="preview">
          <span class="subject text">
            {&sms.text}
          </span>
          <span class={status_class(&sms.status)}>
            {&sms.status}
          </span>
        </span>
        <span class="recipients">
          <span class="label">{"Sender: "}</span>
          <span class="sender">{&sms.from}</span>
        </span>
      </>
    }
}

fn registration_item(registration: &Registration, date: String) -> Html {
    html! {
      <>
        <span class="head">
          <span class="from">
            <span class="kind">{"Sender ID"}</span>
            <span class="name">{&registration.sender_id}</span>
          </span>
          <span class="date" title={registration.date.clone()}>
            {date}
          </span>
        </span>
        <span class="preview">
          <span class="subject">
            {"Registration for policy "}{&registration.policy_id}
          </span>
          <span class={status_class(&registration.status)}>
            {&registration.status}
          </span>
        </span>
      </>
    }
}

#[function_component(MessageList)]
pub fn list(props: &MessageListProps) -> Html {
    let formatter = Formatter::new();
    let now = use_state(get_now);

    {
        let now = now.clone();
        use_interval(
            move || {
                now.set(get_now());
            },
            10 * 1000,
        );
    }

    props
        .messages
        .iter()
        .map(|item| {
            let id = item.id().to_owned();
            let select = props.select.clone();
            let onclick = { Callback::from(move |_| select.emit(id.clone())) };

            let mut classes = vec![match item {
                Item::Mail(_) => "mail",
                Item::Sms(_) => "sms",
                Item::Registration(_) => "registration",
            }];

            if props.selected == item.id() {
                classes.push("selected");
            } else if item.opened() {
                classes.push("opened")
            }

            if let Item::Mail(message) = item
                && !message.attachments.is_empty()
            {
                classes.push("attachments");
            }

            let ago = if item.time() > *now {
                std::time::Duration::from_secs(0)
            } else {
                std::time::Duration::from_secs(*now - item.time())
            };
            let date = formatter.convert(ago);

            html! {
              <li
                key={item.id().to_owned()}
                tabIndex="0"
                onclick={onclick}
                class={classes.join(" ")}
              >
                {match item {
                    Item::Mail(message) => mail_item(message, date),
                    Item::Sms(sms) => sms_item(sms, date),
                    Item::Registration(registration) => registration_item(registration, date),
                }}
              </li>
            }
        })
        .collect()
}
