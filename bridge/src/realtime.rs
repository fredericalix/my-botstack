//! Client for xAI realtime voice (`wss://api.x.ai/v1/realtime`).
//!
//! One connection per robot conversation. The session takes PCM 16 kHz (the
//! robot's decoded microphone) and returns PCM 24 kHz; the server detects the
//! end of each turn. Events are forwarded as JSON; `Realtime` sends commands.
//!
//! Phase-1 findings that shape this: the service sometimes stalls (19 s before
//! the first audio, or no `session.updated`), so callers time out and say so.

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

use crate::audio::{DOWNLINK_RATE, UPLINK_RATE};

// No Debug: it holds the xAI key.
#[derive(Clone)]
pub struct Settings {
    pub api_key: String,
    pub url: String,
    pub model: String,
    pub voice: String,
    pub reasoning: String,
    pub persona: Persona,
}

/// Who the devices belong to, as the instructions describe it.
#[derive(Clone, Debug)]
pub struct Persona {
    /// `OWNER_NAME`; empty says "the user".
    pub owner_name: String,
    /// `TIMEZONE`, for the date in the instructions.
    pub timezone: chrono_tz::Tz,
    /// `GROK_BOT_SCOPE`: what Grok Bot can read.
    pub grok_bot_scope: String,
    /// `KEYTERMS`: words the speech recognition should expect.
    pub keyterms: Vec<String>,
}

pub const DEFAULT_GROK_BOT_SCOPE: &str = "their accounts, mail, calendar and files";

impl Default for Persona {
    fn default() -> Self {
        Self {
            owner_name: String::new(),
            timezone: chrono_tz::UTC,
            grok_bot_scope: DEFAULT_GROK_BOT_SCOPE.into(),
            keyterms: Vec::new(),
        }
    }
}

impl Persona {
    /// The owner's name, or "the user".
    fn who(&self) -> String {
        if self.owner_name.trim().is_empty() { "the user".into() } else { self.owner_name.trim().to_string() }
    }
}

/// Who the session speaks for: one device of the fleet.
#[derive(Clone, Debug)]
pub struct Profile {
    pub name: String,
    /// xAI voice id.
    pub voice: String,
    /// ISO 639-1 code, such as "fr".
    pub language: String,
    /// The owner's notes about this device.
    pub prompt_extra: String,
    pub head: bool,
    pub camera: bool,
}

/// Commands to an open realtime session.
#[derive(Clone)]
pub struct Realtime {
    tx: mpsc::UnboundedSender<Message>,
}

impl Realtime {
    /// A session that only records what the bridge sends, for tests.
    #[cfg(test)]
    pub(crate) fn for_tests() -> (Self, mpsc::UnboundedReceiver<Message>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx }, rx)
    }

    /// Connect, configure the session, and wait for `session.updated`. The
    /// caller bounds the whole of it (`link::CONNECT_TIMEOUT`).
    /// Events after that arrive on the returned receiver; a closed or failed
    /// socket ends with a `{"type": "bridge.closed"}` event.
    pub async fn connect(settings: &Settings, profile: &Profile) -> Result<(Self, mpsc::UnboundedReceiver<Value>)> {
        let url = format!("{}?model={}", settings.url, settings.model);
        let mut request = url.into_client_request()?;
        request
            .headers_mut()
            .insert("Authorization", format!("Bearer {}", settings.api_key).parse()?);
        let (socket, _) = tokio_tungstenite::connect_async(request).await.context("realtime connect")?;
        let (mut sink, mut stream) = socket.split();

        let update = json!({"type": "session.update", "session": session(settings, profile)});
        sink.send(Message::text(update.to_string())).await?;

        async {
            while let Some(message) = stream.next().await {
                if let Message::Text(text) = message? {
                    let event: Value = serde_json::from_str(&text)?;
                    match event["type"].as_str() {
                        Some("session.updated") => return Ok(()),
                        Some("error") => bail!("realtime error before session.updated: {}", event["error"]),
                        _ => {}
                    }
                }
            }
            bail!("realtime closed before session.updated")
        }
        .await?;

        let (tx, mut outgoing) = mpsc::unbounded_channel::<Message>();
        let (events_tx, events) = mpsc::unbounded_channel::<Value>();
        tokio::spawn(async move {
            while let Some(message) = outgoing.recv().await {
                let closing = matches!(message, Message::Close(_));
                if sink.send(message).await.is_err() || closing {
                    break;
                }
            }
        });
        tokio::spawn(async move {
            while let Some(message) = stream.next().await {
                match message {
                    Ok(Message::Text(text)) => match serde_json::from_str::<Value>(&text) {
                        Ok(event) => {
                            if events_tx.send(event).is_err() {
                                return;
                            }
                        }
                        Err(_) => continue,
                    },
                    Ok(Message::Close(_)) | Err(_) => break,
                    Ok(_) => continue,
                }
            }
            let _ = events_tx.send(json!({"type": "bridge.closed"}));
        });
        Ok((Self { tx }, events))
    }

    fn send(&self, event: Value) {
        let _ = self.tx.send(Message::text(event.to_string()));
    }

    /// Microphone PCM 16 kHz, little endian.
    pub fn append(&self, pcm: &[u8]) {
        self.send(json!({"type": "input_audio_buffer.append", "audio": B64.encode(pcm)}));
    }

    /// Tell the model what the robot just said verbatim (a Grok Bot reply, a
    /// pushed message), so a follow-up question has its context.
    pub fn note(&self, text: &str) {
        self.send(json!({
            "type": "conversation.item.create",
            "item": {
                "type": "message",
                "role": "system",
                "content": [{"type": "input_text", "text": format!("The robot just said this aloud, word for word: {text}")}]
            }
        }));
    }

    /// Answer a function call and let the model continue.
    pub fn function_output(&self, call_id: &str, output: &str) {
        self.function_output_only(call_id, output);
        self.send(json!({"type": "response.create"}));
    }

    /// Answer a function call without asking the model for another response.
    pub fn function_output_only(&self, call_id: &str, output: &str) {
        self.send(json!({
            "type": "conversation.item.create",
            "item": {"type": "function_call_output", "call_id": call_id, "output": output}
        }));
    }

    pub fn cancel(&self) {
        self.send(json!({"type": "response.cancel"}));
    }

    pub fn close(&self) {
        let _ = self.tx.send(Message::Close(None));
    }
}

fn session(settings: &Settings, profile: &Profile) -> Value {
    let persona = &settings.persona;
    let now = chrono::Utc::now().with_timezone(&persona.timezone);
    let mut transcription = json!({"language_hint": profile.language});
    if !persona.keyterms.is_empty() {
        transcription["keyterms"] = json!(persona.keyterms);
    }
    json!({
        "voice": profile.voice,
        "instructions": instructions(profile, persona, now),
        "turn_detection": {"type": "server_vad"},
        "reasoning": {"effort": settings.reasoning},
        "audio": {
            "input": {
                "format": {"type": "audio/pcm", "rate": UPLINK_RATE},
                "transcription": transcription
            },
            "output": {"format": {"type": "audio/pcm", "rate": DOWNLINK_RATE}}
        },
        "tools": tools(profile, persona)
    })
}

/// The English name of a device language (an ISO 639-1 code), for prompts;
/// an unknown code is passed as is, and the model understands it.
pub(crate) fn language_name(code: &str) -> &str {
    match code {
        "" | "en" => "English",
        "fr" => "French",
        "de" => "German",
        "es" => "Spanish",
        "it" => "Italian",
        "pt" => "Portuguese",
        "nl" => "Dutch",
        "ja" => "Japanese",
        "zh" => "Chinese",
        "ko" => "Korean",
        other => other,
    }
}

fn instructions(profile: &Profile, persona: &Persona, now: chrono::DateTime<chrono_tz::Tz>) -> String {
    let who = persona.who();
    let mut text = format!("You are Grok, the voice of \"{}\", one of {who}'s small desk devices. ", profile.name);
    if profile.language == "fr" {
        text += &format!("Speak French by default and use \"tu\" with {who}. ");
    } else {
        text += &format!("Speak {} by default with {who}. ", language_name(&profile.language));
    }
    text += &format!(
        "If {who} talks to you in English or French, or asks you to switch language, answer in that language \
         until they ask to switch back. "
    );
    text += &format!(
        "Answer in one to three short spoken sentences, warm and direct. No lists, markdown, emoji, or URLs. \
         Answer everyday questions yourself. \
         When {who} asks about {}, or asks you to ask Grok Bot, call ask_grok_bot with the request in the conversation's language, \
         then say in a few words that you are asking. \
         The answer comes back later and is read out on its own: never invent it. Speech recognition often mangles those names; \
         call the tool anyway, and do not say you lack access. \
         A voice request is read-only: never claim you changed a server, a repository, or an account. ",
        persona.grok_bot_scope
    );
    if profile.head {
        text += "You have a body: a round face on a head that turns. Use set_expression when your mood changes, \
                 gesture to nod for yes, shake for no, or look around, and move_head to face a direction. ";
    } else {
        text += "You are a round face on a screen; you cannot move. Use set_expression when your mood changes. ";
    }
    text += "animate plays a short face animation: wink after a joke or a tease, wide when amazed, exclaim when something is striking. \
             Keep it natural and sparse, not on every sentence. ";
    if profile.camera {
        text += "When they ask what you see, or to look at something, say briefly that you look, then call take_photo. ";
    } else {
        text += "You have no camera: if they ask what you see, say so briefly. ";
    }
    text += "When they say goodbye, au revoir, or that they are done, say a very short goodbye once and call end_conversation. ";
    if !profile.prompt_extra.is_empty() {
        text += &format!("{who}'s notes about this device: {} ", profile.prompt_extra);
    }
    text += &format!(
        "When the conversation started it was {} ({}).",
        now.format("%H:%M on %A %-d %B %Y"),
        now.timezone().name()
    );
    text
}

pub const EXPRESSIONS: [&str; 16] = [
    "neutre", "attentif", "surpris", "excite", "heureux", "hilare", "colere", "triste", "effraye", "mefiant", "confus",
    "curieux", "fier", "timide", "blase", "somnolent",
];

/// Face animations Grok may play; the bridge plays the others itself.
pub const ANIMATIONS: [&str; 3] = ["wink", "wide", "exclaim"];

pub const GESTURES: [&str; 6] = ["nod", "shake", "look_left", "look_right", "look_up", "center"];

/// The tools the device can carry out: head moves only with a head, photos only with a camera.
fn tools(profile: &Profile, persona: &Persona) -> Value {
    let mut tools = vec![
        ask_grok_bot_tool(persona),
        json!({
            "type": "function",
            "name": "set_expression",
            "description": "Change your face to match your mood or the moment.",
            "parameters": {
                "type": "object",
                "properties": {"expression": {"type": "string", "enum": EXPRESSIONS}},
                "required": ["expression"]
            }
        }),
        json!({
            "type": "function",
            "name": "animate",
            "description": "Play a short face animation: wink, wide (wide eyes), or exclaim (your face becomes an exclamation mark).",
            "parameters": {
                "type": "object",
                "properties": {"name": {"type": "string", "enum": ANIMATIONS}},
                "required": ["name"]
            }
        }),
    ];
    if profile.head {
        tools.push(json!({
            "type": "function",
            "name": "gesture",
            "description": "Move your head: nod (yes), shake (no), look left, right or up, or come back to center.",
            "parameters": {
                "type": "object",
                "properties": {"name": {"type": "string", "enum": GESTURES}},
                "required": ["name"]
            }
        }));
        tools.push(json!({
            "type": "function",
            "name": "move_head",
            "description": "Turn your head. yaw: -90 (your left) to 90 (your right), 0 is straight ahead. \
                            pitch: 0 (level) to 60 (up). Stay within 45 degrees unless asked.",
            "parameters": {
                "type": "object",
                "properties": {
                    "yaw": {"type": "integer", "minimum": -90, "maximum": 90},
                    "pitch": {"type": "integer", "minimum": 0, "maximum": 60}
                },
                "required": ["yaw", "pitch"]
            }
        }));
    }
    if profile.camera {
        tools.push(json!({
            "type": "function",
            "name": "take_photo",
            "description": "Look through your camera, which faces the user, and get a description that answers the question.",
            "parameters": {
                "type": "object",
                "properties": {"question": {"type": "string", "description": "What to look for, in the conversation's language."}},
                "required": ["question"]
            }
        }));
    }
    tools.push(json!({
        "type": "function",
        "name": "end_conversation",
        "description": "End the conversation when the user says goodbye or is done. Say your very short goodbye once, before or after calling it, never twice.",
        "parameters": {"type": "object", "properties": {}}
    }));
    Value::Array(tools)
}

fn ask_grok_bot_tool(persona: &Persona) -> Value {
    json!({
        "type": "function",
        "name": "ask_grok_bot",
        "description": format!(
            "Ask Grok Bot, a slower agent with read access to {}. Use it for those topics or when {} says Grok Bot. \
             The answer comes back later, by itself.",
            persona.grok_bot_scope,
            persona.who()
        ),
        "parameters": {
            "type": "object",
            "properties": {
                "question": {"type": "string", "description": "The request, in one or two sentences, in the conversation's language."}
            },
            "required": ["question"]
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn profile(head: bool, camera: bool) -> Profile {
        Profile {
            name: "cuisine".into(),
            voice: "ara".into(),
            language: "fr".into(),
            prompt_extra: "Tu es dans la cuisine.".into(),
            head,
            camera,
        }
    }

    fn names(tools: &Value) -> Vec<&str> {
        tools.as_array().unwrap().iter().map(|t| t["name"].as_str().unwrap()).collect()
    }

    #[test]
    fn tools_follow_the_device() {
        assert_eq!(
            names(&tools(&profile(true, true), &Persona::default())),
            ["ask_grok_bot", "set_expression", "animate", "gesture", "move_head", "take_photo", "end_conversation"]
        );
        assert_eq!(names(&tools(&profile(false, false), &Persona::default())), ["ask_grok_bot", "set_expression", "animate", "end_conversation"]);
        assert_eq!(names(&tools(&profile(false, true), &Persona::default())), ["ask_grok_bot", "set_expression", "animate", "take_photo", "end_conversation"]);
    }

    #[test]
    fn instructions_follow_the_device() {
        let now = chrono_tz::Europe::Paris.with_ymd_and_hms(2026, 10, 14, 9, 5, 0).unwrap();
        let persona = Persona::default();
        let stackchan = instructions(&profile(true, true), &persona, now);
        assert!(stackchan.contains("\"cuisine\""));
        assert!(stackchan.contains("a head that turns") && stackchan.contains("move_head") && stackchan.contains("take_photo"));
        assert!(stackchan.contains("Tu es dans la cuisine."));
        assert!(stackchan.contains("09:05 on Wednesday 14 October 2026"));
        let screen = instructions(&profile(false, false), &persona, now);
        assert!(screen.contains("you cannot move") && screen.contains("no camera"));
        assert!(!screen.contains("move_head") && !screen.contains("gesture") && !screen.contains("take_photo"));
        let english = instructions(&Profile { language: "en".into(), ..profile(false, false) }, &persona, now);
        assert!(english.contains("Speak English"));
        assert!(stackchan.contains("answer in that language") && english.contains("answer in that language"));
        assert!(stackchan.contains("say a very short goodbye once"));
    }

    #[test]
    fn instructions_name_the_owner_or_the_user() {
        let now = chrono_tz::UTC.with_ymd_and_hms(2026, 10, 14, 9, 5, 0).unwrap();
        let anonymous = instructions(&profile(true, true), &Persona::default(), now);
        assert!(anonymous.contains("use \"tu\" with the user"));
        assert!(anonymous.contains("one of the user's small desk devices"));
        assert!(anonymous.contains("When the user asks about their accounts, mail, calendar and files"));
        assert!(!anonymous.contains("Ada"));
        let ada_default = Persona { owner_name: "Ada".into(), ..Persona::default() };
        let named_default = instructions(&profile(true, true), &ada_default, now);
        assert!(named_default.contains("use \"tu\" with Ada."));
        assert!(named_default.contains("When Ada asks about their accounts, mail, calendar and files"));
        let ada = Persona { owner_name: "Ada".into(), grok_bot_scope: "Ada's mail and the lab servers".into(), ..Persona::default() };
        let named = instructions(&Profile { language: "en".into(), ..profile(true, true) }, &ada, now);
        assert!(named.contains("Speak English by default with Ada."));
        assert!(named.contains("one of Ada's small desk devices"));
        assert!(named.contains("When Ada asks about Ada's mail and the lab servers"));
        assert!(named.contains("Ada's notes about this device: Tu es dans la cuisine."));
        for text in [&anonymous, &named_default, &named] {
            for word in ["French sentences", " he ", " his ", " him "] {
                assert!(!text.contains(word), "{word:?} in {text}");
            }
        }
    }

    #[test]
    fn the_date_is_in_the_configured_timezone() {
        let instant = chrono::Utc.with_ymd_and_hms(2026, 10, 15, 12, 5, 0).unwrap();
        let line = |tz: chrono_tz::Tz| instructions(&profile(false, false), &Persona::default(), instant.with_timezone(&tz));
        assert!(line(chrono_tz::Europe::Paris).contains("14:05 on Thursday 15 October 2026 (Europe/Paris)."));
        assert!(line(chrono_tz::UTC).contains("12:05 on Thursday 15 October 2026 (UTC)."));
        assert!(line(chrono_tz::Pacific::Auckland).contains("01:05 on Friday 16 October 2026 (Pacific/Auckland)."));
    }

    #[test]
    fn the_grok_bot_tool_follows_the_persona() {
        let ada = Persona { owner_name: "Ada".into(), grok_bot_scope: "the lab servers".into(), ..Persona::default() };
        let tools = tools(&profile(true, true), &ada);
        let ask = &tools[0];
        let description = ask["description"].as_str().unwrap();
        assert!(description.contains("read access to the lab servers") && description.contains("when Ada says Grok Bot"));
        assert!(ask["parameters"]["properties"]["question"]["description"].as_str().unwrap().contains("in the conversation's language"));
        let photo = tools.as_array().unwrap().iter().find(|t| t["name"] == "take_photo").unwrap();
        assert!(photo["parameters"]["properties"]["question"]["description"].as_str().unwrap().contains("in the conversation's language"));
        assert!(!tools.to_string().contains("French"));
    }

    fn settings(persona: Persona) -> Settings {
        Settings {
            api_key: String::new(),
            url: String::new(),
            model: "m".into(),
            voice: "eve".into(),
            reasoning: "none".into(),
            persona,
        }
    }

    #[test]
    fn the_session_has_the_device_voice_and_language() {
        let english = session(&settings(Persona::default()), &Profile { language: "en".into(), ..profile(false, false) });
        assert_eq!(english["voice"], "ara");
        assert_eq!(english["audio"]["input"]["transcription"]["language_hint"], "en");
        assert!(english["audio"]["input"]["transcription"].get("keyterms").is_none());
        assert_eq!(english["tools"].as_array().unwrap().len(), 4);
        let keyterms = Persona { keyterms: vec!["Grok Bot".into(), "StackChan".into()], ..Persona::default() };
        let with_keyterms = session(&settings(keyterms), &profile(false, false));
        assert_eq!(with_keyterms["audio"]["input"]["transcription"]["keyterms"], json!(["Grok Bot", "StackChan"]));
    }
}
