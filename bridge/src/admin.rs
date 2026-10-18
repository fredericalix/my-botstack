//! `/fleet/mcp`: fleet administration for an AI client (docs/fleet.md).
//!
//! Its own token (`FLEET_ADMIN_TOKEN`), separate from Grok Bot's: approve,
//! rename, profiles, locks, reboots, key rotation and revocation, and which
//! firmware each device runs. A device key never appears in an answer: the
//! bridge sends it to the device over the link (`enroll`).

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tracing::{info, warn};

use crate::App;
use crate::fleet::{Device, Live, ProfileChange, Status};
use crate::grokbot::Kind;
use crate::hub::Command;
use crate::mcp::{self, Response, json_response, string_field, tool_result};

const FAILURES: usize = 5;
const FAILURE_WINDOW: Duration = Duration::from_secs(60);
const BLOCKED_FOR: Duration = Duration::from_secs(60);
/// Client addresses tracked at once; past that, new ones share one bucket.
const MAX_CLIENTS: usize = 1024;

pub struct Admin {
    token: String,
    session_id: String,
    failures: Mutex<Lockout>,
}

/// Wrong tokens per client address, so that one client cannot lock everyone
/// out. A request with no known address uses the shared bucket.
#[derive(Default)]
struct Lockout {
    clients: HashMap<String, Failures>,
    shared: Failures,
}

impl Lockout {
    fn bucket(&mut self, client: &str, now: Instant) -> &mut Failures {
        if client.is_empty() {
            return &mut self.shared;
        }
        if !self.clients.contains_key(client) && self.clients.len() >= MAX_CLIENTS {
            self.clients.retain(|_, failures| failures.active(now));
            if self.clients.len() >= MAX_CLIENTS {
                return &mut self.shared;
            }
        }
        self.clients.entry(client.to_string()).or_default()
    }
}

#[derive(Default)]
struct Failures {
    recent: VecDeque<Instant>,
    blocked_until: Option<Instant>,
}

impl Failures {
    /// Still counts: blocked, or a wrong token within the window.
    fn active(&self, now: Instant) -> bool {
        self.blocked_until.is_some_and(|until| now < until)
            || self.recent.back().is_some_and(|at| now.duration_since(*at) < FAILURE_WINDOW)
    }
}

impl Admin {
    pub fn new(token: String) -> Self {
        Self {
            token: token.trim().to_string(),
            session_id: uuid::Uuid::new_v4().simple().to_string(),
            failures: Mutex::new(Lockout::default()),
        }
    }

    pub fn configured(&self) -> bool {
        !self.token.is_empty()
    }

    /// None when the caller may go on, else the HTTP status: 503 no token
    /// configured, 429 too many wrong tokens lately from this `client`
    /// address, 401 wrong token.
    fn gate(&self, client: &str, authorization: Option<&str>) -> Option<u16> {
        if self.token.is_empty() {
            return Some(503);
        }
        let now = Instant::now();
        let mut lockout = self.failures.lock().unwrap();
        let failures = lockout.bucket(client, now);
        if failures.blocked_until.is_some_and(|until| now < until) {
            return Some(429);
        }
        if mcp::bearer_ok(authorization, &self.token) {
            return None;
        }
        failures.recent.retain(|at| now.duration_since(*at) < FAILURE_WINDOW);
        failures.recent.push_back(now);
        if failures.recent.len() >= FAILURES {
            failures.recent.clear();
            failures.blocked_until = Some(now + BLOCKED_FOR);
            warn!(client, "fleet admin: five wrong tokens in a minute; refusing this address for a minute");
        }
        Some(401)
    }
}

/// HTTP entry for `/fleet/mcp`. `client` is the caller's address (empty when
/// unknown), for the wrong-token lockout.
pub fn handle(app: &App, client: &str, method: &str, authorization: Option<&str>, mcp_session: Option<&str>, body: &[u8]) -> Response {
    if let Some(status) = app.admin.gate(client, authorization) {
        let error = match status {
            503 => "fleet administration is not configured (FLEET_ADMIN_TOKEN)",
            429 => "too many wrong tokens; try again in a minute",
            _ => "unauthorized",
        };
        return json_response(status, json!({"error": error}), vec![]);
    }
    if method != "POST" {
        return json_response(405, json!({"error": "method not allowed"}), vec![("Allow", "POST".into())]);
    }
    let session = mcp_session.unwrap_or(&app.admin.session_id);
    let (id, method, params) = match mcp::parse(body) {
        Ok(mcp::Message::Request { id, method, params }) => (id, method, params),
        Ok(mcp::Message::Notification) => return mcp::accepted(session),
        Err(response) => return response,
    };
    let result = match method.as_str() {
        "initialize" => mcp::initialize(&params, "botstack-fleet"),
        "tools/list" => json!({"tools": tools()}),
        "tools/call" => call_tool(app, &params),
        _ => return mcp::method_not_found(id, &method, session),
    };
    mcp::result(id, result, session)
}

fn call_tool(app: &App, params: &Value) -> Value {
    let (tool, arguments) = mcp::call(params);
    // fleet_approve names its device by `mac`.
    let device = match string_field(&arguments, "device") {
        d if d.trim().is_empty() => string_field(&arguments, "mac"),
        d => d,
    };
    match run(app, tool, &arguments) {
        Ok(text) => {
            info!(tool, device, "fleet admin");
            tool_result(&text, false)
        }
        Err(reason) => {
            warn!(tool, device, reason, "fleet admin refused");
            tool_result(&reason, true)
        }
    }
}

fn run(app: &App, tool: &str, arguments: &Value) -> Result<String, String> {
    match tool {
        "fleet_list" => Ok(pretty(&Value::Array(
            app.fleet.entries().iter().map(|(mac, device, live)| summary(app, mac, device, live.as_ref())).collect(),
        ))),
        "fleet_device" => {
            let (mac, device) = find(app, arguments)?;
            Ok(pretty(&detail(app, &mac, &device, app.fleet.live(&mac).as_ref())))
        }
        "fleet_approve" => {
            let mac = string_field(arguments, "mac").trim().to_lowercase();
            let name = string_field(arguments, "name").trim().to_string();
            app.fleet.approve(&mac, &name)?;
            Ok(if app.hub.send(&mac, Command::Approved) {
                format!("{mac} is approved as {name}. It gets its own key now.")
            } else {
                format!("{mac} is approved as {name}. It gets its own key at its next connection.")
            })
        }
        "fleet_rename" => {
            let (mac, device) = approved(app, arguments)?;
            let name = string_field(arguments, "name").trim().to_string();
            app.fleet.rename(&mac, &name)?;
            Ok(format!("{} is now {name}.", label(&mac, &device)))
        }
        "fleet_set_profile" => {
            let (mac, device) = approved(app, arguments)?;
            let change = ProfileChange {
                voice: optional(arguments, "voice"),
                language: optional(arguments, "language"),
                prompt_extra: optional(arguments, "prompt_extra"),
            };
            if change.voice.is_none() && change.language.is_none() && change.prompt_extra.is_none() {
                return Err("Give voice, language or prompt_extra.".into());
            }
            app.fleet.set_profile(&mac, change)?;
            Ok(format!("{}'s profile is changed; it applies from its next conversation.", label(&mac, &device)))
        }
        "fleet_lock" => {
            let locked = arguments["locked"].as_bool().ok_or("locked (true or false) is required.")?;
            let mac = if string_field(arguments, "device").trim() == "all" { None } else { Some(approved(app, arguments)?.0) };
            app.set_locked(mac.as_deref(), locked)?;
            Ok(app.lock_report(mac.as_deref()))
        }
        "fleet_reboot" => {
            // Pending devices too: a reboot is one of the few things they accept.
            let (mac, device) = find(app, arguments)?;
            if !app.hub.is_online(&mac) {
                return Err(format!("{} is offline.", label(&mac, &device)));
            }
            if !app.fleet.has_feature(&mac, "reboot") {
                return Err(format!("{}'s firmware cannot restart on request.", label(&mac, &device)));
            }
            app.hub.send(&mac, Command::Reboot);
            Ok(format!("{} restarts at its next pause between conversations.", label(&mac, &device)))
        }
        "fleet_rotate_key" => {
            let (mac, device) = approved(app, arguments)?;
            if app.hub.is_online(&mac) {
                if !app.fleet.has_feature(&mac, "enroll") {
                    return Err(format!("{}'s firmware has no key of its own (no enroll feature).", label(&mac, &device)));
                }
                // One rotation at a time: two keys on their way could leave the
                // device and the registry on different keys.
                if device.key_pending_sha256.is_some() {
                    return Err(format!("A key is already on its way to {}; wait for it to confirm.", label(&mac, &device)));
                }
                app.hub.send(&mac, Command::Enroll);
                Ok(format!("A new key was sent to {}; its old key works until it confirms.", label(&mac, &device)))
            } else {
                app.fleet.reset_key(&mac)?;
                Ok(format!(
                    "{} is offline: its key is reset. At its next connection its old key is refused once, \
                     it falls back to the bootstrap key and gets a new key.",
                    label(&mac, &device)
                ))
            }
        }
        "fleet_revoke" => {
            let (mac, device) = find(app, arguments)?;
            app.fleet.revoke(&mac)?;
            app.hub.send(&mac, Command::Revoked);
            Ok(format!(
                "{} is revoked and disconnected. Its MAC is refused for 10 minutes, then it can come back as pending.",
                label(&mac, &device)
            ))
        }
        "fleet_firmware" => Ok(pretty(&firmware(app))),
        "fleet_say" => {
            let (mac, device) = approved(app, arguments)?;
            let text = string_field(arguments, "text");
            if text.trim().is_empty() {
                return Err("text is required.".into());
            }
            let state = app.grokbot.accept(None, &text, Some(&mac), Kind::Push)?;
            Ok(format!("{}: {}", label(&mac, &device), state))
        }
        other => Err(format!("Unknown tool {other}.")),
    }
}

/// The device named by `device` (name or MAC), pending ones included.
fn find(app: &App, arguments: &Value) -> Result<(String, Device), String> {
    let reference = string_field(arguments, "device");
    if reference.trim().is_empty() {
        return Err("device (a name or a MAC) is required.".into());
    }
    app.fleet.find(&reference).ok_or_else(|| format!("No device {reference}."))
}

fn approved(app: &App, arguments: &Value) -> Result<(String, Device), String> {
    let (mac, device) = find(app, arguments)?;
    if device.status != Status::Approved {
        return Err(format!("{mac} is waiting for approval (fleet_approve)."));
    }
    Ok((mac, device))
}

fn optional(arguments: &Value, key: &str) -> Option<String> {
    arguments[key].as_str().map(str::to_string)
}

fn label(mac: &str, device: &Device) -> String {
    if device.name.is_empty() { mac.to_string() } else { device.name.clone() }
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

fn summary(app: &App, mac: &str, device: &Device, live: Option<&Live>) -> Value {
    let project = live.map(|l| l.firmware.as_str()).unwrap_or("");
    let build = live.map(|l| l.build.as_str()).unwrap_or("");
    let published = app.firmware.build(project);
    json!({
        "mac": mac,
        "name": device.name,
        "status": device.status,
        "type": project,
        "online": live.is_some_and(|l| l.online),
        "locked": device.locked,
        "build": build,
        "published_build": published,
        "behind": published.as_deref().is_some_and(|p| !build.is_empty() && p != build),
        "minutes_today": live.map(|l| l.seconds_today / 60).unwrap_or(0),
        "key": device.key_state(),
    })
}

fn detail(app: &App, mac: &str, device: &Device, live: Option<&Live>) -> Value {
    let mut value = summary(app, mac, device, live);
    let extra = json!({
        "voice": device.voice,
        "language": device.language,
        "prompt_extra": device.prompt_extra,
        "first_seen": device.first_seen,
        "last_seen": device.last_seen,
        "approved_at": device.approved_at,
        "features": live.map(|l| l.features.clone()).unwrap_or_default(),
        "state": live.map(|l| l.state.clone()).unwrap_or_default(),
        "connected_since": live.filter(|l| l.online).map(|l| l.connected_since),
        "peer": live.map(|l| l.peer.clone()).unwrap_or_default(),
        "health": live.and_then(|l| l.status.clone()),
        "health_at": live.map(|l| l.status_at).filter(|at| *at > 0),
        "minutes_total": live.map(|l| l.seconds_total / 60).unwrap_or(0),
    });
    if let (Some(value), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
        value.extend(extra.clone());
    }
    value
}

fn firmware(app: &App) -> Value {
    let published: Vec<Value> = app
        .firmware
        .published()
        .into_iter()
        .map(|p| json!({"project": p.project, "build": p.build, "bytes": p.bytes, "modified": p.modified}))
        .collect();
    let behind: Vec<Value> = app
        .fleet
        .entries()
        .iter()
        .filter_map(|(mac, device, live)| {
            let live = live.as_ref()?;
            let wanted = app.firmware.build(&live.firmware)?;
            (!live.build.is_empty() && wanted != live.build)
                .then(|| json!({"device": label(mac, device), "type": live.firmware, "running": live.build, "published": wanted}))
        })
        .collect();
    json!({"published": published, "behind": behind})
}

fn tools() -> Value {
    let device = || json!({"type": "string", "description": "A device name, or its MAC."});
    json!([
        {"name": "fleet_list", "description": "Every device, pending ones included: MAC, name, status (pending or approved), type (firmware project), online, locked, running and published build, minutes today, key state.",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "fleet_device", "description": "One device in detail: profile (voice, language, prompt_extra), features, state, health (RSSI, free internal RAM and PSRAM, uptime, last reset reason, head, camera), when it connected, from where.",
         "inputSchema": {"type": "object", "properties": {"device": device()}, "required": ["device"]}},
        {"name": "fleet_approve", "description": "Approve a pending device under a name ([a-z0-9-], unique). It then receives its own key.",
         "inputSchema": {"type": "object", "properties": {"mac": {"type": "string"}, "name": {"type": "string"}}, "required": ["mac", "name"]}},
        {"name": "fleet_rename", "description": "Rename an approved device.",
         "inputSchema": {"type": "object", "properties": {"device": device(), "name": {"type": "string"}}, "required": ["device", "name"]}},
        {"name": "fleet_set_profile", "description": "Change a device's xAI voice (eve, ara, rex, sal, leo…), language (ISO 639-1, such as fr) or prompt_extra (notes added to its instructions, at most 500 characters). Applies from its next conversation.",
         "inputSchema": {"type": "object", "properties": {"device": device(), "voice": {"type": "string"}, "language": {"type": "string"}, "prompt_extra": {"type": "string"}}, "required": ["device"]}},
        {"name": "fleet_lock", "description": "Lock or unlock a device, or every device with device \"all\". While locked nobody can talk to it; messages and photos from Grok Bot still work.",
         "inputSchema": {"type": "object", "properties": {"device": device(), "locked": {"type": "boolean"}}, "required": ["device", "locked"]}},
        {"name": "fleet_reboot", "description": "Restart a connected device at its next pause between conversations.",
         "inputSchema": {"type": "object", "properties": {"device": device()}, "required": ["device"]}},
        {"name": "fleet_rotate_key", "description": "Give a device a new key. Connected: sent now, the old key works until it confirms (refused while a key is still on its way). Offline: its key is reset, and it gets a new one at its next connection.",
         "inputSchema": {"type": "object", "properties": {"device": device()}, "required": ["device"]}},
        {"name": "fleet_revoke", "description": "Remove a device and disconnect it. Its MAC is refused for 10 minutes, then it can come back as pending.",
         "inputSchema": {"type": "object", "properties": {"device": device()}, "required": ["device"]}},
        {"name": "fleet_firmware", "description": "The published firmware images per project (build, size, date) and the devices running another build.",
         "inputSchema": {"type": "object", "properties": {}}},
        {"name": "fleet_say", "description": "Make a device say a text now (or at its next connection), to test it without Grok Bot.",
         "inputSchema": {"type": "object", "properties": {"device": device(), "text": {"type": "string"}}, "required": ["device", "text"]}}
    ])
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{ADMIN, BOOTSTRAP, MAC};

    const OTHER: &str = "aa:bb:cc:dd:ee:01";

    fn body(id: u64, method: &str, params: Value) -> Vec<u8> {
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string().into_bytes()
    }

    /// (is_error, text) of a tool call.
    fn call(app: &App, tool: &str, arguments: Value) -> (bool, String) {
        let auth = format!("Bearer {ADMIN}");
        let response = handle(app, "", "POST", Some(&auth), None, &body(1, "tools/call", json!({"name": tool, "arguments": arguments})));
        assert_eq!(response.status, 200);
        let answer: Value = serde_json::from_slice(&response.body).unwrap();
        let result = &answer["result"];
        (result["isError"] == true, result["content"][0]["text"].as_str().unwrap_or("").to_string())
    }

    #[test]
    fn the_token_gates_everything() {
        let dir = tempfile::tempdir().unwrap();
        let app = crate::testing::app(dir.path());
        let list = body(1, "tools/list", Value::Null);
        // A missing token counts as a wrong one: 1 + 4 = 5.
        assert_eq!(handle(&app, "", "POST", None, None, &list).status, 401);
        for _ in 0..4 {
            assert_eq!(handle(&app, "", "POST", Some("Bearer wrong"), None, &list).status, 401);
        }
        // Five wrong tokens: that address waits a minute, the right token too.
        assert_eq!(handle(&app, "", "POST", Some(&format!("Bearer {ADMIN}")), None, &list).status, 429);
        let closed = Admin::new(String::new());
        assert!(!closed.configured());
        assert_eq!(closed.gate("", Some("Bearer ")), Some(503));
    }

    #[test]
    fn one_address_locked_out_does_not_lock_out_another() {
        let dir = tempfile::tempdir().unwrap();
        let app = crate::testing::app(dir.path());
        let list = body(1, "tools/list", Value::Null);
        let good = format!("Bearer {ADMIN}");
        for _ in 0..5 {
            assert_eq!(handle(&app, "203.0.113.7", "POST", Some("Bearer wrong"), None, &list).status, 401);
        }
        assert_eq!(handle(&app, "203.0.113.7", "POST", Some(&good), None, &list).status, 429);
        assert_eq!(handle(&app, "198.51.100.2", "POST", Some(&good), None, &list).status, 200);
        // Wrong tokens from elsewhere do not add up with the first address's.
        for _ in 0..4 {
            assert_eq!(handle(&app, "198.51.100.2", "POST", Some("Bearer wrong"), None, &list).status, 401);
        }
        assert_eq!(handle(&app, "198.51.100.2", "POST", Some(&good), None, &list).status, 200);
        // The shared bucket (no known address) is separate too.
        assert_eq!(handle(&app, "", "POST", Some(&good), None, &list).status, 200);
    }

    #[test]
    fn the_lockout_map_is_bounded() {
        let admin = Admin::new(ADMIN.into());
        for n in 0..MAX_CLIENTS {
            assert_eq!(admin.gate(&format!("10.0.{}.{}", n / 256, n % 256), Some("Bearer wrong")), Some(401));
        }
        assert_eq!(admin.failures.lock().unwrap().clients.len(), MAX_CLIENTS);
        // Full of recent failures: a new address falls back to the shared bucket.
        for _ in 0..5 {
            assert_eq!(admin.gate("192.0.2.1", Some("Bearer wrong")), Some(401));
        }
        assert_eq!(admin.gate("192.0.2.2", Some(&format!("Bearer {ADMIN}"))), Some(429));
        assert_eq!(admin.gate("", Some(&format!("Bearer {ADMIN}"))), Some(429));
        // A tracked address keeps its own bucket.
        assert_eq!(admin.gate("10.0.0.1", Some(&format!("Bearer {ADMIN}"))), None);
        // Stale entries are evicted to make room.
        {
            let mut lockout = admin.failures.lock().unwrap();
            let old = Instant::now() - FAILURE_WINDOW - Duration::from_secs(1);
            for failures in lockout.clients.values_mut() {
                failures.recent = VecDeque::from([old]);
            }
            let now = Instant::now();
            lockout.bucket("192.0.2.3", now);
            assert_eq!(lockout.clients.len(), 1);
            assert!(lockout.clients.contains_key("192.0.2.3"));
        }
    }

    #[test]
    fn lists_its_tools() {
        let dir = tempfile::tempdir().unwrap();
        let app = crate::testing::app(dir.path());
        let response = handle(&app, "", "POST", Some(&format!("Bearer {ADMIN}")), None, &body(1, "tools/list", Value::Null));
        let answer: Value = serde_json::from_slice(&response.body).unwrap();
        let names: Vec<&str> = answer["result"]["tools"].as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert_eq!(
            names,
            ["fleet_list", "fleet_device", "fleet_approve", "fleet_rename", "fleet_set_profile", "fleet_lock",
             "fleet_reboot", "fleet_rotate_key", "fleet_revoke", "fleet_firmware", "fleet_say"]
        );
    }

    #[test]
    fn approves_a_pending_device() {
        let dir = tempfile::tempdir().unwrap();
        let app = crate::testing::app(dir.path());
        app.fleet.authenticate(OTHER, BOOTSTRAP);
        let (error, text) = call(&app, "fleet_list", json!({}));
        assert!(!error);
        let list: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(list[1]["status"], "pending");
        let (error, text) = call(&app, "fleet_approve", json!({"mac": OTHER, "name": "cuisine"}));
        assert!(!error, "{text}");
        assert!(text.contains("next connection"));
        let (_, text) = call(&app, "fleet_device", json!({"device": "cuisine"}));
        let device: Value = serde_json::from_str(&text).unwrap();
        assert_eq!((device["status"].as_str(), device["key"].as_str()), (Some("approved"), Some("bootstrap")));
        assert!(call(&app, "fleet_approve", json!({"mac": OTHER, "name": "cuisine"})).0);
    }

    #[test]
    fn no_answer_ever_holds_a_key() {
        let dir = tempfile::tempdir().unwrap();
        let app = crate::testing::app(dir.path());
        let key = app.fleet.issue_key(MAC).unwrap();
        let hash = crate::fleet::key_hash(&key);
        for (tool, arguments) in [
            ("fleet_list", json!({})),
            ("fleet_device", json!({"device": "stackchan"})),
            ("fleet_firmware", json!({})),
            ("fleet_rotate_key", json!({"device": "stackchan"})),
        ] {
            let (_, text) = call(&app, tool, arguments);
            assert!(!text.contains(&key) && !text.contains(&hash), "{tool} leaks a key");
        }
    }

    #[test]
    fn locks_and_unlocks() {
        let dir = tempfile::tempdir().unwrap();
        let app = crate::testing::app(dir.path());
        assert!(call(&app, "fleet_lock", json!({"device": "all"})).0);
        let (error, text) = call(&app, "fleet_lock", json!({"device": "all", "locked": true}));
        assert!(!error && text.contains("stackchan is locked."));
        assert!(app.fleet.locked(MAC));
        call(&app, "fleet_lock", json!({"device": "stackchan", "locked": false}));
        assert!(!app.fleet.locked(MAC));
    }

    #[test]
    fn rotating_an_offline_key_resets_it() {
        let dir = tempfile::tempdir().unwrap();
        let app = crate::testing::app(dir.path());
        let key = app.fleet.issue_key(MAC).unwrap();
        app.fleet.confirm_key(MAC, &crate::fleet::key_id(&key));
        let (error, text) = call(&app, "fleet_rotate_key", json!({"device": "stackchan"}));
        assert!(!error && text.contains("offline"));
        assert_eq!(app.fleet.device(MAC).unwrap().key_state(), "bootstrap");
    }

    #[test]
    fn one_rotation_at_a_time_for_a_connected_device() {
        let dir = tempfile::tempdir().unwrap();
        let app = crate::testing::app(dir.path());
        let (sender, mut commands) = tokio::sync::mpsc::unbounded_channel();
        app.hub.register(MAC, sender);
        let hello = json!({"type": "hello", "firmware": "botstack-fw", "features": ["enroll"]});
        app.fleet.connected(MAC, 1, "", &hello);
        let key = app.fleet.issue_key(MAC).unwrap();
        app.fleet.confirm_key(MAC, &crate::fleet::key_id(&key));
        let (error, text) = call(&app, "fleet_rotate_key", json!({"device": "stackchan"}));
        assert!(!error, "{text}");
        assert!(matches!(commands.try_recv(), Ok(Command::Enroll)));
        // The link issues the key; until the device confirms it, no second one.
        app.fleet.issue_key(MAC).unwrap();
        let (error, text) = call(&app, "fleet_rotate_key", json!({"device": "stackchan"}));
        assert!(error && text.contains("already on its way to stackchan"), "{text}");
        assert!(commands.try_recv().is_err());
    }

    #[test]
    fn revokes_and_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let app = crate::testing::app(dir.path());
        assert!(!call(&app, "fleet_revoke", json!({"device": "stackchan"})).0);
        assert_eq!(app.fleet.authenticate(MAC, BOOTSTRAP), crate::fleet::Auth::Refused(403));
        assert!(call(&app, "fleet_device", json!({"device": "stackchan"})).0);
    }

    #[test]
    fn checks_profiles_and_names() {
        let dir = tempfile::tempdir().unwrap();
        let app = crate::testing::app(dir.path());
        assert!(call(&app, "fleet_set_profile", json!({"device": "stackchan"})).0);
        assert!(call(&app, "fleet_set_profile", json!({"device": "stackchan", "language": "français"})).0);
        assert!(!call(&app, "fleet_set_profile", json!({"device": "stackchan", "voice": "ara", "prompt_extra": "Bureau."})).0);
        assert_eq!(app.fleet.device(MAC).unwrap().voice, "ara");
        assert!(!call(&app, "fleet_rename", json!({"device": "stackchan", "name": "bureau"})).0);
        assert!(call(&app, "fleet_device", json!({"device": "garage"})).0);
    }

    #[test]
    fn say_queues_for_an_offline_device_and_reboot_needs_it_online() {
        let dir = tempfile::tempdir().unwrap();
        let app = crate::testing::app(dir.path());
        let (error, text) = call(&app, "fleet_say", json!({"device": "stackchan", "text": "Test."}));
        assert!(!error && text.ends_with("queued"));
        assert_eq!(app.grokbot.take_pending(MAC), vec!["Test."]);
        assert!(call(&app, "fleet_reboot", json!({"device": "stackchan"})).0);
    }
}
