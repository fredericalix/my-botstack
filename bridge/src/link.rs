//! botstack/v1: one always-on connection per robot (see docs/protocol.md).
//!
//! The WebSocket stays open while the robot is powered. Conversations with
//! Grok (xAI realtime sessions) open and close inside it; between them the
//! bridge can still make the robot speak (pushed messages, late Grok Bot
//! replies) and change its face. The bridge drives the robot's state: the
//! microphone streams in `listening` and `thinking`, and in `speaking` while a
//! conversation is open, so the user can talk over the robot (barge-in).
//!
//! Voice packets are paced at playback speed, never more than four ahead;
//! verbatim speech goes through xAI TTS; xAI stalls are retried and
//! apologised for.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message, WebSocket};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::{App, say};
use crate::audio::{self, Downlink, FRAME_MS, Uplink};
use crate::fleet::Status;
use crate::grokbot::Kind;
use crate::hub::Command;
use crate::realtime::{ANIMATIONS, EXPRESSIONS, Profile, Realtime};
use crate::slots::Slot;
use crate::speech;

pub const PROTOCOL: &str = "botstack/v1";

const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const TICK: Duration = Duration::from_millis(20);
/// How often to look for a newly published firmware.
const FIRMWARE_CHECK: Duration = Duration::from_secs(30);
const FRAME: Duration = Duration::from_millis(FRAME_MS as u64);
/// Voice sent ahead of playback: a cushion far under the robot's queue.
const AHEAD: Duration = Duration::from_millis(4 * FRAME_MS as u64);
/// Delay between `speech start` and the first packet.
const START_GAP: Duration = Duration::from_millis(60);
/// A reply is over when no audio came for this long and xAI said done.
const QUIET: Duration = Duration::from_millis(400);
/// After `speech end`, assume playback is over if the robot stays silent this long.
const PLAYED_FALLBACK: Duration = Duration::from_secs(4);
/// Microphone audio kept while xAI connects (60 ms packets): about 19 s.
const EARLY_MIC_PACKETS: usize = 320;
/// No first audio this long after xAI started a response: apologise.
const STALL: Duration = Duration::from_secs(25);
/// After a pushed message, listen this long for an answer.
const PUSH_LISTEN: Duration = Duration::from_secs(10);
/// After the goodbye is played, close once quiet this long.
const CLOSE_AFTER_GOODBYE: Duration = Duration::from_secs(2);
/// The robot pings every 5 s; a connection silent this long is dead.
const SILENT_DROP: Duration = Duration::from_secs(75);
const PHOTO_TIMEOUT: Duration = Duration::from_secs(30);
/// The robot answers a head move as soon as it has taken it (or cannot).
const HEAD_TIMEOUT: Duration = Duration::from_secs(3);
const CONNECT_ATTEMPTS: u32 = 2;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(8);
/// The face while the robot is locked.
const LOCKED_FACE: &str = "mefiant";
/// The face of a device waiting for approval.
const PENDING_FACE: &str = "confus";

/// Said when xAI stalls.
fn apology(language: &str) -> &'static str {
    say(language, "Désolé, je n'arrive pas à répondre pour l'instant. Tu peux répéter ?", "Sorry, I can't answer right now. Can you say that again?")
}

/// Said when a Grok Bot routine accepts the question and then never calls
/// `stackchan_reply`. The bridge's own line: `keep` is false, so a drop
/// during playback does not write it to `grok_pending.json`, and a link that
/// is already gone drops it too.
fn grok_bot_silent(language: &str) -> &'static str {
    say(language, "Grok Bot n'a pas répondu.", "Grok Bot didn't answer.")
}

/// Said when MAX_CONVERSATIONS are open.
fn busy_text(language: &str) -> &'static str {
    say(language, "Je suis occupé, réessaie dans un moment.", "I'm busy, try again in a moment.")
}

type Connecting = JoinHandle<anyhow::Result<(Realtime, mpsc::UnboundedReceiver<Value>)>>;

pub async fn run(app: Arc<App>, socket: WebSocket, device_id: String, peer: String) {
    let (mut sink, mut stream) = socket.split();
    let (out, mut outgoing) = mpsc::unbounded_channel::<Message>();
    tokio::spawn(async move {
        while let Some(message) = outgoing.recv().await {
            let closing = matches!(message, Message::Close(_));
            if sink.send(message).await.is_err() || closing {
                break;
            }
        }
    });

    let robot_hello = tokio::time::timeout(HELLO_TIMEOUT, async {
        while let Some(Ok(message)) = stream.next().await {
            if let Message::Text(text) = message
                && let Ok(value) = serde_json::from_str::<Value>(&text)
                    && value["type"] == "hello" {
                        return Some(value);
                    }
        }
        None
    })
    .await
    .ok()
    .flatten();
    let Some(robot_hello) = robot_hello.filter(|h| h["protocol"] == PROTOCOL) else {
        warn!(device = %device_id, "no botstack/v1 hello");
        let _ = out.send(Message::Close(None));
        return;
    };
    let bridge_hello = hello(&app, &device_id);
    let approved = bridge_hello["approved"] == true;
    let name = bridge_hello["name"].as_str().unwrap_or("");
    info!(
        device = %device_id,
        name,
        approved,
        firmware = robot_hello["firmware"].as_str().unwrap_or(""),
        build = robot_hello["build"].as_str().unwrap_or(""),
        "robot connected"
    );
    let _ = out.send(Message::Text(bridge_hello.to_string().into()));

    let (commands_tx, mut commands) = mpsc::unbounded_channel::<Command>();
    let generation = app.hub.register(&device_id, commands_tx.clone());
    app.fleet.connected(&device_id, generation, &peer, &robot_hello);
    let mut link = match Link::new(app.clone(), out.clone(), device_id.clone(), commands_tx) {
        Ok(link) => link,
        Err(error) => {
            warn!(%error, "audio setup failed");
            app.hub.unregister(&device_id, generation);
            app.fleet.disconnected(&device_id, generation);
            let _ = out.send(Message::Close(None));
            return;
        }
    };
    link.approved = approved;
    link.project = app.fleet.live(&device_id).map(|live| live.firmware).unwrap_or_default();
    link.set_state("idle");
    if !approved {
        info!(device = %device_id, "waiting for approval (fleet_approve)");
        link.send(json!({"type": "expression", "value": PENDING_FACE}));
    } else if app.fleet.locked(&device_id) {
        link.send(json!({"type": "expression", "value": LOCKED_FACE}));
    }
    link.robot_build = robot_hello["build"].as_str().unwrap_or("").to_string();
    link.offer_firmware();
    if approved {
        link.enroll_if_needed();
        // Messages that waited while the robot was offline.
        let pending = app.grokbot.take_pending(&device_id);
        if !pending.is_empty() {
            info!(device = %device_id, count = pending.len(), "delivering messages queued while offline");
            link.push(pending);
        }
    }

    let mut connecting: Option<Connecting> = None;
    let mut events: Option<mpsc::UnboundedReceiver<Value>> = None;
    let mut last_rx = Instant::now();
    let mut tick = tokio::time::interval(TICK);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        match link.realtime_request.take() {
            Some(true) if connecting.is_none() && events.is_none() => {
                connecting = Some(spawn_connect(app.clone(), device_id.clone(), link.profile()));
            }
            Some(false) => {
                if let Some(task) = connecting.take() {
                    task.abort();
                }
                events = None;
            }
            _ => {}
        }

        tokio::select! {
            message = stream.next() => {
                last_rx = Instant::now();
                match message {
                    Some(Ok(Message::Binary(packet))) => link.microphone(&packet),
                    Some(Ok(Message::Text(text))) => link.robot_text(&text),
                    Some(Ok(Message::Close(_))) | None | Some(Err(_)) => {
                        info!(device = %device_id, "robot disconnected");
                        break;
                    }
                    Some(Ok(_)) => {}
                }
            },
            result = async { connecting.as_mut().unwrap().await }, if connecting.is_some() => {
                connecting = None;
                match result {
                    Ok(Ok((realtime, receiver))) => {
                        if link.conversation_open() {
                            events = Some(receiver);
                            link.realtime_ready(realtime);
                        } else {
                            realtime.close();
                        }
                    }
                    Ok(Err(error)) => {
                        warn!(device = %device_id, %error, "xAI realtime unavailable");
                        link.realtime_failed();
                    }
                    Err(error) => {
                        warn!(device = %device_id, %error, "xAI realtime connect task failed");
                        link.realtime_failed();
                    }
                }
            },
            event = async { events.as_mut().unwrap().recv().await }, if events.is_some() => match event {
                Some(event) => link.realtime_event(event),
                None => {
                    events = None;
                    link.realtime_failed();
                }
            },
            Some(command) = commands.recv() => {
                match command {
                    Command::Replaced => {
                        info!(device = %device_id, "replaced by a newer connection");
                        break;
                    }
                    Command::Revoked => {
                        info!(device = %device_id, "revoked; closing the connection");
                        break;
                    }
                    command => link.command(command),
                }
            }
            _ = tick.tick() => {
                link.tick();
                if last_rx.elapsed() > SILENT_DROP {
                    warn!(device = %device_id, "robot silent; dropping the connection");
                    break;
                }
            }
        }
    }

    if let Some(task) = connecting.take() {
        task.abort();
    }
    app.hub.unregister(&device_id, generation);
    app.fleet.disconnected(&device_id, generation);
    link.finish();
    let _ = out.send(Message::Close(None));
}

/// The bridge's hello: the device's name and whether it is approved. Only an
/// approved device gets the vision URL and token: a pending one could post
/// photos under another device's MAC with it. At approval it gets a second hello.
fn hello(app: &App, device_id: &str) -> Value {
    let device = app.fleet.device(device_id);
    let approved = device.as_ref().is_some_and(|d| d.status == Status::Approved);
    let mut hello = json!({
        "type": "hello",
        "protocol": PROTOCOL,
        "name": device.map(|d| d.name).unwrap_or_default(),
        "approved": approved,
        "voice": {"codec": "opus", "rate": audio::DOWNLINK_RATE, "frame_ms": FRAME_MS},
    });
    if approved {
        hello["vision"] = json!({"url": app.vision.url, "token": app.vision.token});
    }
    hello
}

fn spawn_connect(app: Arc<App>, device_id: String, profile: Profile) -> Connecting {
    tokio::spawn(async move {
        // xAI sometimes never confirms a new session; a second try usually works.
        let mut last = anyhow::anyhow!("not tried");
        for attempt in 1..=CONNECT_ATTEMPTS {
            match tokio::time::timeout(CONNECT_TIMEOUT, Realtime::connect(&app.realtime, &profile)).await {
                Ok(Ok(connected)) => return Ok(connected),
                Ok(Err(error)) => last = error,
                Err(_) => last = anyhow::anyhow!("no session within {} s", CONNECT_TIMEOUT.as_secs()),
            }
            warn!(device = %device_id, attempt, error = %last, "xAI realtime connect failed");
        }
        Err(last)
    })
}

/// One Grok conversation inside the connection.
struct Conversation {
    started: Instant,
    last_activity: Instant,
    response_active: bool,
    awaiting_since: Option<Instant>,
    apologised: bool,
    /// After a tap: drop the rest of the interrupted model audio until the next turn.
    muted: bool,
    /// end_conversation was called: close after the goodbye.
    end_after_reply: bool,
    /// The current response has already played some voice (a goodbye said
    /// before end_conversation must not be asked for again).
    spoke: bool,
    closing: bool,
    /// Opened after a pushed message: close if nobody speaks before this.
    listen_until: Option<Instant>,
    /// Grok turned the head: center it when the conversation closes.
    head_moved: bool,
    /// Its place under MAX_CONVERSATIONS, given back when the conversation is dropped.
    _slot: Slot,
}

struct Link {
    app: Arc<App>,
    out: mpsc::UnboundedSender<Message>,
    commands: mpsc::UnboundedSender<Command>,
    device_id: String,
    /// Approved in the registry; a pending device only gets its face, firmware offers and reboot.
    approved: bool,
    /// The firmware project from its hello (its hardware type).
    project: String,
    uplink: Uplink,
    downlink: Downlink,
    state: &'static str,
    mic: bool,

    conversation: Option<Conversation>,
    realtime: Option<Realtime>,
    /// Some(true): open an xAI session; Some(false): drop it. Read by `run`.
    realtime_request: Option<bool>,
    early_mic: VecDeque<Vec<u8>>,

    queue: VecDeque<Vec<u8>>,
    /// Between `speech start` and the robot's `played`.
    speaking: bool,
    speech_ended: bool,
    played_fallback: Option<Instant>,
    first_packet_at: Instant,
    play_clock: Instant,
    last_audio: Instant,

    /// Bumped by an interrupt, so verbatim speech still being synthesised is dropped.
    epoch: u64,
    synthesising: usize,
    deferred: Vec<(Vec<i16>, Vec<String>)>,
    unspoken: Vec<String>,
    notes: Vec<String>,
    held_pushes: Vec<String>,
    listen_after_speech: bool,

    next_tool_id: u64,
    tools_pending: HashMap<u64, (String, Instant)>,
    /// Grok Bot photos asked of the robot: tool id → (photo key, give up at).
    photo_requests: HashMap<u64, (String, Instant)>,

    /// The firmware build the robot runs, from its hello.
    robot_build: String,
    /// The published firmware last offered to the robot, and when to look again.
    firmware_offered: Option<String>,
    firmware_check: Instant,
}

impl Link {
    fn new(app: Arc<App>, out: mpsc::UnboundedSender<Message>, device_id: String, commands: mpsc::UnboundedSender<Command>) -> anyhow::Result<Self> {
        let now = Instant::now();
        Ok(Self {
            downlink: Downlink::new(&app.voice_shape)?,
            uplink: Uplink::new()?,
            app,
            out,
            commands,
            device_id,
            approved: false,
            project: String::new(),
            state: "",
            mic: false,
            conversation: None,
            realtime: None,
            realtime_request: None,
            early_mic: VecDeque::new(),
            queue: VecDeque::new(),
            speaking: false,
            speech_ended: false,
            played_fallback: None,
            first_packet_at: now,
            play_clock: now,
            last_audio: now,
            epoch: 0,
            synthesising: 0,
            deferred: Vec::new(),
            unspoken: Vec::new(),
            notes: Vec::new(),
            held_pushes: Vec::new(),
            listen_after_speech: false,
            next_tool_id: 1,
            tools_pending: HashMap::new(),
            photo_requests: HashMap::new(),
            robot_build: String::new(),
            firmware_offered: None,
            firmware_check: now + FIRMWARE_CHECK,
        })
    }

    /// Offer a newly published firmware (docs/firmware.md#updates). The robot
    /// installs it once idle, unless it already runs it or rolled it back.
    fn offer_firmware(&mut self) {
        let build = self.app.firmware.build(&self.project);
        if let Some(build) = build.as_deref()
            && self.firmware_offered.as_deref() != Some(build)
        {
            if build != self.robot_build {
                info!(device = %self.device_id, from = %self.robot_build, to = build, "firmware update offered");
            }
            self.send(json!({"type": "firmware", "build": build, "url": self.app.firmware.url}));
        }
        self.firmware_offered = build;
    }

    fn has(&self, feature: &str) -> bool {
        self.app.fleet.has_feature(&self.device_id, feature)
    }

    /// Send the device a fresh key of its own (docs/fleet.md). The bridge
    /// keeps only its hash; the key is never logged.
    fn enroll(&mut self) {
        if !self.approved || !self.has("enroll") {
            return;
        }
        match self.app.fleet.issue_key(&self.device_id) {
            Ok(key) => {
                let id = crate::fleet::key_id(&key);
                info!(device = %self.device_id, id, "device key sent");
                self.send(json!({"type": "enroll", "key": key, "id": id}));
            }
            Err(reason) => warn!(device = %self.device_id, reason, "no device key"),
        }
    }

    /// A device without a confirmed key, or with one not confirmed, gets a new one.
    fn enroll_if_needed(&mut self) {
        if self.app.fleet.needs_key(&self.device_id) {
            self.enroll();
        }
    }

    fn send(&self, value: Value) {
        let _ = self.out.send(Message::Text(value.to_string().into()));
    }

    /// The robot's state. While speaking, the microphone stays on if a
    /// conversation is open: xAI hears the user over the robot (the robot
    /// cancels its own echo) and the bridge stops the robot when they talk.
    fn set_state(&mut self, state: &'static str) {
        let mic = match state {
            "listening" | "thinking" => true,
            "speaking" => self.conversation.is_some(),
            _ => false,
        };
        if self.state != state || self.mic != mic {
            self.state = state;
            self.mic = mic;
            self.send(json!({"type": "state", "value": state, "mic": mic}));
            self.app.fleet.set_state(&self.device_id, state);
        }
    }

    /// A one-shot bloub animation on the robot's face.
    fn animate(&self, name: &str) {
        self.send(json!({"type": "animation", "value": name}));
    }

    fn conversation_open(&self) -> bool {
        self.conversation.is_some()
    }

    /// Who the realtime session and verbatim speech speak for.
    fn profile(&self) -> Profile {
        let device = self.app.fleet.device(&self.device_id);
        Profile {
            name: device.as_ref().map(|d| d.name.clone()).unwrap_or_default(),
            voice: device.as_ref().map(|d| d.voice.clone()).unwrap_or_else(|| self.app.realtime.voice.clone()),
            language: device.as_ref().map(|d| d.language.clone()).unwrap_or_else(|| self.app.fleet.default_language().to_string()),
            prompt_extra: device.map(|d| d.prompt_extra).unwrap_or_default(),
            head: self.has("head"),
            camera: self.has("camera"),
        }
    }

    // ----- conversations -----

    fn open_conversation(&mut self, slot: Slot, listen_for: Option<Duration>) {
        if self.conversation.is_some() {
            return;
        }
        let now = Instant::now();
        info!(device = %self.device_id, short = listen_for.is_some(), "conversation opened");
        self.conversation = Some(Conversation {
            started: now,
            last_activity: now,
            response_active: false,
            awaiting_since: None,
            apologised: false,
            muted: false,
            end_after_reply: false,
            spoke: false,
            closing: false,
            listen_until: listen_for.map(|d| now + d),
            head_moved: false,
            _slot: slot,
        });
        self.realtime_request = Some(true);
        if self.speaking {
            self.set_state("speaking");
        } else {
            self.set_state("listening");
        }
    }

    fn close_conversation(&mut self, reason: &str) {
        let Some(conversation) = self.conversation.take() else {
            return;
        };
        let seconds = conversation.started.elapsed().as_secs();
        self.app.add_realtime_seconds(&self.device_id, seconds);
        info!(device = %self.device_id, reason, seconds, "conversation closed");
        if let Some(realtime) = self.realtime.take() {
            realtime.close();
        }
        self.realtime_request = Some(false);
        self.early_mic.clear();
        self.tools_pending.clear();
        if conversation.head_moved {
            self.robot_tool("head.set_angles", json!({"yaw": 0, "pitch": 0, "speed": 250}), None);
        }
        if self.speaking {
            self.set_state("speaking");
        } else {
            self.set_state("idle");
        }
        match reason {
            "goodbye" => self.animate("wink"),
            "idle" | "no answer to the push" => self.animate("sleep"),
            _ => {}
        }
        // Messages that waited for the end of the conversation.
        if !self.held_pushes.is_empty() {
            let pushes = std::mem::take(&mut self.held_pushes);
            self.push(pushes);
        }
    }

    fn realtime_ready(&mut self, realtime: Realtime) {
        for pcm in self.early_mic.drain(..) {
            realtime.append(&pcm);
        }
        for note in self.notes.drain(..) {
            realtime.note(&note);
        }
        self.realtime = Some(realtime);
        if let Some(conversation) = &mut self.conversation {
            conversation.last_activity = Instant::now();
        }
    }

    fn realtime_failed(&mut self) {
        if self.conversation.is_some() {
            self.animate("alert");
            self.send(json!({"type": "expression", "value": "triste"}));
            self.close_conversation("xAI unavailable");
        }
    }

    /// MAX_CONVERSATIONS are open: say so, then back to idle (after `played`).
    fn busy(&mut self) {
        warn!(device = %self.device_id, active = self.app.slots.active(), "conversation refused: the cap is reached");
        self.send(json!({"type": "expression", "value": "triste"}));
        let text = busy_text(&self.profile().language);
        self.speak_verbatim(vec![text.to_string()], false, false);
    }

    // ----- robot -----

    fn microphone(&mut self, packet: &[u8]) {
        if packet.is_empty() || self.conversation.is_none() {
            return;
        }
        let pcm = match self.uplink.decode(packet) {
            Ok(pcm) => pcm,
            Err(error) => {
                debug!(%error, "uplink packet skipped");
                return;
            }
        };
        match &self.realtime {
            Some(realtime) => realtime.append(&pcm),
            None => {
                if self.early_mic.len() == EARLY_MIC_PACKETS {
                    self.early_mic.pop_front();
                }
                self.early_mic.push_back(pcm);
            }
        }
    }

    fn robot_text(&mut self, text: &str) {
        let Ok(message) = serde_json::from_str::<Value>(text) else {
            return;
        };
        match message["type"].as_str() {
            Some("talk") => match message["action"].as_str() {
                Some("start") if !self.approved => {
                    info!(device = %self.device_id, "talk refused: waiting for approval");
                    self.animate("alert");
                }
                Some("start") if self.app.fleet.locked(&self.device_id) => {
                    info!(device = %self.device_id, "talk refused: locked");
                    self.animate("alert");
                }
                Some("start") => {
                    if self.speaking {
                        self.interrupt("tap");
                    }
                    self.listen_after_speech = false;
                    if self.conversation.is_none() {
                        match self.app.slots.try_take() {
                            Some(slot) => {
                                self.animate("wake");
                                self.open_conversation(slot, None);
                            }
                            None => self.busy(),
                        }
                    }
                }
                Some("stop") => {
                    // The robot may still be playing the end of a reply: silence it too.
                    self.interrupt("tap");
                    self.close_conversation("tap");
                }
                _ => {}
            },
            Some("interrupt") => self.interrupt("tap"),
            Some("played") => self.played(),
            Some("tool_result") => self.tool_result(&message),
            Some("enrolled") => {
                let id = message["id"].as_str().unwrap_or("");
                let shown: String = id.chars().take(8).collect();
                if self.app.fleet.confirm_key(&self.device_id, id) {
                    info!(device = %self.device_id, id = %shown, "device key confirmed");
                } else {
                    warn!(device = %self.device_id, id = %shown, "enrolled for a key that does not wait");
                }
            }
            Some("status") => self.app.fleet.set_status(&self.device_id, &message),
            Some(kind) => debug!(kind, "robot message ignored"),
            None => {}
        }
    }

    /// Stop the robot's speech: a tap (`interrupt`) or the user talking over it.
    fn interrupt(&mut self, reason: &str) {
        if !self.speaking {
            return;
        }
        info!(device = %self.device_id, reason, "speech interrupted");
        if let Some(conversation) = &mut self.conversation {
            if conversation.response_active
                && let Some(realtime) = &self.realtime {
                    realtime.cancel();
                }
            conversation.muted = true;
            conversation.response_active = false;
            conversation.awaiting_since = None;
        }
        // A tap skips whatever was being said, verbatim speech included.
        self.epoch += 1;
        self.deferred.clear();
        self.unspoken.clear();
        self.queue.clear();
        self.downlink.clear();
        // The robot drops what it still holds, then answers `played`.
        self.send(json!({"type": "speech", "action": "cancel"}));
        self.speech_ended = true;
        self.played_fallback = Some(Instant::now() + PLAYED_FALLBACK);
    }

    fn played(&mut self) {
        if !self.speaking {
            return;
        }
        self.speaking = false;
        self.speech_ended = false;
        self.played_fallback = None;
        let now = Instant::now();
        if let Some(conversation) = &mut self.conversation {
            conversation.last_activity = now;
            self.set_state("listening");
        } else if std::mem::take(&mut self.listen_after_speech) && !self.app.fleet.locked(&self.device_id) && self.approved {
            // A pushed message: let the user answer (not while locked, nor over the cap).
            match self.app.slots.try_take() {
                Some(slot) => self.open_conversation(slot, Some(PUSH_LISTEN)),
                None => self.set_state("idle"),
            }
        } else {
            self.set_state("idle");
        }
    }

    // ----- xAI realtime -----

    fn realtime_event(&mut self, event: Value) {
        if self.conversation.is_none() {
            return;
        }
        let kind = event["type"].as_str().unwrap_or("");
        let now = Instant::now();
        match kind {
            "input_audio_buffer.speech_started" => {
                if let Some(c) = &mut self.conversation {
                    c.last_activity = now;
                    c.muted = false;
                    c.listen_until = None;
                    // Talking again after the goodbye keeps the conversation open.
                    c.closing = false;
                    c.end_after_reply = false;
                }
                // Barge-in: the user talks over the robot.
                if self.speaking {
                    self.interrupt("barge-in");
                }
            }
            "input_audio_buffer.speech_stopped" => {
                if let Some(c) = &mut self.conversation {
                    c.last_activity = now;
                }
                if !self.speaking {
                    self.set_state("thinking");
                }
            }
            "conversation.item.input_audio_transcription.completed" => {
                info!(device = %self.device_id, heard = event["transcript"].as_str().unwrap_or(""), "user");
            }
            "response.created" => {
                if let Some(c) = &mut self.conversation {
                    c.response_active = true;
                    c.spoke = false;
                    c.muted = false;
                    c.awaiting_since.get_or_insert(now);
                }
            }
            "response.output_audio.delta" => {
                let muted = self.conversation.as_ref().is_some_and(|c| c.muted);
                if muted {
                    return;
                }
                if let Some(c) = &mut self.conversation {
                    c.awaiting_since = None;
                    c.apologised = false;
                    c.spoke = true;
                    c.last_activity = now;
                }
                self.last_audio = now;
                if let Ok(bytes) = B64.decode(event["delta"].as_str().unwrap_or("")) {
                    self.enqueue(&audio::pcm_from_le(&bytes));
                }
            }
            "response.output_audio_transcript.done" => {
                info!(device = %self.device_id, said = event["transcript"].as_str().unwrap_or(""), "grok");
            }
            "response.function_call_arguments.done" => self.tool_call(&event),
            "response.done" => {
                let completed = event["response"]["status"] == "completed";
                if let Some(c) = &mut self.conversation {
                    c.response_active = false;
                    c.awaiting_since = None;
                    c.last_activity = now;
                    if c.end_after_reply && completed {
                        c.end_after_reply = false;
                        c.closing = true;
                    }
                }
                self.play_deferred();
            }
            "error" => warn!(error = %event["error"], "xAI realtime error"),
            "bridge.closed" => self.realtime_failed(),
            _ => {}
        }
    }

    // ----- tools -----

    fn tool_call(&mut self, event: &Value) {
        let Some(realtime) = self.realtime.clone() else {
            return;
        };
        let call_id = event["call_id"].as_str().unwrap_or("").to_string();
        let name = event["name"].as_str().unwrap_or("");
        let arguments: Value = serde_json::from_str(event["arguments"].as_str().unwrap_or("{}")).unwrap_or_default();
        info!(device = %self.device_id, tool = name, arguments = %arguments, "tool call");
        let output = match name {
            "ask_grok_bot" => self.ask_grok_bot(arguments["question"].as_str().unwrap_or("")),
            "set_expression" => {
                let expression = arguments["expression"].as_str().unwrap_or("neutre");
                if EXPRESSIONS.contains(&expression) {
                    self.send(json!({"type": "expression", "value": expression}));
                    "Done.".to_string()
                } else {
                    format!("Unknown expression {expression}.")
                }
            }
            "animate" => {
                let animation = arguments["name"].as_str().unwrap_or("");
                if ANIMATIONS.contains(&animation) {
                    self.animate(animation);
                    "Done.".to_string()
                } else {
                    format!("Unknown animation {animation}.")
                }
            }
            "gesture" => {
                self.head_moved();
                self.gesture(arguments["name"].as_str().unwrap_or("center"), call_id);
                return;
            }
            "move_head" => {
                self.head_moved();
                let yaw = arguments["yaw"].as_i64().unwrap_or(0).clamp(-90, 90);
                let pitch = arguments["pitch"].as_i64().unwrap_or(0).clamp(0, 60);
                // Grok hears whether the head took it, not a blind "Done."
                let args = json!({"yaw": yaw, "pitch": pitch, "speed": 200});
                self.robot_tool("head.set_angles", args, Some((call_id, HEAD_TIMEOUT)));
                return;
            }
            "take_photo" => {
                let question = arguments["question"].as_str().unwrap_or("Qu'est-ce que tu vois ?").to_string();
                self.robot_tool("camera.take_photo", json!({"question": question}), Some((call_id, PHOTO_TIMEOUT)));
                return;
            }
            "end_conversation" => {
                let spoke = self.conversation.as_ref().is_some_and(|c| c.spoke);
                if let Some(conversation) = &mut self.conversation {
                    conversation.end_after_reply = true;
                }
                // Grok said goodbye before calling it: asking for one more
                // made it say goodbye twice. The current response ends the
                // conversation (`response.done`).
                if spoke {
                    realtime.function_output_only(&call_id, "The conversation closes now. You already said goodbye.");
                    return;
                }
                "The conversation closes after your goodbye. Say a very short goodbye now.".to_string()
            }
            other => format!("Unknown tool {other}."),
        };
        realtime.function_output(&call_id, &output);
    }

    /// Ask the robot to run a tool. With a call_id, its result answers that call.
    fn robot_tool(&mut self, name: &str, args: Value, answer: Option<(String, Duration)>) -> u64 {
        let id = self.next_tool_id;
        self.next_tool_id += 1;
        self.send(json!({"type": "tool", "id": id, "name": name, "args": args}));
        if let Some((call_id, timeout)) = answer {
            self.tools_pending.insert(id, (call_id, Instant::now() + timeout));
        }
        id
    }

    fn tool_result(&mut self, message: &Value) {
        let Some(id) = message["id"].as_u64() else {
            return;
        };
        // A Grok Bot photo: the JPEG went to the vision URL; only a failure is news.
        if let Some((key, _)) = self.photo_requests.remove(&id) {
            if message["ok"] != true {
                let result = message["result"].as_str().unwrap_or("no picture");
                self.app.deliver_photo(&key, Err(format!("The robot could not take the photo: {result}")));
            }
            return;
        }
        if message["ok"] != true {
            warn!(device = %self.device_id, id, result = %message["result"], "robot tool failed");
        }
        let Some((call_id, _)) = self.tools_pending.remove(&id) else {
            return;
        };
        let result = message["result"].as_str().unwrap_or("");
        let output = if message["ok"] == true {
            result.to_string()
        } else {
            format!("The robot could not do it: {result}")
        };
        if let Some(realtime) = &self.realtime {
            realtime.function_output(&call_id, &output);
        }
    }

    fn head_moved(&mut self) {
        if let Some(conversation) = &mut self.conversation {
            conversation.head_moved = true;
        }
    }

    /// Head gestures as timed sequences of `head.set_angles`; Grok hears the
    /// robot's answer to the first step.
    fn gesture(&mut self, name: &str, call_id: String) {
        // (yaw, pitch, speed, hold ms)
        let steps: &[(i64, i64, i64, u64)] = match name {
            "nod" => &[(0, 18, 500, 250), (0, 0, 500, 250), (0, 14, 500, 250), (0, 0, 400, 0)],
            "shake" => &[(-20, 0, 500, 250), (20, 0, 500, 300), (-14, 0, 500, 300), (0, 0, 400, 0)],
            "look_left" => &[(-40, 5, 250, 0)],
            "look_right" => &[(40, 5, 250, 0)],
            "look_up" => &[(0, 35, 250, 0)],
            _ => &[(0, 0, 250, 0)],
        };
        let (&(yaw, pitch, speed, hold), rest) = steps.split_first().expect("a gesture has steps");
        self.robot_tool("head.set_angles", json!({"yaw": yaw, "pitch": pitch, "speed": speed}), Some((call_id, HEAD_TIMEOUT)));
        let mut delay = Duration::from_millis(hold.max(1));
        for &(yaw, pitch, speed, hold) in rest {
            let id = self.next_tool_id;
            self.next_tool_id += 1;
            let message = json!({"type": "tool", "id": id, "name": "head.set_angles",
                                 "args": {"yaw": yaw, "pitch": pitch, "speed": speed}});
            let out = self.out.clone();
            tokio::spawn(async move {
                tokio::time::sleep(delay).await;
                let _ = out.send(Message::Text(message.to_string().into()));
            });
            delay += Duration::from_millis(hold.max(1));
        }
    }

    fn ask_grok_bot(&mut self, question: &str) -> String {
        let grokbot = self.app.grokbot.clone();
        let question = question.split_whitespace().collect::<Vec<_>>().join(" ");
        if question.is_empty() {
            return "No question was given. Ask them what to ask Grok Bot.".into();
        }
        if !self.app.fleet.is_approved(&self.device_id) {
            return "Grok Bot is not available from this device. Say so briefly.".into();
        }
        if !grokbot.webhook_configured() {
            return "Grok Bot is not configured yet. Say so briefly.".into();
        }
        if !grokbot.allow() {
            return "Grok Bot can only be asked ten times an hour and the budget is used. Say so briefly.".into();
        }
        let request_id = uuid::Uuid::new_v4().simple().to_string();
        grokbot.remember(&request_id, &self.device_id);
        let profile = self.profile();
        let payload = json!({
            "request_id": request_id.clone(),
            "question": question.chars().take(1000).collect::<String>(),
            "device_id": self.device_id,
            "device_name": profile.name,
            "language": profile.language,
            "reply_via": "mcp:stackchan_reply",
        });
        let language = profile.language;
        let commands = self.commands.clone();
        let timeout = grokbot.reply_timeout();
        tokio::spawn(async move {
            if let Err(reason) = grokbot.post_webhook(payload, &language).await {
                grokbot.forget(&request_id);
                let _ = commands.send(Command::Say { text: reason.to_string(), kind: Kind::Reply, keep: false });
                return;
            }
            tokio::time::sleep(timeout).await;
            if !grokbot.claim_silence(&request_id) {
                return;
            }
            let text = grok_bot_silent(&language).to_string();
            // The bridge's own apology. A closed link drops it: it is not one
            // of Grok Bot's messages, so it is not written to grok_pending.json.
            let _ = commands.send(Command::Say { text, kind: Kind::Reply, keep: false });
        });
        "The question was sent to Grok Bot. Tell them in a few words that you are asking it; \
         its answer will be read out later by itself."
            .into()
    }

    // ----- verbatim speech and pushes -----

    fn command(&mut self, command: Command) {
        match command {
            // A device waiting for approval gets no push and no tool.
            Command::Say { .. } if !self.approved => {
                warn!(device = %self.device_id, "message for a device waiting for approval dropped");
            }
            Command::Photo { key } if !self.approved => {
                self.app.deliver_photo(&key, Err("The device is waiting for approval.".into()));
            }
            // A Grok Bot answer: in the conversation if one is open, else right
            // away. A bridge line (`keep: false`) is said now and not saved.
            Command::Say { text, kind: Kind::Reply, keep } => {
                if self.conversation.is_some() || !keep {
                    self.speak_verbatim(vec![text], true, keep);
                } else {
                    self.push(vec![text]);
                }
            }
            // A pushed message never cuts a conversation. `keep: false` is a
            // bridge line: said now, not held for the next connection.
            Command::Say { text, kind: Kind::Push, keep } => {
                if !keep {
                    self.speak_verbatim(vec![text], true, false);
                } else if self.conversation.is_some() {
                    self.held_pushes.push(text);
                } else {
                    self.push(vec![text]);
                }
            }
            Command::Lock(locked) => {
                info!(device = %self.device_id, locked, "lock");
                if locked {
                    self.listen_after_speech = false;
                    if self.conversation.is_some() {
                        self.interrupt("lock");
                        self.close_conversation("locked");
                    }
                }
                self.send(json!({"type": "expression", "value": if locked { LOCKED_FACE } else { "neutre" }}));
            }
            Command::Photo { key } => {
                info!(device = %self.device_id, "photo for Grok Bot");
                let question = format!("{}{key}", crate::PHOTO_QUESTION_PREFIX);
                let id = self.robot_tool("camera.take_photo", json!({"question": question}), None);
                // Ids the robot never answers are dropped by `tick`.
                self.photo_requests.insert(id, (key, Instant::now() + PHOTO_TIMEOUT));
            }
            Command::Approved => {
                if self.approved {
                    return;
                }
                self.approved = true;
                info!(device = %self.device_id, "approved");
                // Its name and the vision token, which a pending device does not get.
                self.send(hello(&self.app, &self.device_id));
                self.send(json!({"type": "expression", "value": "neutre"}));
                self.animate("wink");
                self.enroll_if_needed();
                let pending = self.app.grokbot.take_pending(&self.device_id);
                if !pending.is_empty() {
                    self.push(pending);
                }
            }
            Command::Enroll => self.enroll(),
            Command::Reboot => {
                info!(device = %self.device_id, "reboot asked");
                self.send(json!({"type": "reboot"}));
            }
            // `run` ends the connection for these two.
            Command::Replaced | Command::Revoked => {}
            Command::Played { epoch, pcm, texts } => {
                self.synthesising = self.synthesising.saturating_sub(1);
                if epoch != self.epoch {
                    return;
                }
                if self.conversation.as_ref().is_some_and(|c| c.response_active) {
                    // Never cut the model: play right after its answer.
                    self.deferred.push((pcm, texts));
                } else {
                    self.play(pcm, texts);
                }
            }
        }
    }

    /// Speak messages now, outside a conversation: notify face, chime, the
    /// text, then a short listen so the user can answer.
    fn push(&mut self, texts: Vec<String>) {
        info!(device = %self.device_id, count = texts.len(), "push");
        self.animate("notify");
        self.listen_after_speech = true;
        self.speak_verbatim(texts, true, true);
    }

    /// `keep`: a message for the user, saved for the next connection if the
    /// robot drops before it is played (not our own apology).
    fn speak_verbatim(&mut self, texts: Vec<String>, chime: bool, keep: bool) {
        if texts.is_empty() {
            return;
        }
        if keep {
            self.unspoken.extend(texts.iter().cloned());
        }
        self.synthesising += 1;
        let epoch = self.epoch;
        let tts = self.app.tts.clone();
        let commands = self.commands.clone();
        let device = self.device_id.clone();
        let profile = self.profile();
        tokio::spawn(async move {
            let mut pcm = if chime { speech::chime() } else { Vec::new() };
            let mut spoken = Vec::new();
            for text in texts {
                match tts.synthesise(&text, &profile.voice, &profile.language).await {
                    Ok(voice) => {
                        pcm.extend(voice);
                        pcm.extend(std::iter::repeat_n(0, audio::DOWNLINK_RATE as usize * 3 / 10));
                        spoken.push(text);
                    }
                    Err(error) => warn!(device = %device, %error, "verbatim speech failed"),
                }
            }
            let _ = commands.send(Command::Played { epoch, pcm, texts: spoken });
        });
    }

    fn play(&mut self, pcm: Vec<i16>, texts: Vec<String>) {
        if pcm.is_empty() {
            return;
        }
        if let Ok(Some(packet)) = self.downlink.flush() {
            self.queue.push_back(packet);
        }
        self.enqueue(&pcm);
        if let Ok(Some(packet)) = self.downlink.flush() {
            self.queue.push_back(packet);
        }
        self.last_audio = Instant::now();
        for text in texts {
            info!(device = %self.device_id, said = text.as_str(), "verbatim");
            self.unspoken.retain(|t| *t != text);
            match &self.realtime {
                Some(realtime) => realtime.note(&text),
                None => self.notes.push(text),
            }
        }
    }

    fn play_deferred(&mut self) {
        for (pcm, texts) in std::mem::take(&mut self.deferred) {
            self.play(pcm, texts);
        }
    }

    fn enqueue(&mut self, pcm: &[i16]) {
        match self.downlink.push(pcm) {
            Ok(packets) => self.queue.extend(packets),
            Err(error) => warn!(%error, "downlink encode failed"),
        }
    }

    fn end_speech(&mut self) {
        if self.speaking && !self.speech_ended {
            self.send(json!({"type": "speech", "action": "end"}));
            self.speech_ended = true;
            self.played_fallback = Some(Instant::now() + PLAYED_FALLBACK);
        }
    }

    // ----- clock -----

    fn tick(&mut self) {
        let now = Instant::now();
        let response_active = self.conversation.as_ref().is_some_and(|c| c.response_active);

        // A finished reply leaves a partial frame behind.
        if !response_active && now.duration_since(self.last_audio) > QUIET {
            match self.downlink.flush() {
                Ok(Some(packet)) => self.queue.push_back(packet),
                Ok(None) => {}
                Err(error) => warn!(%error, "downlink flush failed"),
            }
        }

        if !self.queue.is_empty() {
            if !self.speaking || self.speech_ended {
                self.set_state("speaking");
                self.send(json!({"type": "speech", "action": "start"}));
                self.speaking = true;
                self.speech_ended = false;
                self.played_fallback = None;
                self.first_packet_at = now + START_GAP;
                self.play_clock = self.first_packet_at;
            }
            while now >= self.first_packet_at && !self.queue.is_empty() && self.play_clock.saturating_duration_since(now) < AHEAD {
                let packet = self.queue.pop_front().unwrap();
                let _ = self.out.send(Message::Binary(packet.into()));
                self.play_clock = self.play_clock.max(now) + FRAME;
            }
        } else if self.speaking
            && !self.speech_ended
            && !response_active
            && self.synthesising == 0
            && self.deferred.is_empty()
            && now.duration_since(self.last_audio) > QUIET
            && now + FRAME >= self.play_clock
        {
            self.end_speech();
        }
        if self.played_fallback.is_some_and(|t| now > t) {
            debug!(device = %self.device_id, "no played from the robot; assuming playback ended");
            self.played();
        }

        // Grok Bot has given up on these photos already (`App::take_photo`).
        self.photo_requests.retain(|_, (_, until)| now <= *until);

        let expired: Vec<u64> = self.tools_pending.iter().filter(|(_, (_, at))| now > *at).map(|(id, _)| *id).collect();
        for id in expired {
            if let Some((call_id, _)) = self.tools_pending.remove(&id) {
                warn!(device = %self.device_id, id, "robot tool timed out");
                if let Some(realtime) = &self.realtime {
                    realtime.function_output(&call_id, "The robot did not answer in time. Say so briefly.");
                }
            }
        }

        if now > self.firmware_check {
            self.firmware_check = now + FIRMWARE_CHECK;
            self.offer_firmware();
        }

        self.conversation_clock(now);
    }

    /// Stalls and the reasons a conversation ends.
    fn conversation_clock(&mut self, now: Instant) {
        let Some(c) = self.conversation.as_ref() else {
            return;
        };
        if let Some(since) = c.awaiting_since
            && now.duration_since(since) > STALL {
                if c.apologised {
                    warn!(device = %self.device_id, "xAI stalled twice; closing the conversation");
                    self.close_conversation("xAI stalled");
                    return;
                }
                warn!(device = %self.device_id, "xAI stalled; apologising");
                if let Some(realtime) = &self.realtime {
                    realtime.cancel();
                }
                if let Some(c) = &mut self.conversation {
                    c.response_active = false;
                    c.apologised = true;
                    c.awaiting_since = None;
                }
                let text = apology(&self.profile().language);
                self.speak_verbatim(vec![text.to_string()], false, false);
                return;
            }
        let quiet = self.realtime.is_some()
            && !self.speaking
            && self.queue.is_empty()
            && !c.response_active
            && c.awaiting_since.is_none()
            && self.synthesising == 0
            && self.deferred.is_empty();
        if !quiet {
            return;
        }
        let idle = now.duration_since(c.last_activity);
        if c.listen_until.is_some_and(|t| now > t) {
            self.close_conversation("no answer to the push");
        } else if c.closing && idle > CLOSE_AFTER_GOODBYE {
            self.close_conversation("goodbye");
        } else if idle > self.app.idle_close {
            self.close_conversation("idle");
        }
    }

    fn finish(&mut self) {
        if let Some(conversation) = self.conversation.take() {
            self.app.add_realtime_seconds(&self.device_id, conversation.started.elapsed().as_secs());
        }
        if let Some(realtime) = self.realtime.take() {
            realtime.close();
        }
        // Unspoken messages go back to the pending file for the next connection,
        // unless the device is no longer approved (revoked).
        let unspoken: Vec<String> = self.held_pushes.drain(..).chain(self.unspoken.drain(..)).collect();
        if !self.app.fleet.is_approved(&self.device_id) {
            if !unspoken.is_empty() {
                info!(device = %self.device_id, count = unspoken.len(), "unspoken messages dropped: the device is not approved");
            }
            return;
        }
        for text in unspoken {
            let _ = self.app.grokbot.accept(None, &text, Some(&self.device_id), Kind::Push);
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::{BOOTSTRAP, MAC};

    const OTHER: &str = "aa:bb:cc:dd:ee:01";

    fn link(app: &Arc<App>, mac: &str) -> (Link, mpsc::UnboundedReceiver<Message>) {
        let (out, receiver) = mpsc::unbounded_channel();
        let (commands, _) = mpsc::unbounded_channel();
        let mut link = Link::new(app.clone(), out, mac.to_string(), commands).unwrap();
        link.approved = app.fleet.is_approved(mac);
        (link, receiver)
    }

    fn sent(receiver: &mut mpsc::UnboundedReceiver<Message>) -> Vec<Value> {
        let mut messages = Vec::new();
        while let Ok(message) = receiver.try_recv() {
            if let Message::Text(text) = message {
                messages.push(serde_json::from_str(&text).unwrap());
            }
        }
        messages
    }

    #[test]
    fn the_bridge_apologises_in_the_device_language() {
        assert_eq!(apology("fr"), "Désolé, je n'arrive pas à répondre pour l'instant. Tu peux répéter ?");
        assert_eq!(apology("en"), "Sorry, I can't answer right now. Can you say that again?");
        assert_eq!(busy_text("fr"), "Je suis occupé, réessaie dans un moment.");
        assert_eq!(busy_text("es"), "I'm busy, try again in a moment.");
        assert_eq!(grok_bot_silent("fr"), "Grok Bot n'a pas répondu.");
        assert_eq!(grok_bot_silent("en"), "Grok Bot didn't answer.");
        assert_eq!(grok_bot_silent("de"), "Grok Bot didn't answer.");
    }

    fn app_waiting(dir: &std::path::Path, timeout: Duration) -> Arc<App> {
        let fleet = Arc::new(
            crate::fleet::Fleet::load(
                crate::fleet::Config {
                    path: dir.join("fleet.json"),
                    bootstrap: BOOTSTRAP.into(),
                    default_voice: "eve".into(),
                    default_language: "en".into(),
                    timezone: chrono_tz::UTC,
                    seed: Some(crate::fleet::Seed { mac: MAC.into(), name: "stackchan".into(), locked: false }),
                },
                None,
            )
            .unwrap(),
        );
        let grokbot = Arc::new(crate::grokbot::GrokBot::new(
            crate::grokbot::Config {
                token: "grok-token".into(),
                webhook_url: "https://example.invalid/hook".into(),
                webhook_token: "hook-key".into(),
                default_device: MAC.into(),
                pending_path: dir.join("pending.json"),
                rate_per_hour: 10,
                reply_timeout: timeout,
                fleet: fleet.clone(),
            },
            None,
            Some(Arc::new(|_url: String, _payload: serde_json::Value| Box::pin(async { Ok(200) }))),
        ));
        App::new(
            grokbot,
            fleet,
            crate::realtime::Settings {
                api_key: String::new(),
                url: String::new(),
                model: String::new(),
                voice: "eve".into(),
                reasoning: "none".into(),
                persona: crate::realtime::Persona::default(),
            },
            crate::vision::Vision::new(String::new(), "grok-4.3".into(), "https://example.invalid/vision".into()),
            Duration::from_secs(40),
            crate::firmware::Firmware { dir: dir.join("firmware"), url: "https://example.invalid/firmware".into() },
            crate::slots::Slots::new(3),
            crate::testing::ADMIN.into(),
        )
    }

    #[tokio::test]
    async fn an_unanswered_grok_bot_question_is_said_aloud() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_waiting(dir.path(), Duration::from_millis(40));
        let (out, _outgoing) = mpsc::unbounded_channel();
        let (commands, mut incoming) = mpsc::unbounded_channel();
        let mut link = Link::new(app, out, MAC.to_string(), commands).unwrap();
        link.approved = true;
        let told = link.ask_grok_bot("what is on my calendar");
        assert!(told.contains("sent to Grok Bot"), "{told}");
        let command = tokio::time::timeout(Duration::from_secs(2), incoming.recv()).await.expect("silence").expect("channel");
        let Command::Say { text, kind, keep } = command else { panic!("expected the silence line") };
        assert_eq!(kind, Kind::Reply);
        assert!(!keep);
        assert_eq!(text, "Grok Bot didn't answer.");
    }

    #[tokio::test]
    async fn silence_is_dropped_when_the_robot_is_already_gone() {
        let dir = tempfile::tempdir().unwrap();
        let app = app_waiting(dir.path(), Duration::from_millis(40));
        let (out, _outgoing) = mpsc::unbounded_channel();
        let (commands, incoming) = mpsc::unbounded_channel();
        let mut link = Link::new(app.clone(), out, MAC.to_string(), commands).unwrap();
        link.approved = true;
        assert!(link.ask_grok_bot("hello").contains("sent to Grok Bot"));
        drop(incoming);
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(app.grokbot.take_pending(MAC).is_empty());
    }

    #[test]
    fn an_unknown_device_speaks_the_default_language() {
        let dir = tempfile::tempdir().unwrap();
        let app = crate::testing::app(dir.path());
        let (link, _receiver) = link(&app, OTHER);
        assert_eq!(link.profile().language, app.fleet.default_language());
    }

    #[test]
    fn only_an_approved_device_gets_the_vision_token() {
        let dir = tempfile::tempdir().unwrap();
        let app = crate::testing::app(dir.path());
        app.fleet.authenticate(OTHER, BOOTSTRAP);
        let pending = hello(&app, OTHER);
        assert_eq!((pending["approved"].as_bool(), pending["name"].as_str()), (Some(false), Some("")));
        assert!(pending.get("vision").is_none(), "{pending}");
        let approved = hello(&app, MAC);
        assert_eq!((approved["approved"].as_bool(), approved["name"].as_str()), (Some(true), Some("stackchan")));
        assert_eq!(approved["vision"]["token"].as_str(), Some(app.vision.token.as_str()));
        assert_eq!(approved["protocol"], PROTOCOL);
    }

    #[test]
    fn approval_sends_a_second_hello_with_the_vision_token() {
        let dir = tempfile::tempdir().unwrap();
        let app = crate::testing::app(dir.path());
        app.fleet.authenticate(OTHER, BOOTSTRAP);
        let (mut link, mut receiver) = link(&app, OTHER);
        assert!(!link.approved);
        app.fleet.approve(OTHER, "cuisine").unwrap();
        link.command(Command::Approved);
        let messages = sent(&mut receiver);
        let hello = messages.iter().find(|m| m["type"] == "hello").expect("a second hello");
        assert_eq!((hello["approved"].as_bool(), hello["name"].as_str()), (Some(true), Some("cuisine")));
        assert_eq!(hello["vision"]["token"].as_str(), Some(app.vision.token.as_str()));
        // The face comes after the hello, which sets the neutral face itself.
        let at = |kind: &str| messages.iter().position(|m| m["type"] == kind).unwrap();
        assert!(at("hello") < at("expression"));
        // Approved once: a second Approved changes nothing.
        link.command(Command::Approved);
        assert!(sent(&mut receiver).is_empty());
    }

    #[tokio::test]
    async fn a_bridge_line_is_not_requeued_when_playback_is_cut() {
        let dir = tempfile::tempdir().unwrap();
        let app = crate::testing::app(dir.path());
        {
            let (mut session, _receiver) = link(&app, MAC);
            session.open_conversation(app.slots.try_take().unwrap(), None);
            session.command(Command::Say {
                text: "Grok Bot didn't answer.".into(),
                kind: Kind::Reply,
                keep: false,
            });
            assert!(session.unspoken.is_empty());
            session.finish();
            assert!(app.grokbot.take_pending(MAC).is_empty());
        }
        let (mut session, _receiver) = link(&app, MAC);
        session.open_conversation(app.slots.try_take().unwrap(), None);
        session.command(Command::Say {
            text: "The deploy finished.".into(),
            kind: Kind::Reply,
            keep: true,
        });
        assert_eq!(session.unspoken, vec!["The deploy finished.".to_string()]);
        session.finish();
        assert_eq!(app.grokbot.take_pending(MAC), vec!["The deploy finished."]);
    }

    #[test]
    fn unspoken_pushes_wait_only_for_an_approved_device() {
        let dir = tempfile::tempdir().unwrap();
        let app = crate::testing::app(dir.path());
        let (mut link_, _receiver) = link(&app, MAC);
        link_.unspoken.push("Bonjour.".into());
        link_.held_pushes.push("Le café est prêt.".into());
        link_.finish();
        assert_eq!(app.grokbot.take_pending(MAC), vec!["Le café est prêt.", "Bonjour."]);

        let (mut link_, _receiver) = link(&app, MAC);
        link_.unspoken.push("Bonjour.".into());
        app.fleet.revoke(MAC).unwrap();
        link_.finish();
        assert!(app.grokbot.take_pending(MAC).is_empty());
    }

    /// What the bridge sent xAI, by event type.
    fn xai_sent(receiver: &mut mpsc::UnboundedReceiver<tokio_tungstenite::tungstenite::Message>) -> Vec<String> {
        let mut types = Vec::new();
        while let Ok(tokio_tungstenite::tungstenite::Message::Text(text)) = receiver.try_recv() {
            let event: Value = serde_json::from_str(&text).unwrap();
            types.push(event["type"].as_str().unwrap_or("").to_string());
        }
        types
    }

    fn end_call() -> Value {
        json!({"type": "response.function_call_arguments.done", "call_id": "c1", "name": "end_conversation", "arguments": "{}"})
    }

    #[tokio::test]
    async fn a_goodbye_already_said_is_not_asked_again() {
        let dir = tempfile::tempdir().unwrap();
        let app = crate::testing::app(dir.path());
        let (mut link, _robot) = link(&app, MAC);
        link.open_conversation(app.slots.try_take().unwrap(), None);
        let (realtime, mut xai) = Realtime::for_tests();
        link.realtime = Some(realtime);

        // Grok says goodbye, then calls end_conversation in the same response.
        link.realtime_event(json!({"type": "response.created"}));
        link.realtime_event(json!({"type": "response.output_audio.delta", "delta": B64.encode([0u8; 960])}));
        link.realtime_event(end_call());
        assert_eq!(xai_sent(&mut xai), ["conversation.item.create"]);
        link.realtime_event(json!({"type": "response.done", "response": {"status": "completed"}}));
        assert!(link.conversation.as_ref().unwrap().closing);

        // Grok calls end_conversation without a word: it is asked for one.
        link.conversation.as_mut().unwrap().closing = false;
        link.realtime_event(json!({"type": "response.created"}));
        link.realtime_event(end_call());
        assert_eq!(xai_sent(&mut xai), ["conversation.item.create", "response.create"]);
    }
}
