use axum::{
    Extension, Json, Router,
    body::Body,
    extract::{
        Path, WebSocketUpgrade,
        ws::{self, WebSocket},
    },
    http::{StatusCode, Uri, header},
    response::{Html, IntoResponse, Response},
    routing::{get, post, put},
};
use mailcrab::{Action, Error, MailMessage, MailMessageMetadata, Result as AppResult};
use serde::Serialize;
use std::{
    ffi::OsStr,
    net::{IpAddr, SocketAddr},
    sync::Arc,
};
use tokio::{net::TcpListener, sync::broadcast::error::RecvError};
use tokio_util::sync::CancellationToken;
use tower_http::trace::{DefaultMakeSpan, TraceLayer};
use tracing::{error, info, warn};
use uuid::Uuid;

use crate::{
    AppState, Asset, Event, VERSION,
    sinch::{
        FINAL_STATUSES, Registration, SinchInfo, SinchSettings, SmsMessage,
        conversation::{FinalizeError, finalize},
        registration::{REGISTRATION_STATUSES, SetStatusError, set_status},
    },
};

#[derive(Debug, Serialize)]
struct VersionInfo {
    version_be: String,
}

/// serialize an event and send it to a websocket client, returns false when the client is gone
async fn send_event(socket: &mut WebSocket, event: &Event) -> bool {
    match serde_json::to_string(event) {
        Ok(json) => {
            if socket.send(ws::Message::Text(json.into())).await.is_err() {
                info!("WS client disconnected");
                return false;
            }
        }
        Err(e) => {
            error!("could not convert event to json {:?}", e);
        }
    }

    true
}

/// handle actions from the UI, these apply to mail, SMS and registrations alike
fn handle_action(state: &AppState, action: Action) {
    match action {
        Action::RemoveAll => {
            if let Ok(mut storage) = state.storage.write() {
                storage.clear();
            }
            if let Ok(mut sms) = state.sms.write() {
                sms.clear();
            }
            if let Ok(mut registrations) = state.registrations.write() {
                registrations.clear();
            }
            info!("storage cleared");
        }
        Action::Open(id) => {
            if let Ok(mut storage) = state.storage.write()
                && let Some(message) = storage.get_mut(&id)
            {
                message.open();
            } else if let Ok(mut sms) = state.sms.write()
                && let Some(sms) = sms.get_mut(&id)
            {
                sms.opened = true;
            } else if let Ok(mut registrations) = state.registrations.write()
                && let Some(registration) = registrations.get_mut(&id)
            {
                registration.opened = true;
            }
            info!("message {} opened", &id);
        }
        Action::Remove(id) => {
            remove(state, id);
        }
    }
}

/// remove an email, SMS or registration, returns false when nothing was found
fn remove(state: &AppState, id: Uuid) -> bool {
    let removed = state
        .storage
        .write()
        .is_ok_and(|mut s| s.remove(&id).is_some())
        || state.sms.write().is_ok_and(|mut s| s.remove(&id).is_some())
        || state
            .registrations
            .write()
            .is_ok_and(|mut s| s.remove(&id).is_some());
    if removed {
        info!("message {} removed", &id);
    }

    removed
}

/// send mail, SMS and registration events to websocket clients
async fn ws_handler(
    ws: WebSocketUpgrade,
    Extension(state): Extension<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(|mut socket: WebSocket| async move {
        let mut receive = state.rx.resubscribe();
        let mut events = state.events.subscribe();
        let mut active = true;
        let mut ping_interval = tokio::time::interval(tokio::time::Duration::from_secs(30));

        while active {
            tokio::select! {
                _ = ping_interval.tick() => {
                    if socket.send(ws::Message::Ping(Default::default())).await.is_err() {
                        info!("WS client disconnected");
                        active = false;
                    }
                },
                internal_received = receive.recv() => {
                    match internal_received {
                        Ok(message) => {
                            active = send_event(&mut socket, &Event::Mail(message.into())).await;
                        },
                        Err(RecvError::Lagged(skipped)) => {
                            warn!("websocket client lagging, skipped {skipped} messages");
                        },
                        Err(e) => {
                            error!("event pipeline error {:?}", e);
                            active = false;
                        }
                    }
                },
                event = events.recv() => {
                    match event {
                        Ok(event) => {
                            active = send_event(&mut socket, &event).await;
                        },
                        Err(RecvError::Lagged(skipped)) => {
                            warn!("websocket client lagging, skipped {skipped} events");
                        },
                        Err(e) => {
                            error!("event pipeline error {:?}", e);
                            active = false;
                        }
                    }
                },
                socket_received = socket.recv() => {
                    match socket_received {
                        Some(Ok(ws::Message::Text(action))) => {
                            match serde_json::from_str(action.as_str()) {
                                Ok(action) => handle_action(&state, action),
                                Err(e) => warn!("unknown action {:?}", e),
                            }
                        },
                        Some(Ok(ws::Message::Pong(_))) => {
                            // pass
                        },
                        Some(Ok(ws::Message::Close(_))) | None => {
                            info!("websocket closed");
                            active = false;
                        },
                        Some(Err(e)) => {
                            warn!("websocket error {:?}", e);
                            active = false;
                        },
                        Some(Ok(other_message)) => {
                            info!("received unexpected message {:?}", other_message);
                        },
                    }
                }
            }
        }
    })
}

/// return metadata of all currently stored messages
async fn messages_handler(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<Vec<MailMessageMetadata>>, StatusCode> {
    if let Ok(storage) = state.storage.read() {
        let mut messages = storage
            .values()
            .map(|message| message.clone().into())
            .collect::<Vec<MailMessageMetadata>>();

        messages.sort_by_key(|m| m.time);

        Ok(Json(messages))
    } else {
        Err(StatusCode::INTERNAL_SERVER_ERROR)
    }
}

/// return full message with attachments
async fn message_handler(
    Path(id): Path<Uuid>,
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<MailMessage>, StatusCode> {
    if let Ok(storage) = state.storage.read() {
        match storage.get(&id) {
            Some(message) => Ok(Json(message.clone())),
            _ => Err(StatusCode::NOT_FOUND),
        }
    } else {
        Err(StatusCode::INTERNAL_SERVER_ERROR)
    }
}

/// return message body (html/text)
async fn message_body_handler(
    Path(id): Path<Uuid>,
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Html<String>, StatusCode> {
    if let Ok(storage) = state.storage.read() {
        match storage.get(&id) {
            Some(message) => Ok(Html(message.render(&state.prefix))),
            _ => Err(StatusCode::NOT_FOUND),
        }
    } else {
        Err(StatusCode::INTERNAL_SERVER_ERROR)
    }
}

/// delete a message
async fn message_delete_handler(
    Path(id): Path<Uuid>,
    Extension(state): Extension<Arc<AppState>>,
) -> StatusCode {
    if remove(&state, id) {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    }
}

/// delete all messages
async fn message_delete_all_handler(Extension(state): Extension<Arc<AppState>>) -> StatusCode {
    handle_action(&state, Action::RemoveAll);

    StatusCode::OK
}

/// return all SMS messages
async fn sms_list_handler(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<Vec<SmsMessage>>, StatusCode> {
    let storage = state
        .sms
        .read()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut messages = storage.values().cloned().collect::<Vec<_>>();
    messages.sort_by_key(|m| m.time);

    Ok(Json(messages))
}

/// return all sender ID registrations
async fn registrations_handler(
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<Vec<Registration>>, StatusCode> {
    let storage = state
        .registrations
        .read()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let mut registrations = storage.values().cloned().collect::<Vec<_>>();
    registrations.sort_by_key(|r| r.time);

    Ok(Json(registrations))
}

/// send the final delivery report for a pending SMS, only one final report is ever sent
async fn sms_status_handler(
    Path((id, status)): Path<(Uuid, String)>,
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<SmsMessage>, StatusCode> {
    let status = status.to_ascii_uppercase();
    if !FINAL_STATUSES.contains(&status.as_str()) {
        return Err(StatusCode::BAD_REQUEST);
    }

    match finalize(&state, id, &status).await {
        Ok(sms) => Ok(Json(sms)),
        Err(FinalizeError::NotFound) => Err(StatusCode::NOT_FOUND),
        Err(FinalizeError::AlreadyFinal) => Err(StatusCode::CONFLICT),
    }
}

/// change the status of a sender ID registration
async fn registration_status_handler(
    Path((id, status)): Path<(Uuid, String)>,
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Json<Registration>, StatusCode> {
    let status = status.to_ascii_uppercase();
    if !REGISTRATION_STATUSES.contains(&status.as_str()) {
        return Err(StatusCode::BAD_REQUEST);
    }

    match set_status(&state, id, &status).await {
        Ok(registration) => Ok(Json(registration)),
        Err(SetStatusError::NotFound) => Err(StatusCode::NOT_FOUND),
        Err(SetStatusError::AlreadyFinal) => Err(StatusCode::CONFLICT),
    }
}

/// return the Sinch mock configuration, shown in the UI
async fn sinch_info_handler(Extension(state): Extension<Arc<AppState>>) -> Json<SinchInfo> {
    Json(state.sinch.info())
}

/// change the Sinch mock settings, they apply to messages accepted from now on
async fn sinch_settings_handler(
    Extension(state): Extension<Arc<AppState>>,
    Json(settings): Json<SinchSettings>,
) -> Result<Json<SinchInfo>, (StatusCode, String)> {
    let settings = settings
        .validate()
        .map_err(|e| (StatusCode::BAD_REQUEST, e))?;
    state.sinch.set_settings(settings);
    info!("Sinch mock settings changed");

    let info = state.sinch.info();
    let _ = state.events.send(Event::Settings(info.clone()));

    Ok(Json(info))
}

/// return version
async fn version_handler() -> Result<Json<VersionInfo>, StatusCode> {
    let vi = VersionInfo {
        version_be: VERSION.to_string(),
    };

    Ok(Json(vi))
}

/// return raw attachment by index
async fn attachment_handler(
    Path((id, index)): Path<(Uuid, usize)>,
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Response, StatusCode> {
    let storage = state
        .storage
        .read()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let message = storage.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    let (filename, mime, bytes) = message
        .attachment_content(index)
        .ok_or(StatusCode::NOT_FOUND)?;
    let disposition = format!(
        "attachment; filename=\"{}\"",
        filename.replace('\\', "\\\\").replace('"', "\\\"")
    );
    let len = bytes.len();
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, mime)
        .header(header::CONTENT_LENGTH, len)
        .header(header::CONTENT_DISPOSITION, disposition)
        .body(Body::from(bytes))
        .unwrap())
}

/// return the raw message (plain/text)
async fn message_raw_handler(
    Path(id): Path<Uuid>,
    Extension(state): Extension<Arc<AppState>>,
) -> Result<Response, StatusCode> {
    let storage = state
        .storage
        .read()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    let message = storage.get(&id).ok_or(StatusCode::NOT_FOUND)?;
    let bytes = message.raw_bytes().unwrap_or_default();
    let len = bytes.len();
    Ok(Response::builder()
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .header(header::CONTENT_LENGTH, len)
        .body(Body::from(bytes))
        .unwrap())
}

async fn not_found() -> Response {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .body(Body::from("404"))
        .unwrap()
}

async fn index(Extension(state): Extension<Arc<AppState>>) -> impl IntoResponse {
    Html(state.index.as_ref().expect("index.html not found").clone())
}

async fn static_handler(uri: Uri) -> impl IntoResponse {
    let path = uri.path().trim_start_matches('/');
    let mime = std::path::Path::new(path)
        .extension()
        .and_then(OsStr::to_str)
        .and_then(|ext| match ext.to_lowercase().as_str() {
            "js" => Some("text/javascript"),
            "css" => Some("text/css"),
            "svg" => Some("image/svg+xml"),
            "png" => Some("image/png"),
            "wasm" => Some("application/wasm"),
            "woff2" => Some("font/woff2"),
            _ => None,
        });

    match (Asset::get(path), mime) {
        (Some(content), Some(mime)) => Response::builder()
            .header(header::CONTENT_TYPE, mime)
            .body(Body::from(content.data))
            .unwrap(),
        _ => not_found().await,
    }
}

pub async fn web_server(
    host: IpAddr,
    port: u16,
    app_state: Arc<AppState>,
    token: CancellationToken,
) -> AppResult<()> {
    let mut router = Router::new()
        .route("/ws", get(ws_handler))
        .route("/api/messages", get(messages_handler))
        .route("/api/message/{id}", get(message_handler))
        .route("/api/message/{id}/body", get(message_body_handler))
        .route("/api/delete/{id}", post(message_delete_handler))
        .route("/api/delete-all", post(message_delete_all_handler))
        .route("/api/version", get(version_handler))
        .route(
            "/api/message/{id}/attachment/{index}",
            get(attachment_handler),
        )
        .route("/api/message/{id}/raw", get(message_raw_handler))
        .route("/api/sms", get(sms_list_handler))
        .route("/api/sms/{id}/status/{status}", post(sms_status_handler))
        .route("/api/registrations", get(registrations_handler))
        .route(
            "/api/registration/{id}/status/{status}",
            post(registration_status_handler),
        )
        .route("/api/sinch", get(sinch_info_handler))
        .route("/api/sinch/settings", put(sinch_settings_handler))
        .nest_service("/static", get(static_handler))
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::default().include_headers(true)),
        );

    if app_state.index.is_some() {
        router = router.route("/", get(index));
    }

    let app = match app_state.prefix.as_str() {
        "/" | "" => router,
        prefix => Router::new().nest(prefix, router.clone()),
    }
    .layer(Extension(app_state.clone()));

    let addr = SocketAddr::from((host, port));
    let listener = TcpListener::bind(&addr).await?;

    info!("HTTP server ready to accept connections");

    axum::serve(listener, app)
        .with_graceful_shutdown(Box::leak(Box::new(token)).cancelled())
        .await
        .map_err(|e| Error::WebServer(e.to_string()))?;

    Ok(())
}
