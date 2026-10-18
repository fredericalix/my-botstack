//! botstack bridge: each device's WebSocket on one side, xAI realtime voice on
//! the other, the Grok Bot callbacks and the fleet registry in between.
//!
//! The robot speaks botstack/v1 (docs/protocol.md) over one always-on
//! WebSocket.

pub mod admin;
pub mod audio;
pub mod firmware;
pub mod fleet;
pub mod grokbot;
pub mod http;
pub mod hub;
pub mod link;
pub mod mcp;
pub mod realtime;
pub mod slots;
pub mod speech;
pub mod vision;

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::oneshot;

/// A photo for Grok Bot: the robot's upload carries this prefix and a key in
/// its `question` field, so the camera tool needs no firmware change.
pub const PHOTO_QUESTION_PREFIX: &str = "botstack-photo:";
/// Photo, JPEG and upload take 2-4 s; a busy robot can be slower.
const PHOTO_WAIT: Duration = Duration::from_secs(30);

type PhotoResult = Result<Vec<u8>, String>;

/// What the bridge itself says aloud, in the device's language: French for
/// `fr`, English for anything else. Text the model writes (photo answers)
/// follows any language through `realtime::language_name` instead.
pub fn say(language: &str, fr: &'static str, en: &'static str) -> &'static str {
    if language == "fr" { fr } else { en }
}

pub struct App {
    pub grokbot: Arc<grokbot::GrokBot>,
    pub hub: Arc<hub::Hub>,
    pub realtime: realtime::Settings,
    pub tts: speech::Tts,
    pub vision: vision::Vision,
    pub voice_shape: audio::VoiceShape,
    /// Close a conversation after this long without speech.
    pub idle_close: Duration,
    pub fleet: Arc<fleet::Fleet>,
    /// The cap on simultaneous conversations.
    pub slots: slots::Slots,
    pub firmware: firmware::Firmware,
    pub admin: admin::Admin,
    realtime_seconds: AtomicU64,
    /// Grok Bot photos being taken, by key.
    photos: Mutex<HashMap<String, oneshot::Sender<PhotoResult>>>,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        grokbot: Arc<grokbot::GrokBot>,
        fleet: Arc<fleet::Fleet>,
        realtime: realtime::Settings,
        vision: vision::Vision,
        idle_close: Duration,
        firmware: firmware::Firmware,
        slots: slots::Slots,
        admin_token: String,
    ) -> Arc<Self> {
        let hub = Arc::new(hub::Hub::default());
        grokbot.set_speaker(hub.clone());
        let tts = speech::Tts::new(realtime.api_key.clone());
        Arc::new(Self {
            grokbot,
            fleet,
            hub,
            realtime,
            tts,
            vision,
            voice_shape: audio::VoiceShape::default(),
            idle_close,
            firmware,
            slots,
            admin: admin::Admin::new(admin_token),
            realtime_seconds: AtomicU64::new(0),
            photos: Mutex::new(HashMap::new()),
        })
    }

    /// Lock or unlock one device (`Some(mac)`) or every approved one, and tell
    /// the connected ones at once.
    pub fn set_locked(&self, mac: Option<&str>, locked: bool) -> Result<(), String> {
        let macs = match mac {
            Some(mac) => {
                self.fleet.set_locked(mac, locked)?;
                vec![mac.to_string()]
            }
            None => self.fleet.set_locked_all(locked)?,
        };
        for mac in macs {
            self.hub.send(&mac, hub::Command::Lock(locked));
        }
        Ok(())
    }

    /// "cuisine is locked. stackchan is unlocked. …", for one device or all.
    pub fn lock_report(&self, mac: Option<&str>) -> String {
        let lines: Vec<String> = self
            .fleet
            .entries()
            .into_iter()
            .filter(|(m, device, _)| device.status == fleet::Status::Approved && mac.is_none_or(|wanted| wanted == m.as_str()))
            .map(|(m, device, _)| {
                let name = if device.name.is_empty() { m } else { device.name };
                format!("{name} is {}.", if device.locked { "locked" } else { "unlocked" })
            })
            .collect();
        format!(
            "{} While a device is locked nobody can talk to it; messages and photos from Grok Bot still work.",
            lines.join(" ")
        )
    }

    /// A JPEG from the robot's camera, for Grok Bot. Err is a short reason.
    pub async fn take_photo(&self, device_id: &str) -> PhotoResult {
        let key = uuid::Uuid::new_v4().simple().to_string();
        let (sender, receiver) = oneshot::channel();
        self.photos.lock().unwrap().insert(key.clone(), sender);
        let result = if self.hub.send(device_id, hub::Command::Photo { key: key.clone() }) {
            match tokio::time::timeout(PHOTO_WAIT, receiver).await {
                Ok(Ok(result)) => result,
                _ => Err("The robot did not send the photo in time.".into()),
            }
        } else {
            Err("The robot is offline.".into())
        };
        self.photos.lock().unwrap().remove(&key);
        result
    }

    /// Hand a photo (or why there is none) to the `take_photo` waiting for
    /// `key`. False when nobody waits for it any more.
    pub fn deliver_photo(&self, key: &str, result: PhotoResult) -> bool {
        match self.photos.lock().unwrap().remove(key) {
            Some(sender) => sender.send(result).is_ok(),
            None => false,
        }
    }

    /// Conversation time, the basis of the xAI bill ($0.08/min), in total and per device.
    pub fn add_realtime_seconds(&self, mac: &str, seconds: u64) {
        self.realtime_seconds.fetch_add(seconds, Ordering::Relaxed);
        self.fleet.add_seconds(mac, seconds);
    }

    pub fn realtime_seconds(&self) -> u64 {
        self.realtime_seconds.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;

    pub const MAC: &str = "02:00:00:00:00:01";
    pub const BOOTSTRAP: &str = "bootstrap-key";
    pub const ADMIN: &str = "admin-token";

    /// An App on `dir`, with the StackChan approved and no network behind it.
    pub fn app(dir: &std::path::Path) -> Arc<App> {
        let fleet = Arc::new(
            fleet::Fleet::load(
                fleet::Config {
                    path: dir.join("fleet.json"),
                    bootstrap: BOOTSTRAP.into(),
                    default_voice: "eve".into(),
                    default_language: "en".into(),
                    timezone: chrono_tz::UTC,
                    seed: Some(fleet::Seed { mac: MAC.into(), name: "stackchan".into(), locked: false }),
                },
                None,
            )
            .unwrap(),
        );
        let grokbot = Arc::new(grokbot::GrokBot::new(
            grokbot::Config {
                token: "grok-token".into(),
                webhook_url: String::new(),
                webhook_token: String::new(),
                default_device: MAC.into(),
                pending_path: dir.join("pending.json"),
                rate_per_hour: 10,
                fleet: fleet.clone(),
            },
            None,
            None,
        ));
        App::new(
            grokbot,
            fleet,
            realtime::Settings {
                api_key: String::new(),
                url: String::new(),
                model: String::new(),
                voice: "eve".into(),
                reasoning: "none".into(),
                persona: realtime::Persona::default(),
            },
            vision::Vision::new(String::new(), "grok-4.3".into(), "https://example.invalid/vision".into()),
            Duration::from_secs(40),
            firmware::Firmware { dir: dir.join("firmware"), url: "https://example.invalid/firmware".into() },
            slots::Slots::new(3),
            ADMIN.into(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spoken_text_follows_the_device_language() {
        assert_eq!(say("fr", "Bonjour", "Hello"), "Bonjour");
        assert_eq!(say("en", "Bonjour", "Hello"), "Hello");
        assert_eq!(say("de", "Bonjour", "Hello"), "Hello");
        assert_eq!(say("", "Bonjour", "Hello"), "Hello");
    }
}
