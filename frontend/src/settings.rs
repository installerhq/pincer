use crate::{
    api::save_sinch_settings,
    types::{DlrRule, SinchInfo, SinchSettings},
};
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{Event, HtmlInputElement, HtmlSelectElement, InputEvent};
use yew::{Callback, Html, Properties, UseStateHandle, function_component, html, use_state};

#[derive(Properties, PartialEq)]
pub struct SettingsPanelProps {
    pub info: SinchInfo,
    pub close: Callback<()>,
    pub saved: Callback<SinchInfo>,
}

fn input_value(e: &InputEvent) -> String {
    e.target()
        .and_then(|t| t.dyn_into::<HtmlInputElement>().ok())
        .map(|el| el.value())
        .unwrap_or_default()
}

fn select_value(e: &Event) -> String {
    e.target()
        .and_then(|t| t.dyn_into::<HtmlSelectElement>().ok())
        .map(|el| el.value())
        .unwrap_or_default()
}

/// update a copy of the draft settings
fn update<F: Fn(&mut SinchSettings, String) + 'static>(
    draft: &UseStateHandle<SinchSettings>,
    change: F,
) -> impl Fn(String) + use<F> {
    let draft = draft.clone();
    move |value| {
        let mut settings = (*draft).clone();
        change(&mut settings, value);
        draft.set(settings);
    }
}

fn outcome_select(outcomes: &[String], selected: &str, on: impl Fn(String) + 'static) -> Html {
    html! {
      <select onchange={move |e: Event| on(select_value(&e))}>
        {outcomes.iter().map(|o| html! {
          <option value={o.clone()} selected={o == selected}>{o}</option>
        }).collect::<Html>()}
      </select>
    }
}

fn text_input(value: &str, placeholder: &str, secret: bool, on: impl Fn(String) + 'static) -> Html {
    html! {
      <input
        type={if secret { "password" } else { "text" }}
        value={value.to_owned()}
        placeholder={placeholder.to_owned()}
        oninput={move |e: InputEvent| on(input_value(&e))}
      />
    }
}

/// panel to change the Sinch mock settings at runtime
#[function_component(SettingsPanel)]
pub fn settings_panel(props: &SettingsPanelProps) -> Html {
    let draft = use_state(|| props.info.settings.clone());
    let error: UseStateHandle<Option<String>> = use_state(|| None);
    let saving = use_state(|| false);
    // rows are keyed by generation and index, so removing a rule rebuilds the
    // rows instead of reusing inputs and selects that hold stale values
    let generation = use_state(|| 0u32);
    let outcomes = &props.info.outcomes;

    let save = {
        let draft = draft.clone();
        let error = error.clone();
        let saving = saving.clone();
        let saved = props.saved.clone();
        move |_| {
            let settings = (*draft).clone();
            let error = error.clone();
            let saving = saving.clone();
            let saved = saved.clone();
            error.set(None);
            saving.set(true);
            spawn_local(async move {
                match save_sinch_settings(&settings).await {
                    Ok(info) => saved.emit(info),
                    Err(e) => error.set(Some(e)),
                }
                saving.set(false);
            });
        }
    };
    let close = {
        let close = props.close.clone();
        move |_| close.emit(())
    };
    let add_rule = {
        let draft = draft.clone();
        let generation = generation.clone();
        move |_| {
            generation.set(*generation + 1);
            let mut settings = (*draft).clone();
            settings.dlr_rules.push(DlrRule {
                suffix: String::new(),
                outcome: "FAILED".to_owned(),
            });
            draft.set(settings);
        }
    };

    html! {
      <div class="settings-backdrop" onclick={close.clone()}>
        <div class="settings" onclick={|e: web_sys::MouseEvent| e.stop_propagation()}>
          <h2>{"SMS settings"}</h2>
          <p class="note">
            {"Changes apply to SMS sent from now on and last until Pincer restarts, the environment variables provide the defaults."}
          </p>

          <h3>{"Delivery reports"}</h3>
          <label>
            <span>{"Default outcome"}</span>
            {outcome_select(outcomes, &draft.dlr_status, update(&draft, |s, v| s.dlr_status = v))}
          </label>
          <label>
            <span>{"Delay between reports (ms)"}</span>
            <input
              type="number"
              min="0"
              step="100"
              value={draft.dlr_delay_ms.to_string()}
              oninput={{
                  let set = update(&draft, |s, v| s.dlr_delay_ms = v.parse().unwrap_or(0));
                  move |e: InputEvent| set(input_value(&e))
              }}
            />
          </label>

          <div class="rules">
            <span class="label">{"Rules, by the last digits of the recipient"}</span>
            {draft.dlr_rules.iter().enumerate().map(|(index, rule)| html! {
              <div class="rule" key={format!("{}-{index}", *generation)}>
                {text_input(&rule.suffix, "e.g. 0001", false, update(&draft, move |s, v| s.dlr_rules[index].suffix = v))}
                {outcome_select(outcomes, &rule.outcome, update(&draft, move |s, v| s.dlr_rules[index].outcome = v))}
                <button class="remove" title="Remove rule" onclick={{
                    let remove = update(&draft, move |s, _| { s.dlr_rules.remove(index); });
                    let generation = generation.clone();
                    move |_| {
                        generation.set(*generation + 1);
                        remove(String::new())
                    }
                }}>{"\u{2715}"}</button>
              </div>
            }).collect::<Html>()}
            <button class="add" onclick={add_rule}>{"Add rule"}</button>
          </div>

          <h3>{"Webhooks"}</h3>
          <label>
            <span>{"Delivery report URL"}</span>
            {text_input(
                draft.webhook_url.as_deref().unwrap_or_default(),
                "http://host.docker.internal:3000/webhooks/sinch/status",
                false,
                update(&draft, |s, v| s.webhook_url = Some(v)),
            )}
          </label>
          <label>
            <span>{"Delivery report secret"}</span>
            {text_input(&draft.webhook_secret, "unsigned when empty", true, update(&draft, |s, v| s.webhook_secret = v))}
          </label>
          <label>
            <span>{"Registration URL"}</span>
            {text_input(
                draft.registration_webhook_url.as_deref().unwrap_or_default(),
                "http://host.docker.internal:3000/webhooks/sinch/registrations",
                false,
                update(&draft, |s, v| s.registration_webhook_url = Some(v)),
            )}
          </label>
          <label>
            <span>{"Registration secret"}</span>
            {text_input(&draft.registration_hmac_secret, "unsigned when empty", true, update(&draft, |s, v| s.registration_hmac_secret = v))}
          </label>

          if let Some(e) = &*error {
            <p class="error" role="alert">{e}</p>
          }
          <div class="buttons">
            <button onclick={close}>{"Cancel"}</button>
            <button class="primary" disabled={*saving} onclick={save}>{"Save"}</button>
          </div>
        </div>
      </div>
    }
}
