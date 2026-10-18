//! Grok Bot side of the bridge.
//!
//! - `ask_grok_bot` (a realtime tool) posts a webhook; your agent answers later
//!   through MCP `stackchan_reply(request_id, text)`.
//! - A Grok Bot agent can also push `stackchan_say(text)` at any time.
//! - `stackchan_photo` returns a photo from the robot's camera, and
//!   `stackchan_lock` locks or unlocks the robot. They act on the robot, so the
//!   HTTP layer runs them (`host_call`, then `photo_response` / `text_response`).
//! - Text that cannot be spoken now is kept in a small JSON file on the volume
//!   and spoken at the start of the next conversation.
//!
//! Only approved devices of the fleet may ask; devices are named by name or MAC;
//! the budgets are global (the device-id header is client input). A reply whose
//! request is unknown (restart, expiry) goes to `GROK_DEVICE_ID` when that is
//! set, or to the only approved device when it is not. With no such device the
//! call is an error: nothing is queued under a name no robot will ever read.
//! If `stackchan_reply` does not arrive within `reply_timeout`, the robot says
//! that Grok Bot did not answer (`claim_silence`).
//!
//! `handle` is plain data in and out, so the HTTP layer stays thin and the
//! behaviour is tested without a server.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use serde_json::{Value, json};

pub use crate::mcp::Response;
use crate::fleet::{Fleet, Status};
use crate::say;
use crate::mcp::{self, json_response, string_field, tool_result};

pub const MAX_BODY: usize = 64 * 1024;
const MAX_PENDING: usize = 5;
const MAX_TEXT: usize = 500;
const RATE_WINDOW_S: f64 = 3600.0;
const REQUEST_TTL_S: f64 = 3600.0;
const PUSH_PER_HOUR: usize = 30;
/// The camera can be triggered from afar: keep it rare.
const PHOTOS_PER_HOUR: usize = 20;
/// How long the robot waits for `stackchan_reply` before it says Grok Bot
/// did not answer. `GROK_BOT_REPLY_TIMEOUT` overrides it (seconds).
pub const DEFAULT_REPLY_TIMEOUT_SECS: u64 = 180;

/// What the text is, for the live session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Answer to an `ask_grok_bot` call: spoken as soon as possible.
    Reply,
    /// Unsolicited message: waits for the end of the conversation.
    Push,
}

/// Live delivery to a connected robot. Returns false when no conversation
/// can take the text, so it goes to the pending file.
pub trait Speaker: Send + Sync {
    fn speak(&self, device_id: &str, request_id: Option<&str>, text: &str, kind: Kind) -> bool;
}

pub type Clock = Arc<dyn Fn() -> f64 + Send + Sync>;
pub type Poster = Arc<dyn Fn(String, Value) -> futures_util::future::BoxFuture<'static, Result<u16, String>> + Send + Sync>;

pub struct Config {
    pub token: String,
    pub webhook_url: String,
    pub webhook_token: String,
    pub fleet: Arc<Fleet>,
    pub default_device: String,
    pub pending_path: PathBuf,
    pub rate_per_hour: usize,
    /// Wait for `stackchan_reply` this long, then have the robot say Grok Bot
    /// did not answer.
    pub reply_timeout: Duration,
}

pub struct GrokBot {
    token: String,
    webhook_url: String,
    fleet: Arc<Fleet>,
    default_device: String,
    pending_path: PathBuf,
    rate_per_hour: usize,
    reply_timeout: Duration,
    clock: Clock,
    poster: Poster,
    speaker: Mutex<Option<Arc<dyn Speaker>>>,
    state: Mutex<State>,
    session_id: String,
}

struct Pending {
    device: String,
    at: f64,
    /// The robot was already told that this request got no answer.
    silence_announced: bool,
}

#[derive(Default)]
struct State {
    requests: HashMap<String, Pending>,
    calls: VecDeque<f64>,
    pushes: VecDeque<f64>,
    photos: VecDeque<f64>,
}

/// A tool the HTTP layer runs, because it acts on the robot.
#[derive(Debug, PartialEq)]
pub enum HostTool {
    Photo,
    /// Some: lock or unlock; None: only report the state.
    Lock(Option<bool>),
}

/// An authorized call of a `HostTool`.
pub struct HostCall {
    id: Value,
    session: String,
    pub tool: HostTool,
    /// The device; None for a lock of the whole fleet.
    pub device_id: Option<String>,
    /// Set when the call is refused; nothing is done.
    pub refused: Option<&'static str>,
}

impl GrokBot {
    pub fn new(config: Config, clock: Option<Clock>, poster: Option<Poster>) -> Self {
        let webhook_token = config.webhook_token.clone();
        let poster = poster.unwrap_or_else(|| {
            let client = reqwest::Client::builder()
                .timeout(Duration::from_secs(8))
                .build()
                .expect("http client");
            Arc::new(move |url: String, payload: Value| {
                let client = client.clone();
                let token = webhook_token.clone();
                Box::pin(async move {
                    let mut request = client.post(url).json(&payload);
                    if !token.is_empty() {
                        request = request.bearer_auth(token);
                    }
                    request
                        .send()
                        .await
                        .map(|r| r.status().as_u16())
                        .map_err(|e| if e.is_timeout() { "timeout".into() } else { "connect".into() })
                })
            })
        });
        Self {
            token: config.token,
            webhook_url: config.webhook_url.trim().to_string(),
            fleet: config.fleet,
            default_device: config.default_device.trim().to_lowercase(),
            pending_path: config.pending_path,
            rate_per_hour: config.rate_per_hour,
            reply_timeout: config.reply_timeout,
            clock: clock.unwrap_or_else(|| Arc::new(unix_now)),
            poster,
            speaker: Mutex::new(None),
            state: Mutex::new(State::default()),
            session_id: uuid::Uuid::new_v4().simple().to_string(),
        }
    }

    pub fn set_speaker(&self, speaker: Arc<dyn Speaker>) {
        *self.speaker.lock().unwrap() = Some(speaker);
    }

    pub fn webhook_configured(&self) -> bool {
        !self.webhook_url.is_empty()
    }

    pub fn reply_timeout(&self) -> Duration {
        self.reply_timeout
    }

    /// The MAC of an approved device named `reference` (name or MAC). An empty
    /// reference is the default device: `GROK_DEVICE_ID` when it is approved,
    /// or the only approved device when `GROK_DEVICE_ID` is unset.
    pub fn resolve(&self, reference: &str) -> Result<String, &'static str> {
        let reference = reference.trim();
        if reference.is_empty() {
            return self.approved_default().ok_or("unknown device");
        }
        match self.fleet.find(reference) {
            Some((mac, device)) if device.status == Status::Approved => Ok(mac),
            _ => Err("unknown device"),
        }
    }

    /// Where a reply goes when it names no device. `GROK_DEVICE_ID` when set
    /// (a forgotten request still has somewhere to wait, even if that device
    /// is not approved right now). Otherwise the only approved device.
    fn fallback_device(&self) -> Option<String> {
        if !self.default_device.is_empty() {
            return Some(self.default_device.clone());
        }
        self.sole_approved()
    }

    fn approved_default(&self) -> Option<String> {
        if !self.default_device.is_empty() {
            return self.fleet.is_approved(&self.default_device).then(|| self.default_device.clone());
        }
        self.sole_approved()
    }

    fn sole_approved(&self) -> Option<String> {
        let mut found = None;
        for (mac, device, _) in self.fleet.entries() {
            if device.status != Status::Approved {
                continue;
            }
            if found.is_some() {
                return None;
            }
            found = Some(mac);
        }
        found
    }

    /// The approved devices, for `stackchan_devices`.
    fn devices(&self) -> String {
        let list: Vec<Value> = self
            .fleet
            .entries()
            .into_iter()
            .filter(|(_, device, _)| device.status == Status::Approved)
            .map(|(mac, device, live)| {
                json!({
                    "name": device.name,
                    "mac": mac,
                    "type": live.as_ref().map(|l| l.firmware.clone()).unwrap_or_default(),
                    "online": live.as_ref().is_some_and(|l| l.online),
                    "locked": device.locked,
                    "language": device.language,
                    "state": live.as_ref().map(|l| l.state.clone()).unwrap_or_default(),
                    "features": live.map(|l| l.features).unwrap_or_default(),
                })
            })
            .collect();
        serde_json::to_string_pretty(&list).unwrap_or_default()
    }

    /// One budget for the whole server.
    pub fn allow(&self) -> bool {
        let now = (self.clock)();
        let mut state = self.state.lock().unwrap();
        while state.calls.front().is_some_and(|t| now - t > RATE_WINDOW_S) {
            state.calls.pop_front();
        }
        if state.calls.len() >= self.rate_per_hour {
            return false;
        }
        state.calls.push_back(now);
        true
    }

    fn allow_push(&self) -> bool {
        let now = (self.clock)();
        within_budget(&mut self.state.lock().unwrap().pushes, now, PUSH_PER_HOUR)
    }

    fn allow_photo(&self) -> bool {
        let now = (self.clock)();
        within_budget(&mut self.state.lock().unwrap().photos, now, PHOTOS_PER_HOUR)
    }

    pub fn remember(&self, request_id: &str, device_id: &str) {
        let now = (self.clock)();
        let mut state = self.state.lock().unwrap();
        state.requests.retain(|_, pending| now - pending.at <= REQUEST_TTL_S);
        state.requests.insert(
            request_id.to_string(),
            Pending { device: device_id.to_string(), at: now, silence_announced: false },
        );
    }

    /// True the first time `request_id` is still waiting, so the robot says
    /// Grok Bot did not answer. A reply that arrives later is still delivered.
    pub fn claim_silence(&self, request_id: &str) -> bool {
        let mut state = self.state.lock().unwrap();
        let Some(pending) = state.requests.get_mut(request_id) else { return false };
        if pending.silence_announced {
            return false;
        }
        pending.silence_announced = true;
        true
    }

    pub fn forget(&self, request_id: &str) {
        self.state.lock().unwrap().requests.remove(request_id);
    }

    /// Post the question to your Grok Bot agent. Err is a short reason,
    /// spoken in the device's `language`.
    pub async fn post_webhook(&self, payload: Value, language: &str) -> Result<(), &'static str> {
        if self.webhook_url.is_empty() {
            return Err(say(language, "Grok Bot n'est pas encore configuré.", "Grok Bot is not set up yet."));
        }
        match (self.poster)(self.webhook_url.clone(), payload).await {
            Ok(status) if (200..300).contains(&status) => Ok(()),
            Ok(_) => Err(say(language, "Grok Bot n'a pas accepté la question.", "Grok Bot did not accept the question.")),
            Err(_) => Err(say(language, "Je n'arrive pas à joindre Grok Bot.", "I can't reach Grok Bot.")),
        }
    }

    /// Speak now if a conversation can take it, else queue it.
    /// Ok is "empty", "spoken", or "queued". Err when a reply names no device
    /// and there is no default (nothing would ever read a queued text).
    pub fn accept(&self, request_id: Option<&str>, text: &str, device_id: Option<&str>, kind: Kind) -> Result<&'static str, &'static str> {
        let cleaned = clean_speech(text);
        if cleaned.is_empty() {
            return Ok("empty");
        }
        let known = request_id.and_then(|id| self.state.lock().unwrap().requests.remove(id));
        let explicit = device_id.map(str::trim).filter(|d| !d.is_empty()).map(str::to_string);
        let from_request = known.and_then(|pending| {
            if pending.device.is_empty() || pending.device == "unknown" { None } else { Some(pending.device) }
        });
        let Some(target) = explicit.or(from_request).or_else(|| self.fallback_device()) else {
            return Err("unknown request and no default device");
        };
        let speaker = self.speaker.lock().unwrap().clone();
        if let Some(speaker) = speaker
            && speaker.speak(&target, request_id, &cleaned, kind) {
                return Ok("spoken");
            }
        self.enqueue(&target, &cleaned);
        Ok("queued")
    }

    /// Pending texts for this device, consumed once.
    pub fn take_pending(&self, device_id: &str) -> Vec<String> {
        let key = device_id.trim().to_lowercase();
        let _guard = self.state.lock().unwrap();
        let mut pending = self.read_pending();
        let mut items = Vec::new();
        pending.retain(|name, values| {
            if name.to_lowercase() == key {
                items.append(values);
                false
            } else {
                true
            }
        });
        if !items.is_empty() {
            self.write_pending(&pending);
        }
        items
    }

    fn enqueue(&self, device_id: &str, text: &str) {
        let _guard = self.state.lock().unwrap();
        let mut pending = self.read_pending();
        let items = pending.entry(device_id.to_lowercase()).or_default();
        items.push(text.to_string());
        let excess = items.len().saturating_sub(MAX_PENDING);
        items.drain(..excess);
        self.write_pending(&pending);
    }

    fn read_pending(&self) -> HashMap<String, Vec<String>> {
        std::fs::read_to_string(&self.pending_path)
            .ok()
            .and_then(|raw| serde_json::from_str(&raw).ok())
            .unwrap_or_default()
    }

    fn write_pending(&self, pending: &HashMap<String, Vec<String>>) {
        if let Some(parent) = self.pending_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let temporary = self.pending_path.with_extension("tmp");
        if std::fs::write(&temporary, serde_json::to_vec(pending).unwrap_or_default()).is_ok() {
            let _ = std::fs::rename(&temporary, &self.pending_path);
        }
    }

    /// HTTP entry for `/grok/*`.
    pub fn handle(&self, method: &str, path: &str, authorization: Option<&str>, mcp_session: Option<&str>, body: &[u8]) -> Response {
        let path = path.split('?').next().unwrap_or("");
        if method == "GET" && path == "/grok/health" {
            return json_response(200, json!({"ok": true}), vec![]);
        }
        if !self.authorized(authorization) {
            return json_response(401, json!({"error": "unauthorized"}), vec![]);
        }
        if path == "/grok/mcp" && method != "POST" {
            // Streamable HTTP: no server-initiated SSE stream, no session to delete.
            return json_response(405, json!({"error": "method not allowed"}), vec![("Allow", "POST".into())]);
        }
        match (method, path) {
            ("POST", "/grok/callback") => self.callback(body),
            ("POST", "/grok/mcp") => self.mcp(body, mcp_session),
            _ => json_response(404, json!({"error": "not found"}), vec![]),
        }
    }

    /// Some when the request is an authorized MCP `tools/call` of
    /// `stackchan_photo` or `stackchan_lock`: the HTTP layer runs it, then
    /// answers with `photo_response` or `text_response`. Everything else goes
    /// through `handle`.
    pub fn host_call(&self, method: &str, path: &str, authorization: Option<&str>, mcp_session: Option<&str>, body: &[u8]) -> Option<HostCall> {
        if method != "POST" || path.split('?').next() != Some("/grok/mcp") || !self.authorized(authorization) {
            return None;
        }
        let message: Value = serde_json::from_slice(body).ok()?;
        let id = message.get("id").filter(|id| !id.is_null())?.clone();
        if message["method"] != "tools/call" {
            return None;
        }
        let arguments = &message["params"]["arguments"];
        let tool = match message["params"]["name"].as_str() {
            Some("stackchan_photo") => HostTool::Photo,
            Some("stackchan_lock") => HostTool::Lock(arguments["locked"].as_bool()),
            _ => return None,
        };
        let asked = string_field(arguments, "device_id");
        let (device_id, refused) = match (&tool, self.resolve(&asked)) {
            (HostTool::Lock(_), _) if asked.trim().is_empty() => (None, None),
            (_, Err(reason)) => (None, Some(reason)),
            (HostTool::Photo, Ok(mac)) if self.fleet.live(&mac).is_some_and(|live| !live.has("camera")) => {
                (Some(mac), Some("this device has no camera"))
            }
            (HostTool::Photo, Ok(mac)) if !self.allow_photo() => (Some(mac), Some("rate limited: at most 20 photos an hour")),
            (_, Ok(mac)) => (Some(mac), None),
        };
        let session = mcp_session.unwrap_or(&self.session_id).to_string();
        Some(HostCall { id, session, tool, device_id, refused })
    }

    /// The MCP answer to a `stackchan_photo` call: the JPEG as image content,
    /// or the reason there is none.
    pub fn photo_response(&self, call: &HostCall, photo: Result<Vec<u8>, String>) -> Response {
        let result = match photo {
            Ok(jpeg) => json!({
                "content": [
                    {"type": "image", "data": B64.encode(&jpeg), "mimeType": "image/jpeg"},
                    {"type": "text", "text": "A photo StackChan's camera just took."}
                ],
                "isError": false
            }),
            Err(reason) => tool_result(&reason, true),
        };
        json_response(200, json!({"jsonrpc": "2.0", "id": call.id, "result": result}), vec![("Mcp-Session-Id", call.session.clone())])
    }

    /// The MCP answer to a `HostCall` that returns text.
    pub fn text_response(&self, call: &HostCall, text: &str, is_error: bool) -> Response {
        json_response(
            200,
            json!({"jsonrpc": "2.0", "id": call.id, "result": tool_result(text, is_error)}),
            vec![("Mcp-Session-Id", call.session.clone())],
        )
    }

    fn authorized(&self, authorization: Option<&str>) -> bool {
        mcp::bearer_ok(authorization, &self.token)
    }

    fn callback(&self, body: &[u8]) -> Response {
        let Ok(payload) = serde_json::from_slice::<Value>(body) else {
            return json_response(400, json!({"error": "invalid json"}), vec![]);
        };
        if !payload.is_object() {
            return json_response(400, json!({"error": "expected a json object"}), vec![]);
        }
        let request_id = string_field(&payload, "request_id");
        let text = string_field(&payload, "text");
        if request_id.is_empty() || text.is_empty() {
            return json_response(400, json!({"error": "request_id and text are required"}), vec![]);
        }
        let mut device_id = string_field(&payload, "device_id");
        if !device_id.is_empty() {
            match self.resolve(&device_id) {
                Ok(mac) => device_id = mac,
                Err(_) => return json_response(400, json!({"error": "unknown device"}), vec![]),
            }
        }
        match self.accept(Some(&request_id), &text, Some(&device_id), Kind::Reply) {
            Ok(state) => json_response(200, json!({"ok": true, "delivery": state}), vec![]),
            Err(reason) => json_response(400, json!({"error": reason}), vec![]),
        }
    }

    fn mcp(&self, body: &[u8], mcp_session: Option<&str>) -> Response {
        let session = mcp_session.unwrap_or(&self.session_id);
        let (id, method, params) = match mcp::parse(body) {
            Ok(mcp::Message::Request { id, method, params }) => (id, method, params),
            Ok(mcp::Message::Notification) => return mcp::accepted(session),
            Err(response) => return response,
        };
        let result = match method.as_str() {
            "initialize" => mcp::initialize(&params, "stackchan-bridge"),
            "tools/list" => json!({"tools": [reply_tool(), say_tool(), photo_tool(), lock_tool(), devices_tool()]}),
            "tools/call" => self.call_tool(&params),
            _ => return mcp::method_not_found(id, &method, session),
        };
        mcp::result(id, result, session)
    }

    fn call_tool(&self, params: &Value) -> Value {
        let (name, arguments) = mcp::call(params);
        let text = string_field(&arguments, "text");
        let asked = string_field(&arguments, "device_id");
        let device_id = if asked.trim().is_empty() {
            String::new()
        } else {
            match self.resolve(&asked) {
                Ok(mac) => mac,
                Err(reason) => return tool_result(reason, true),
            }
        };
        match name {
            "stackchan_reply" => {
                let request_id = string_field(&arguments, "request_id");
                if request_id.is_empty() || text.is_empty() {
                    return tool_result("request_id and text are required", true);
                }
                match self.accept(Some(&request_id), &text, Some(&device_id), Kind::Reply) {
                    Ok(state) => tool_result(state, false),
                    Err(reason) => tool_result(reason, true),
                }
            }
            "stackchan_say" => {
                if text.is_empty() {
                    return tool_result("text is required", true);
                }
                let target = match self.resolve(&asked) {
                    Ok(mac) => mac,
                    Err(reason) => return tool_result(reason, true),
                };
                if !self.allow_push() {
                    return tool_result("rate limited: at most 30 messages an hour", true);
                }
                match self.accept(None, &text, Some(&target), Kind::Push) {
                    Ok(state) => tool_result(state, false),
                    Err(reason) => tool_result(reason, true),
                }
            }
            "stackchan_devices" => tool_result(&self.devices(), false),
            // Run by the HTTP layer (host_call); only reached without it.
            "stackchan_photo" | "stackchan_lock" => tool_result("not available here", true),
            other => tool_result(&format!("Unknown tool {other}"), true),
        }
    }
}

fn reply_tool() -> Value {
    json!({
        "name": "stackchan_reply",
        "description": "Speak the answer to one StackChan question. Call this once at the end of every webhook run, with the same request_id.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "request_id": {"type": "string"},
                "device_id": {"type": "string", "description": "Optional. The device_id from the webhook body."},
                "text": {"type": "string", "description": "One to three spoken sentences in the device's language (stackchan_devices gives it; the webhook body has `language`). No markdown and no URLs."}
            },
            "required": ["request_id", "text"]
        }
    })
}

fn say_tool() -> Value {
    json!({
        "name": "stackchan_say",
        "description": "Make StackChan say a message out loud, at any time. It waits for the end of a conversation, plays a chime, then speaks. If the robot is offline, it speaks at the next conversation.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "text": {"type": "string", "description": "One to three spoken sentences in the device's language (stackchan_devices gives it; the webhook body has `language`). No markdown and no URLs."},
                "device_id": {"type": "string", "description": "Optional. A device name from stackchan_devices, or its MAC. Default: the bridge's default device, when it has one."}
            },
            "required": ["text"]
        }
    })
}

fn photo_tool() -> Value {
    json!({
        "name": "stackchan_photo",
        "description": "Take a photo with StackChan's camera and return it. The camera is in the robot's face and looks where the robot faces, usually at the user's desk. The face shows that it is looking. 320x240 JPEG; it takes a few seconds. Only devices with a camera.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "device_id": {"type": "string", "description": "Optional. A device name from stackchan_devices, or its MAC. Default: the bridge's default device, when it has one."}
            }
        }
    })
}

fn devices_tool() -> Value {
    json!({
        "name": "stackchan_devices",
        "description": "List the owner's devices: name, type, online, locked, language (ISO 639-1, the language to write its texts in), state (idle, listening, thinking, speaking) and features (head, camera…). Use a name as device_id in the other tools.",
        "inputSchema": {"type": "object", "properties": {}}
    })
}

fn lock_tool() -> Value {
    json!({
        "name": "stackchan_lock",
        "description": "Lock or unlock a device, or every device. While it is locked nobody can talk to it (taps and head touches are refused, its face looks wary); stackchan_say and stackchan_photo still work. Without `locked`, only reports whether it is locked.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "locked": {"type": "boolean", "description": "true to lock, false to unlock. Omit to read the state."},
                "device_id": {"type": "string", "description": "Optional. A device name or MAC. Without it, every device."}
            }
        }
    })
}

/// Rolling one-hour budget: true (and counted) when `times` has room.
fn within_budget(times: &mut VecDeque<f64>, now: f64, per_hour: usize) -> bool {
    while times.front().is_some_and(|t| now - t > RATE_WINDOW_S) {
        times.pop_front();
    }
    if times.len() >= per_hour {
        return false;
    }
    times.push_back(now);
    true
}

/// Text for a voice: no markdown marks, one line, at most 500 characters.
pub fn clean_speech(text: &str) -> String {
    let flattened: String = text.chars().filter(|c| !matches!(c, '`' | '*' | '#')).collect();
    let joined = flattened.split_whitespace().collect::<Vec<_>>().join(" ");
    joined.chars().take(MAX_TEXT).collect()
}

pub(crate) fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

#[cfg(test)]
mod tests {
    //! Replies, pushes, budgets, devices, photos and locks.
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    const MAC: &str = "02:00:00:00:00:01";

    struct Recorder {
        spoken: Mutex<Vec<(String, String, Kind)>>,
    }

    impl Speaker for Recorder {
        fn speak(&self, device_id: &str, request_id: Option<&str>, text: &str, kind: Kind) -> bool {
            if request_id == Some("asleep") || kind == Kind::Push {
                return false;
            }
            self.spoken.lock().unwrap().push((device_id.into(), text.into(), kind));
            true
        }
    }

    struct Fixture {
        bot: GrokBot,
        fleet: Arc<crate::fleet::Fleet>,
        recorder: Arc<Recorder>,
        now: Arc<AtomicU64>,
        posts: Arc<Mutex<Vec<Value>>>,
        _dir: tempfile::TempDir,
    }

    fn fixture() -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let now = Arc::new(AtomicU64::new(1_000_000));
        let posts = Arc::new(Mutex::new(Vec::new()));
        let clock_now = now.clone();
        let poster_posts = posts.clone();
        let fleet = Arc::new(
            crate::fleet::Fleet::load(
                crate::fleet::Config {
                    path: dir.path().join("fleet.json"),
                    bootstrap: "boot".into(),
                    default_voice: "eve".into(),
                    default_language: "fr".into(),
                    timezone: chrono_tz::UTC,
                    seed: Some(crate::fleet::Seed { mac: MAC.into(), name: "stackchan".into(), locked: false }),
                },
                None,
            )
            .unwrap(),
        );
        let bot = GrokBot::new(
            Config {
                token: "secret-token".into(),
                fleet: fleet.clone(),
                webhook_url: "https://example.invalid/hook".into(),
                webhook_token: String::new(),
                default_device: "02:00:00:00:00:01".into(),
                pending_path: dir.path().join("pending.json"),
                rate_per_hour: 10,
                reply_timeout: Duration::from_secs(DEFAULT_REPLY_TIMEOUT_SECS),
            },
            Some(Arc::new(move || clock_now.load(Ordering::SeqCst) as f64)),
            Some(Arc::new(move |_url, payload| {
                poster_posts.lock().unwrap().push(payload);
                Box::pin(async { Ok(200) })
            })),
        );
        let recorder = Arc::new(Recorder { spoken: Mutex::new(Vec::new()) });
        bot.set_speaker(recorder.clone());
        Fixture { bot, fleet, recorder, now, posts, _dir: dir }
    }

    const AUTH: Option<&str> = Some("Bearer secret-token");

    fn mcp(bot: &GrokBot, message: Value) -> (u16, Value) {
        let response = bot.handle("POST", "/grok/mcp", AUTH, None, message.to_string().as_bytes());
        let body = if response.body.is_empty() { Value::Null } else { serde_json::from_slice(&response.body).unwrap() };
        (response.status, body)
    }

    #[test]
    fn health_is_public_and_other_routes_need_the_bearer() {
        let f = fixture();
        assert_eq!(f.bot.handle("GET", "/grok/health", None, None, b"").status, 200);
        assert_eq!(f.bot.handle("POST", "/grok/mcp", None, None, b"{}").status, 401);
        assert_eq!(f.bot.handle("POST", "/grok/mcp", Some("Bearer wrong"), None, b"{}").status, 401);
    }

    #[test]
    fn mcp_initialize_list_and_reply() {
        let f = fixture();
        f.bot.remember("req-1", "device-a");
        let (status, body) = mcp(&f.bot, json!({"jsonrpc": "2.0", "id": 1, "method": "initialize"}));
        assert_eq!(status, 200);
        assert_eq!(body["result"]["protocolVersion"], "2025-03-26");
        let (_, body) = mcp(&f.bot, json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}));
        let names: Vec<&str> = body["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["stackchan_reply", "stackchan_say", "stackchan_photo", "stackchan_lock", "stackchan_devices"]);
        let (_, body) = mcp(
            &f.bot,
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
                   "params": {"name": "stackchan_reply", "arguments": {"request_id": "req-1", "text": "Pong from **Grok**."}}}),
        );
        assert_eq!(body["result"]["isError"], false);
        assert_eq!(*f.recorder.spoken.lock().unwrap(), vec![("device-a".to_string(), "Pong from Grok.".to_string(), Kind::Reply)]);
    }

    fn photo_body(id: u64, arguments: Value) -> Vec<u8> {
        json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
               "params": {"name": "stackchan_photo", "arguments": arguments}})
        .to_string()
        .into_bytes()
    }

    #[test]
    fn photo_calls_are_found_answered_and_rate_limited() {
        let f = fixture();
        let body = photo_body(7, json!({}));
        assert!(f.bot.host_call("POST", "/grok/mcp", None, None, &body).is_none());
        let say = br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"stackchan_say","arguments":{}}}"#;
        assert!(f.bot.host_call("POST", "/grok/mcp", AUTH, None, say).is_none());
        let call = f.bot.host_call("POST", "/grok/mcp", AUTH, None, &body).unwrap();
        assert_eq!(call.tool, HostTool::Photo);
        assert_eq!(call.device_id.as_deref(), Some(MAC));
        assert!(call.refused.is_none());

        let response = f.bot.photo_response(&call, Ok(vec![0xFF, 0xD8, 0xFF]));
        let answer: Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(answer["id"], 7);
        assert_eq!(answer["result"]["content"][0]["type"], "image");
        assert_eq!(answer["result"]["content"][0]["data"], "/9j/");
        assert_eq!(answer["result"]["content"][0]["mimeType"], "image/jpeg");
        let failed = f.bot.photo_response(&call, Err("offline".into()));
        assert_eq!(serde_json::from_slice::<Value>(&failed.body).unwrap()["result"]["isError"], true);

        let stranger = photo_body(8, json!({"device_id": "11:22:33:44:55:66"}));
        assert_eq!(f.bot.host_call("POST", "/grok/mcp", AUTH, None, &stranger).unwrap().refused, Some("unknown device"));
        for _ in 1..PHOTOS_PER_HOUR {
            assert!(f.bot.host_call("POST", "/grok/mcp", AUTH, None, &body).unwrap().refused.is_none());
        }
        assert!(f.bot.host_call("POST", "/grok/mcp", AUTH, None, &body).unwrap().refused.is_some());
        f.now.fetch_add(3601, Ordering::SeqCst);
        assert!(f.bot.host_call("POST", "/grok/mcp", AUTH, None, &body).unwrap().refused.is_none());
    }

    #[test]
    fn lock_calls_carry_the_wanted_state() {
        let f = fixture();
        let lock = |arguments: Value| {
            let body = json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call",
                              "params": {"name": "stackchan_lock", "arguments": arguments}});
            f.bot.host_call("POST", "/grok/mcp", AUTH, None, body.to_string().as_bytes())
        };
        assert_eq!(lock(json!({"locked": true})).unwrap().tool, HostTool::Lock(Some(true)));
        assert_eq!(lock(json!({"locked": false})).unwrap().tool, HostTool::Lock(Some(false)));
        assert_eq!(lock(json!({})).unwrap().tool, HostTool::Lock(None));
        let call = lock(json!({})).unwrap();
        let answer: Value = serde_json::from_slice(&f.bot.text_response(&call, "StackChan is unlocked.", false).body).unwrap();
        assert_eq!(answer["result"]["content"][0]["text"], "StackChan is unlocked.");
        let body = json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {"name": "stackchan_lock", "arguments": {}}});
        assert!(f.bot.host_call("POST", "/grok/mcp", Some("Bearer wrong"), None, body.to_string().as_bytes()).is_none());
    }

    #[test]
    fn notification_is_accepted() {
        let f = fixture();
        let (status, body) = mcp(&f.bot, json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        assert_eq!(status, 202);
        assert_eq!(body, Value::Null);
    }

    #[test]
    fn callback_queues_when_the_robot_is_asleep() {
        let f = fixture();
        f.bot.remember("asleep", "device-b");
        let response = f.bot.handle("POST", "/grok/callback", AUTH, None, br#"{"request_id":"asleep","text":"late answer"}"#);
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(body["delivery"], "queued");
        assert_eq!(f.bot.take_pending("DEVICE-B"), vec!["late answer"]);
        assert!(f.bot.take_pending("device-b").is_empty());
    }

    #[test]
    fn unknown_request_goes_to_the_default_device() {
        let f = fixture();
        assert_eq!(f.bot.accept(Some("asleep"), "after restart", None, Kind::Reply), Ok("queued"));
        assert_eq!(f.bot.take_pending(MAC), vec!["after restart"]);
        // A configured default still wins when several devices are approved.
        f.fleet.authenticate("11:22:33:44:55:66", "boot");
        f.fleet.approve("11:22:33:44:55:66", "kitchen").unwrap();
        assert_eq!(f.bot.accept(Some("asleep"), "and another", None, Kind::Reply), Ok("queued"));
        assert_eq!(f.bot.take_pending(MAC), vec!["and another"]);
    }

    #[test]
    fn requests_expire() {
        let f = fixture();
        f.bot.remember("old", "device-b");
        f.now.fetch_add(3601, Ordering::SeqCst);
        f.bot.remember("new", "device-b");
        let state = f.bot.state.lock().unwrap();
        assert!(!state.requests.contains_key("old"));
        assert!(state.requests.contains_key("new"));
    }

    #[test]
    fn only_approved_devices_may_ask() {
        let f = fixture();
        assert_eq!(f.bot.resolve(MAC).as_deref(), Ok(MAC));
        assert!(f.bot.resolve("11:22:33:44:55:66").is_err());
        f.fleet.authenticate("11:22:33:44:55:66", "boot");
        assert!(f.bot.resolve("11:22:33:44:55:66").is_err());
        f.fleet.approve("11:22:33:44:55:66", "cuisine").unwrap();
        assert_eq!(f.bot.resolve("11:22:33:44:55:66").as_deref(), Ok("11:22:33:44:55:66"));
    }

    #[test]
    fn without_a_configured_default_the_only_approved_device_is_used() {
        let f = fixture();
        let bot = GrokBot::new(
            Config {
                token: "secret-token".into(),
                fleet: f.fleet.clone(),
                webhook_url: String::new(),
                webhook_token: String::new(),
                default_device: String::new(),
                pending_path: f._dir.path().join("other.json"),
                rate_per_hour: 10,
                reply_timeout: Duration::from_secs(DEFAULT_REPLY_TIMEOUT_SECS),
            },
            None,
            None,
        );
        assert_eq!(bot.resolve("").as_deref(), Ok(MAC));
        assert_eq!(bot.resolve("stackchan").as_deref(), Ok(MAC));
        f.fleet.authenticate("11:22:33:44:55:66", "boot");
        f.fleet.approve("11:22:33:44:55:66", "kitchen").unwrap();
        assert_eq!(bot.resolve(""), Err("unknown device"));
    }

    #[test]
    fn devices_are_named() {
        let f = fixture();
        f.fleet.authenticate("11:22:33:44:55:66", "boot");
        f.fleet.approve("11:22:33:44:55:66", "cuisine").unwrap();
        assert_eq!(f.bot.resolve("cuisine").as_deref(), Ok("11:22:33:44:55:66"));
        assert_eq!(f.bot.resolve("11:22:33:44:55:66").as_deref(), Ok("11:22:33:44:55:66"));
        assert_eq!(f.bot.resolve("").as_deref(), Ok(MAC));
        assert!(f.bot.resolve("garage").is_err());

        let (_, body) = mcp(&f.bot, json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "stackchan_say", "arguments": {"text": "Le four est chaud.", "device_id": "cuisine"}}}));
        assert_eq!(body["result"]["content"][0]["text"], "queued");
        assert_eq!(f.bot.take_pending("11:22:33:44:55:66"), vec!["Le four est chaud."]);

        let (_, body) = mcp(&f.bot, json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "stackchan_devices", "arguments": {}}}));
        let listed: Value = serde_json::from_str(body["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        // By MAC: 02:00:… sorts before 11:22:….
        let names: Vec<&str> = listed.as_array().unwrap().iter().map(|d| d["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["stackchan", "cuisine"]);
        let languages: Vec<&str> = listed.as_array().unwrap().iter().map(|d| d["language"].as_str().unwrap()).collect();
        assert_eq!(languages, ["fr", "fr"]);
        f.fleet.set_profile("11:22:33:44:55:66", crate::fleet::ProfileChange { language: Some("en".into()), ..Default::default() }).unwrap();
        let (_, body) = mcp(&f.bot, json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "stackchan_devices", "arguments": {}}}));
        let listed: Value = serde_json::from_str(body["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(listed[1]["language"], "en");
    }

    #[test]
    fn texts_are_asked_in_the_device_language() {
        let f = fixture();
        let (_, body) = mcp(&f.bot, json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}));
        let tools = body["result"]["tools"].as_array().unwrap();
        for name in ["stackchan_reply", "stackchan_say"] {
            let tool = tools.iter().find(|t| t["name"] == name).unwrap();
            let text = tool["inputSchema"]["properties"]["text"]["description"].as_str().unwrap();
            assert!(text.contains("in the device's language") && !text.contains("French"), "{name}: {text}");
        }
        assert!(!body.to_string().contains("French"));
    }

    #[test]
    fn a_say_without_a_device_needs_the_default_device_approved() {
        let f = fixture();
        f.fleet.revoke(MAC).unwrap();
        let (_, body) = mcp(&f.bot, json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "stackchan_say", "arguments": {"text": "Bonjour."}}}));
        assert_eq!(body["result"]["isError"], true);
        assert_eq!(body["result"]["content"][0]["text"], "unknown device");
        assert!(f.bot.take_pending(MAC).is_empty());
    }

    #[test]
    fn a_photo_needs_a_camera() {
        let f = fixture();
        f.fleet.authenticate("11:22:33:44:55:66", "boot");
        f.fleet.approve("11:22:33:44:55:66", "cuisine").unwrap();
        f.fleet.connected("11:22:33:44:55:66", 1, "", &json!({"firmware": "botstack-p4", "features": ["status"]}));
        let body = photo_body(3, json!({"device_id": "cuisine"}));
        assert_eq!(f.bot.host_call("POST", "/grok/mcp", AUTH, None, &body).unwrap().refused, Some("this device has no camera"));
    }

    #[test]
    fn a_lock_without_a_device_is_for_the_whole_fleet() {
        let f = fixture();
        let lock = |arguments: Value| {
            let body = json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": {"name": "stackchan_lock", "arguments": arguments}});
            f.bot.host_call("POST", "/grok/mcp", AUTH, None, body.to_string().as_bytes()).unwrap()
        };
        assert_eq!(lock(json!({"locked": true})).device_id, None);
        assert_eq!(lock(json!({"locked": true, "device_id": "stackchan"})).device_id.as_deref(), Some(MAC));
        assert_eq!(lock(json!({"locked": true, "device_id": "garage"})).refused, Some("unknown device"));
    }

    #[test]
    fn rate_limit_is_ten_per_hour_for_everyone() {
        let f = fixture();
        for _ in 0..10 {
            assert!(f.bot.allow());
        }
        assert!(!f.bot.allow());
        f.now.fetch_add(3601, Ordering::SeqCst);
        assert!(f.bot.allow());
    }

    #[test]
    fn mcp_rejects_bad_bodies_and_get() {
        let f = fixture();
        for raw in [&b"42"[..], b"\"text\"", b"not json", b"[1]"] {
            assert_eq!(f.bot.handle("POST", "/grok/mcp", AUTH, None, raw).status, 400);
        }
        let response = f.bot.handle("GET", "/grok/mcp", AUTH, None, b"");
        assert_eq!(response.status, 405);
        assert!(response.headers.contains(&("Allow", "POST".into())));
        assert_eq!(f.bot.handle("POST", "/grok/callback", AUTH, None, b"[1]").status, 400);
        let (_, body) = mcp(&f.bot, json!({"jsonrpc": "2.0", "id": 9, "method": "tools/call", "params": "x"}));
        assert_eq!(body["result"]["isError"], true);
    }

    #[test]
    fn mcp_echoes_a_known_protocol_version() {
        let f = fixture();
        let (_, body) = mcp(&f.bot, json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}));
        assert_eq!(body["result"]["protocolVersion"], "2025-06-18");
        let (_, body) = mcp(&f.bot, json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "1999-01-01"}}));
        assert_eq!(body["result"]["protocolVersion"], "2025-03-26");
    }

    #[test]
    fn push_is_queued_for_the_robot_and_rate_limited() {
        let f = fixture();
        let call = |text: &str| {
            mcp(&f.bot, json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
                               "params": {"name": "stackchan_say", "arguments": {"text": text}}})).1
        };
        assert_eq!(call("Le déploiement est fini.")["result"]["content"][0]["text"], "queued");
        assert_eq!(f.bot.take_pending(MAC), vec!["Le déploiement est fini."]);
        for _ in 0..29 {
            call("again");
        }
        assert_eq!(call("one too many")["result"]["isError"], true);
        let (_, body) = mcp(&f.bot, json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "stackchan_say", "arguments": {"text": "x", "device_id": "11:22:33:44:55:66"}}}));
        assert_eq!(body["result"]["isError"], true);
    }

    #[test]
    fn replies_go_only_to_the_robot() {
        let f = fixture();
        let (_, body) = mcp(&f.bot, json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": {"name": "stackchan_reply",
                       "arguments": {"request_id": "r1", "text": "x", "device_id": "11:22:33:44:55:66"}}}));
        assert_eq!(body["result"]["isError"], true);
        assert!(f.bot.take_pending("11:22:33:44:55:66").is_empty());
    }

    #[test]
    fn pending_keeps_the_last_five() {
        let f = fixture();
        for i in 0..7 {
            assert!(f.bot.accept(None, &format!("m{i}"), Some("asleep-device"), Kind::Push).is_ok());
        }
        assert_eq!(f.bot.take_pending("asleep-device"), vec!["m2", "m3", "m4", "m5", "m6"]);
    }

    #[tokio::test]
    async fn webhook_reports_acceptance() {
        let f = fixture();
        assert!(f.bot.post_webhook(json!({"request_id": "req", "question": "status"}), "fr").await.is_ok());
        assert_eq!(f.posts.lock().unwrap()[0]["question"], "status");
    }

    #[tokio::test]
    async fn webhook_errors_are_spoken_in_the_device_language() {
        let f = fixture();
        let bot = |url: &str| {
            GrokBot::new(
                Config {
                    token: "secret-token".into(),
                    fleet: f.fleet.clone(),
                    webhook_url: url.into(),
                    webhook_token: String::new(),
                    default_device: MAC.into(),
                    pending_path: f._dir.path().join("other.json"),
                    rate_per_hour: 10,
                    reply_timeout: Duration::from_secs(DEFAULT_REPLY_TIMEOUT_SECS),
                },
                None,
                Some(Arc::new(|url: String, _payload| {
                    Box::pin(async move { if url.contains("refuses") { Ok(500) } else { Err("connect".into()) } })
                })),
            )
        };
        let payload = || json!({"request_id": "req", "question": "status"});
        assert_eq!(bot("").post_webhook(payload(), "fr").await, Err("Grok Bot n'est pas encore configuré."));
        assert_eq!(bot("").post_webhook(payload(), "en").await, Err("Grok Bot is not set up yet."));
        assert_eq!(bot("https://refuses.invalid").post_webhook(payload(), "fr").await, Err("Grok Bot n'a pas accepté la question."));
        assert_eq!(bot("https://refuses.invalid").post_webhook(payload(), "de").await, Err("Grok Bot did not accept the question."));
        assert_eq!(bot("https://down.invalid").post_webhook(payload(), "fr").await, Err("Je n'arrive pas à joindre Grok Bot."));
        assert_eq!(bot("https://down.invalid").post_webhook(payload(), "en").await, Err("I can't reach Grok Bot."));
    }

    #[test]
    fn speech_is_cleaned_and_bounded() {
        assert_eq!(clean_speech("  **Bonjour**\n`toi` # "), "Bonjour toi");
        assert_eq!(clean_speech(&"é".repeat(600)).chars().count(), 500);
    }

    /// A bot with no `GROK_DEVICE_ID`, on the fixture's fleet.
    fn bot_without_default(f: &Fixture) -> GrokBot {
        let bot = GrokBot::new(
            Config {
                token: "secret-token".into(),
                fleet: f.fleet.clone(),
                webhook_url: "https://example.invalid/hook".into(),
                webhook_token: String::new(),
                default_device: String::new(),
                pending_path: f._dir.path().join("no-default.json"),
                rate_per_hour: 10,
                reply_timeout: Duration::from_secs(DEFAULT_REPLY_TIMEOUT_SECS),
            },
            None,
            None,
        );
        bot.set_speaker(f.recorder.clone());
        bot
    }

    #[test]
    fn an_unset_default_uses_the_only_approved_device() {
        let f = fixture();
        let bot = bot_without_default(&f);
        assert_eq!(bot.resolve("").as_deref(), Ok(MAC));
        let response = bot.handle(
            "POST",
            "/grok/callback",
            AUTH,
            None,
            br#"{"request_id":"no-such","text":"hello from nowhere"}"#,
        );
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(body["delivery"], "spoken");
        assert_eq!(
            *f.recorder.spoken.lock().unwrap(),
            vec![(MAC.to_string(), "hello from nowhere".to_string(), Kind::Reply)]
        );
        assert!(bot.take_pending("unknown").is_empty());
        // A push with no device_id uses that same device (Kind::Push is queued
        // by the test speaker).
        let (_, body) = mcp(
            &bot,
            json!({"jsonrpc": "2.0", "id": 5, "method": "tools/call",
                   "params": {"name": "stackchan_say", "arguments": {"text": "Bonjour."}}}),
        );
        assert_eq!(body["result"]["isError"], false);
        assert_eq!(bot.take_pending(MAC), vec!["Bonjour."]);
    }

    #[test]
    fn a_reply_nobody_will_hear_is_an_error() {
        let f = fixture();
        let bot = bot_without_default(&f);
        f.fleet.authenticate("11:22:33:44:55:66", "boot");
        f.fleet.approve("11:22:33:44:55:66", "kitchen").unwrap();
        assert_eq!(bot.resolve(""), Err("unknown device"));
        let response = bot.handle(
            "POST",
            "/grok/callback",
            AUTH,
            None,
            br#"{"request_id":"no-such","text":"hello from nowhere"}"#,
        );
        let body: Value = serde_json::from_slice(&response.body).unwrap();
        assert_eq!(response.status, 400);
        assert_eq!(body["error"], "unknown request and no default device");
        assert!(f.recorder.spoken.lock().unwrap().is_empty());
        assert!(bot.take_pending("unknown").is_empty());
        assert!(bot.take_pending(MAC).is_empty());
        let (_, mcp_body) = mcp(
            &bot,
            json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
                   "params": {"name": "stackchan_reply", "arguments": {"request_id": "also-unknown", "text": "hi"}}}),
        );
        assert_eq!(mcp_body["result"]["isError"], true);
        assert_eq!(mcp_body["result"]["content"][0]["text"], "unknown request and no default device");
    }

    #[test]
    fn a_known_request_does_not_need_a_default_device() {
        let f = fixture();
        let bot = bot_without_default(&f);
        f.fleet.authenticate("11:22:33:44:55:66", "boot");
        f.fleet.approve("11:22:33:44:55:66", "kitchen").unwrap();
        bot.remember("req-1", MAC);
        assert_eq!(bot.accept(Some("req-1"), "pong", None, Kind::Reply), Ok("spoken"));
        assert_eq!(*f.recorder.spoken.lock().unwrap(), vec![(MAC.to_string(), "pong".to_string(), Kind::Reply)]);
    }

    #[test]
    fn silence_is_announced_once_and_a_late_reply_still_lands() {
        let f = fixture();
        f.bot.remember("req", "device-a");
        assert!(f.bot.claim_silence("req"));
        assert!(!f.bot.claim_silence("req"));
        assert_eq!(f.bot.accept(Some("req"), "late but welcome", None, Kind::Reply), Ok("spoken"));
        assert!(!f.bot.claim_silence("req"));
        assert_eq!(*f.recorder.spoken.lock().unwrap(), vec![("device-a".to_string(), "late but welcome".to_string(), Kind::Reply)]);
        f.bot.remember("gone", MAC);
        f.bot.forget("gone");
        assert!(!f.bot.claim_silence("gone"));
    }
}
