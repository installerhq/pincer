use futures::{StreamExt, channel::mpsc::Sender};
use gloo_console::error;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{HtmlInputElement, InputEvent, NotificationOptions};
use yew::prelude::*;

use crate::{
    api::{fetch_messages_metadata, fetch_registrations, fetch_sinch_info, fetch_sms},
    dark_mode::{init_dark_mode, toggle_dark_mode},
    list::MessageList,
    settings::SettingsPanel,
    sinch_view::{RegistrationView, SmsView},
    types::{Action, Address, Event, Item, SinchInfo},
    view::ViewMessage,
    websocket::WebsocketService,
};

pub enum Msg {
    Select(String),
    SetTab(Tab),
    Event(Box<Event>),
    Messages(Vec<Item>),
    SinchInfo(SinchInfo),
    Remove(String),
    Loading(bool),
    ShowSettings(bool),
    RemoveAll,
    Search(String),
}

#[derive(Clone, PartialEq, Eq)]
pub enum Tab {
    Formatted,
    Text,
    Headers,
    Raw,
}

pub struct Overview {
    selected: String,
    tab: Tab,
    messages: Vec<Item>,
    sinch_info: SinchInfo,
    sender: Sender<Action>,
    loading: bool,
    show_settings: bool,
    search_query: String,
}

impl Component for Overview {
    type Message = Msg;
    type Properties = ();

    fn create(ctx: &Context<Self>) -> Self {
        let link = ctx.link().clone();
        link.send_message(Msg::Loading(true));
        spawn_local(async move {
            let (mail, sms, registrations) = futures::join!(
                fetch_messages_metadata(),
                fetch_sms(),
                fetch_registrations()
            );
            let mut messages: Vec<Item> = mail
                .into_iter()
                .map(Item::Mail)
                .chain(sms.into_iter().map(Item::Sms))
                .chain(registrations.into_iter().map(Item::Registration))
                .collect();
            messages.sort_by_key(Item::time);
            link.send_message(Msg::Messages(messages));
            link.send_message(Msg::Loading(false));
        });

        let link = ctx.link().clone();
        spawn_local(async move {
            if let Some(info) = fetch_sinch_info().await {
                link.send_message(Msg::SinchInfo(info));
            }
        });

        let mut wss = WebsocketService::new();

        let link = ctx.link().clone();
        spawn_local(async move {
            while let Some(message) = wss.receiver.next().await {
                link.send_message(Msg::Event(Box::new(message)));
            }
        });

        spawn_local(async {
            init_dark_mode();
        });

        web_sys::Notification::request_permission().ok();

        Self {
            messages: vec![],
            sinch_info: Default::default(),
            tab: Tab::Formatted,
            selected: Default::default(),
            sender: wss.sender,
            loading: true,
            show_settings: false,
            search_query: String::new(),
        }
    }

    fn update(&mut self, _ctx: &Context<Self>, msg: Self::Message) -> bool {
        match msg {
            Msg::Loading(value) => {
                self.loading = value;
            }
            Msg::Event(event) => {
                let item = match *event {
                    Event::Mail(message) => Item::Mail(message),
                    Event::Sms(sms) => Item::Sms(sms),
                    Event::Registration(registration) => Item::Registration(registration),
                    Event::Removed(id) => {
                        self.messages.retain(|m| m.id() != id);
                        return true;
                    }
                    Event::Settings(info) => {
                        self.sinch_info = info;
                        return true;
                    }
                };

                if let Some(existing) = self.messages.iter_mut().find(|m| m.id() == item.id()) {
                    // updates of SMS and registrations replace the entry, keeping local state
                    let opened = existing.opened();
                    *existing = item;
                    if opened {
                        existing.open();
                    }
                } else {
                    notify(&item);
                    self.messages.push(item);
                    self.messages.sort_by_key(Item::time);
                }
            }
            Msg::SinchInfo(info) => {
                self.sinch_info = info;
            }
            Msg::ShowSettings(show) => {
                self.show_settings = show;
            }
            Msg::Messages(messages) => {
                // keep what arrived over the websocket while loading, it is newer
                let live = std::mem::take(&mut self.messages);
                let mut merged: Vec<Item> = messages
                    .into_iter()
                    .filter(|m| !live.iter().any(|l| l.id() == m.id()))
                    .collect();
                merged.extend(live);
                merged.sort_by_key(Item::time);
                self.messages = merged;
            }
            Msg::Select(id) => {
                self.selected = id.clone();

                let unopened = self
                    .messages
                    .iter_mut()
                    .find(|m| m.id() == self.selected && !m.opened());

                if let Some(unopened_message) = unopened {
                    if self.sender.try_send(Action::Open(id)).is_err() {
                        error!("Error registering message as opened");
                    }

                    unopened_message.open();
                }
            }
            Msg::SetTab(tab) => {
                self.tab = tab;
            }
            Msg::Remove(id) => {
                self.messages.retain(|m| m.id() != id);

                if self.sender.try_send(Action::Remove(id)).is_err() {
                    error!("Error removing email");
                }
            }
            Msg::RemoveAll => {
                if self.sender.try_send(Action::RemoveAll).is_ok() {
                    self.messages.clear();
                }
            }
            Msg::Search(query) => {
                self.search_query = query;
            }
        };

        true
    }

    fn rendered(&mut self, _ctx: &Context<Self>, _first_render: bool) {
        let count = self.messages.iter().filter(|m| !m.opened()).count();
        gloo_utils::document().set_title(&format!("Pincer ({count})"));
    }

    fn view(&self, ctx: &Context<Self>) -> Html {
        let link = ctx.link();
        let filtered_messages = self.filtered_messages();
        let selected_message = self.messages.iter().find(|m| m.id() == self.selected);
        let selected_id = self.selected.clone();

        html! {
          <>
            <header>
              <h1>{"Pin"}<span>{"cer"}</span></h1>
              <div>
                <input
                  type="search"
                  placeholder="Search mail and SMS…"
                  aria-label="Search mail and SMS"
                  class="search"
                  oninput={link.callback(|e: InputEvent| {
                    let query = e
                        .target()
                        .and_then(|t| t.dyn_into::<HtmlInputElement>().ok())
                        .map(|el| el.value())
                        .unwrap_or_default();
                    Msg::Search(query)
                  })}
                />
                if !self.messages.is_empty() {
                  <button onclick={link.callback(|_| Msg::RemoveAll)}>
                    {"Remove all"}<span>{"("}{self.messages.len()}{")"}</span>
                  </button>
                }
                if self.sinch_info.port > 0 {
                  <button class="settings-button" title="SMS settings" onclick={link.callback(|_| Msg::ShowSettings(true))}>
                    {"Settings"}
                  </button>
                }
                <button class="dark-mode" title="Toggle dark mode" onclick={Callback::from(|_| {
                    toggle_dark_mode();
                })} />
              </div>
            </header>
            if self.show_settings {
              <SettingsPanel
                info={self.sinch_info.clone()}
                close={link.callback(|_| Msg::ShowSettings(false))}
                saved={link.batch_callback(|info| vec![Msg::SinchInfo(info), Msg::ShowSettings(false)])}
              />
            }
            if self.messages.is_empty() {
              <div class="empty">
                if self.loading {
                    <div class="bouncing-loader">
                        <div></div>
                        <div></div>
                        <div></div>
                    </div>
                } else {
                    { "The inbox is empty 📭" }
                    if self.sinch_info.port > 0 {
                        <p class="hint">
                            {"Send email over SMTP, or SMS through the Sinch API on port "}
                            {self.sinch_info.port}
                        </p>
                        if !self.sinch_info.settings.dlr_rules.is_empty() {
                            <p class="hint">
                                {"SMS to numbers ending in "}
                                {self.sinch_info.settings.dlr_rules.iter().map(|rule| format!("{} \u{2192} {}", rule.suffix, rule.outcome)).collect::<Vec<_>>().join(", ")}
                                {", other numbers \u{2192} "}{&self.sinch_info.settings.dlr_status}
                            </p>
                        }
                    }
                }
              </div>
            } else {
              <div class="main">
                <div class="list">
                    if filtered_messages.is_empty() {
                        <div class="no-results" role="status">
                            {"No messages match your search"}
                        </div>
                    } else {
                        <ul>
                            <MessageList
                                messages={filtered_messages}
                                selected={self.selected.clone()}
                                select={link.callback(Msg::Select)}
                            />
                        </ul>
                    }
                </div>
                <div class="view">
                    {match selected_message {
                        Some(Item::Mail(message)) => html! {
                            <ViewMessage
                                message={message.clone()}
                                set_tab={link.callback(Msg::SetTab)}
                                remove={link.callback(move |_| Msg::Remove(selected_id.clone()))}
                                active_tab={self.tab.clone()}
                            />
                        },
                        Some(Item::Sms(sms)) => html! {
                            <SmsView
                                key={sms.id.clone()}
                                sms={sms.clone()}
                                info={self.sinch_info.clone()}
                                remove={link.callback(move |_| Msg::Remove(selected_id.clone()))}
                            />
                        },
                        Some(Item::Registration(registration)) => html! {
                            <RegistrationView
                                key={registration.id.clone()}
                                registration={registration.clone()}
                                info={self.sinch_info.clone()}
                                remove={link.callback(move |_| Msg::Remove(selected_id.clone()))}
                            />
                        },
                        None => html! {},
                    }}
                </div>
            </div>
            }
          </>
        }
    }
}

impl Overview {
    fn filtered_messages(&self) -> Vec<Item> {
        let query = self.search_query.trim().to_lowercase();
        if query.is_empty() {
            return self.messages.clone();
        }

        let address_matches = |address: &Address| {
            [address.name.as_deref(), address.email.as_deref()]
                .into_iter()
                .flatten()
                .any(|value| value.to_lowercase().contains(&query))
        };

        let matches = |value: &str| value.to_lowercase().contains(&query);

        self.messages
            .iter()
            .filter(|item| match item {
                Item::Mail(m) => {
                    address_matches(&m.from)
                        || m.to.iter().any(address_matches)
                        || matches(&m.subject)
                        || matches(&m.envelope_from)
                        || m.envelope_recipients.iter().any(|r| matches(r))
                }
                Item::Sms(s) => {
                    matches(&s.to)
                        || matches(&s.from)
                        || matches(&s.text)
                        || matches(&s.status)
                        || matches(&s.message_id)
                        || matches(&s.metadata)
                }
                Item::Registration(r) => {
                    matches(&r.sender_id)
                        || matches(&r.status)
                        || matches(&r.policy_id)
                        || matches(&r.id)
                        || r.tags.iter().any(|t| matches(t))
                }
            })
            .cloned()
            .collect()
    }
}

/// show a browser notification for a new message
fn notify(item: &Item) {
    let (title, body) = match item {
        Item::Mail(message) => (
            format!(
                "Pincer: {} <{}>",
                message.from.name.clone().unwrap_or_default(),
                message.from.email.clone().unwrap_or_default()
            ),
            message.subject.clone(),
        ),
        Item::Sms(sms) => (format!("Pincer SMS: {}", sms.to), sms.text.clone()),
        Item::Registration(registration) => (
            format!("Pincer sender ID: {}", registration.sender_id),
            format!("Registration {}", registration.status),
        ),
    };

    let notif_options = NotificationOptions::default();
    notif_options.set_body(&body);
    let _ = web_sys::Notification::new_with_options(&title, &notif_options);
}
