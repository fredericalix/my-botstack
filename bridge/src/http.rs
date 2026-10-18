//! HTTP surface.
//!
//! - `/botstack/v1`: a device's always-on connection (docs/protocol.md).
//! - `/botstack/v1/firmware`: the published firmware of the device's project.
//! - `/grok/*`: Grok Bot MCP and callback.
//! - `/fleet/mcp`: fleet administration for an AI client (`FLEET_ADMIN_TOKEN`).
//! - `/mcp/vision/explain`: photos from the robot's camera tool, described by
//!   the vision model, or handed to Grok Bot (`stackchan_photo`).

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{Extension, Router};
use axum::body::Bytes;
use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Multipart, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{any, get, post};
use tracing::{info, warn};

use crate::App;
use crate::fleet::Auth;
use crate::grokbot::{HostTool, MAX_BODY};

/// A 320×240 JPEG at quality 80 is about 10–30 kB.
const VISION_MAX_BODY: usize = 1024 * 1024;

pub fn router(app: Arc<App>) -> Router {
    Router::new()
        .route("/botstack/v1", get(link))
        .route("/botstack/v1/firmware", get(firmware))
        .route("/grok/{*rest}", any(grok))
        .route("/fleet/mcp", any(fleet))
        .route(
            "/mcp/vision/explain",
            post(vision).layer(DefaultBodyLimit::max(VISION_MAX_BODY)),
        )
        .route("/healthz", get(|| async { "ok" }))
        .route("/metrics", get(metrics))
        .layer(DefaultBodyLimit::max(MAX_BODY))
        .with_state(app)
}

fn header<'a>(headers: &'a HeaderMap, name: &str) -> &'a str {
    headers.get(name).and_then(|v| v.to_str().ok()).unwrap_or("")
}

/// The client address the proxy saw (Caddy sets `X-Forwarded-For` and
/// drops one the client sent).
fn peer(headers: &HeaderMap) -> String {
    header(headers, "x-forwarded-for").split(',').next().unwrap_or("").trim().chars().take(64).collect()
}

/// The client's address: the proxy's `X-Forwarded-For`, else the socket peer
/// when the server records it, else empty.
fn client(headers: &HeaderMap, socket: Option<SocketAddr>) -> String {
    let forwarded = peer(headers);
    if !forwarded.is_empty() {
        return forwarded;
    }
    socket.map(|address| address.ip().to_string()).unwrap_or_default()
}

/// The device's MAC when its key opens it (docs/fleet.md): approved, or
/// pending with the bootstrap key.
fn device(app: &App, headers: &HeaderMap, what: &str) -> Result<String, StatusCode> {
    let device_id = header(headers, "device-id").trim().to_lowercase();
    let key = header(headers, "authorization").strip_prefix("Bearer ").unwrap_or("").trim();
    match app.fleet.authenticate(&device_id, key) {
        Auth::Refused(status) => {
            warn!(device = device_id, status, "{what} refused");
            Err(StatusCode::from_u16(status).unwrap_or(StatusCode::FORBIDDEN))
        }
        Auth::Approved | Auth::Pending => Ok(device_id),
    }
}

/// botstack/v1: the robot's always-on connection.
async fn link(State(app): State<Arc<App>>, headers: HeaderMap, upgrade: WebSocketUpgrade) -> Response {
    let device_id = match device(&app, &headers, "link") {
        Ok(device_id) => device_id,
        Err(status) => return status.into_response(),
    };
    let peer = peer(&headers);
    upgrade
        .max_message_size(64 * 1024)
        .max_frame_size(64 * 1024)
        .on_upgrade(move |socket| crate::link::run(app, socket, device_id, peer))
}

/// The published firmware (`scripts/publish-firmware.sh`). The robot checks
/// its signature before it installs it.
async fn firmware(State(app): State<Arc<App>>, headers: HeaderMap) -> Response {
    let device_id = match device(&app, &headers, "firmware download") {
        Ok(device_id) => device_id,
        Err(status) => return status.into_response(),
    };
    let project = app.fleet.live(&device_id).map(|live| live.firmware).unwrap_or_default();
    let Some(path) = app.firmware.path(&project) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    match tokio::fs::read(&path).await {
        Ok(image) => {
            let build = crate::firmware::image_info(&image).map(|image| image.build).unwrap_or_default();
            info!(device = device_id, project, build, bytes = image.len(), "firmware download");
            ([(axum::http::header::CONTENT_TYPE, "application/octet-stream")], image).into_response()
        }
        Err(_) => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Fleet administration (docs/fleet.md), with its own token.
async fn fleet(
    State(app): State<Arc<App>>,
    socket: Option<Extension<ConnectInfo<SocketAddr>>>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let client = client(&headers, socket.map(|Extension(ConnectInfo(address))| address));
    let authorization = headers.get("authorization").and_then(|v| v.to_str().ok());
    let session = headers.get("mcp-session-id").and_then(|v| v.to_str().ok());
    into_http(crate::admin::handle(&app, &client, method.as_str(), authorization, session, &body))
}

async fn grok(State(app): State<Arc<App>>, method: Method, uri: Uri, headers: HeaderMap, body: Bytes) -> Response {
    let authorization = headers.get("authorization").and_then(|v| v.to_str().ok());
    let session = headers.get("mcp-session-id").and_then(|v| v.to_str().ok());
    let response = match app.grokbot.host_call(method.as_str(), uri.path(), authorization, session, &body) {
        Some(call) if call.refused.is_some() => app.grokbot.text_response(&call, call.refused.unwrap_or(""), true),
        Some(call) if call.tool == HostTool::Photo => {
            let mac = call.device_id.clone().unwrap_or_default();
            let photo = app.take_photo(&mac).await;
            match &photo {
                Ok(jpeg) => info!(device = mac, bytes = jpeg.len(), "photo sent to Grok Bot"),
                Err(reason) => warn!(device = mac, reason, "no photo for Grok Bot"),
            }
            app.grokbot.photo_response(&call, photo)
        }
        Some(call) => {
            if let HostTool::Lock(Some(locked)) = call.tool {
                match app.set_locked(call.device_id.as_deref(), locked) {
                    Ok(()) => info!(locked, device = call.device_id.as_deref().unwrap_or("all"), "lock switched by Grok Bot"),
                    Err(reason) => return into_http(app.grokbot.text_response(&call, &reason, true)),
                }
            }
            app.grokbot.text_response(&call, &app.lock_report(call.device_id.as_deref()), false)
        }
        None => app.grokbot.handle(method.as_str(), uri.path(), authorization, session, &body),
    };
    into_http(response)
}

fn into_http(response: crate::mcp::Response) -> Response {
    let mut builder = Response::builder().status(response.status);
    for (name, value) in response.headers {
        builder = builder.header(name, value);
    }
    builder.body(response.body.into()).unwrap_or_else(|_| StatusCode::INTERNAL_SERVER_ERROR.into_response())
}

/// The camera tool posts here. The body we return is the tool result the
/// model reads, so failures are returned as 200 with a short explanation.
async fn vision(State(app): State<Arc<App>>, headers: HeaderMap, mut form: Multipart) -> Response {
    let device_id = header(&headers, "device-id").trim().to_lowercase();
    let token = header(&headers, "authorization").strip_prefix("Bearer ").unwrap_or("");
    let allowed = app.fleet.is_approved(&device_id) && app.fleet.has_feature(&device_id, "camera");
    if !allowed || !crate::mcp::constant_time_eq(token.as_bytes(), app.vision.token.as_bytes()) {
        warn!(device = device_id, "vision refused");
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let mut question = String::new();
    let mut jpeg = Vec::new();
    loop {
        match form.next_field().await {
            Ok(Some(field)) => {
                let name = field.name().unwrap_or("").to_string();
                match field.bytes().await {
                    Ok(bytes) if name == "question" => question = String::from_utf8_lossy(&bytes).to_string(),
                    Ok(bytes) if name == "file" => jpeg = bytes.to_vec(),
                    Ok(_) => {}
                    Err(error) => {
                        warn!(%error, "vision upload failed");
                        return "The photo upload failed.".into_response();
                    }
                }
            }
            Ok(None) => break,
            Err(error) => {
                warn!(%error, "vision upload unreadable");
                return "The photo upload failed.".into_response();
            }
        }
    }
    if let Some(key) = question.strip_prefix(crate::PHOTO_QUESTION_PREFIX) {
        let result = if jpeg.is_empty() { Err("The camera sent no photo.".to_string()) } else { Ok(jpeg) };
        return if app.deliver_photo(key, result) {
            "Photo delivered."
        } else {
            "Nobody waits for this photo any more."
        }
        .into_response();
    }
    if jpeg.is_empty() {
        return "The camera sent no photo.".into_response();
    }
    info!(device = device_id, bytes = jpeg.len(), question, "photo");
    let language = app.fleet.device(&device_id).map(|d| d.language).unwrap_or_else(|| app.fleet.default_language().to_string());
    match app.vision.explain(&question, &jpeg, &language).await {
        Ok(answer) => {
            info!(device = device_id, answer, "vision");
            answer.into_response()
        }
        Err(error) => {
            warn!(%error, "vision model failed");
            "The vision model did not answer. Say you could not see well.".into_response()
        }
    }
}

async fn metrics(State(app): State<Arc<App>>) -> String {
    let mut text = String::from(
        "# HELP botstack_realtime_seconds_total Conversation time on xAI realtime voice.\n\
         # TYPE botstack_realtime_seconds_total counter\n",
    );
    text += &format!("botstack_realtime_seconds_total {}\n", app.realtime_seconds());
    let mut online = 0;
    for (mac, device, live) in app.fleet.entries() {
        let Some(live) = live else { continue };
        online += usize::from(live.online);
        let label = if device.name.is_empty() { mac } else { device.name };
        text += &format!("botstack_realtime_seconds_total{{device=\"{label}\"}} {}\n", live.seconds_total);
    }
    text += &format!(
        "# HELP botstack_devices_online Devices connected now.\n# TYPE botstack_devices_online gauge\nbotstack_devices_online {online}\n\
         # HELP botstack_conversations_active Conversations holding an xAI session (cap {}).\n\
         # TYPE botstack_conversations_active gauge\nbotstack_conversations_active {}\n",
        app.slots.max(),
        app.slots.active()
    );
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_client_is_the_first_forwarded_hop_else_the_socket() {
        let socket = Some(SocketAddr::from(([192, 0, 2, 9], 50000)));
        let mut headers = HeaderMap::new();
        assert_eq!(client(&headers, socket), "192.0.2.9");
        assert_eq!(client(&headers, None), "");
        headers.insert("x-forwarded-for", "203.0.113.7, 10.0.0.2".parse().unwrap());
        assert_eq!(client(&headers, socket), "203.0.113.7");
    }
}
