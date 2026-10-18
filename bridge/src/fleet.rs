//! The fleet: every device the bridge knows, in `fleet.json` on the data
//! volume (docs/fleet.md), and what each connected device says about itself.
//!
//! A device starts `pending` when it first connects with the bootstrap key
//! (`ROBOT_TOKEN`, embedded in every image). Once approved it gets its own key
//! over the link (`enroll`); after it confirmed it (`enrolled`, or its first
//! login with it), only that key opens its MAC. Keys are kept as SHA-256 only. Every change is written to a
//! temporary file then renamed; a change that cannot be saved is refused and
//! leaves the registry as it was.

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use tracing::error;

use crate::grokbot::Clock;
use crate::mcp::constant_time_eq;

const MAX_PENDING: usize = 20;
/// A pending device not seen for this long is forgotten.
const PENDING_TTL_S: f64 = 24.0 * 3600.0;
/// A revoked MAC is refused this long, so it does not come back as pending at once.
const REVOKED_COOLDOWN_S: f64 = 600.0;
const MAX_PROMPT_EXTRA: usize = 500;
/// Features the StackChan firmware had before it listed them in its hello.
const LEGACY_STACKCHAN: [&str; 3] = ["head", "head_touch", "camera"];
const STACKCHAN_PROJECT: &str = "botstack-fw";
const MAX_FEATURES: usize = 16;
/// The fields of a device's `status` the bridge keeps.
const STATUS_FIELDS: [&str; 7] = ["rssi", "heap_internal", "heap_psram", "uptime_s", "reset_reason", "head", "camera"];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Pending,
    Approved,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Device {
    /// Empty while pending.
    pub name: String,
    pub status: Status,
    /// SHA-256 (hex) of the device's own key, once it confirmed it.
    pub key_sha256: Option<String>,
    /// SHA-256 of the key last sent (`enroll`) and not confirmed yet.
    pub key_pending_sha256: Option<String>,
    pub locked: bool,
    /// xAI voice id.
    pub voice: String,
    /// ISO 639-1 code, such as "fr".
    pub language: String,
    /// The owner's notes, appended to the realtime instructions.
    pub prompt_extra: String,
    /// Unix seconds.
    pub first_seen: u64,
    pub last_seen: u64,
    pub approved_at: Option<u64>,
}

impl Device {
    /// "confirmed" (its own key), "pending" (a key sent, not confirmed) or
    /// "bootstrap" (no key of its own yet).
    pub fn key_state(&self) -> &'static str {
        match (&self.key_sha256, &self.key_pending_sha256) {
            (_, Some(_)) => "pending",
            (Some(_), None) => "confirmed",
            (None, None) => "bootstrap",
        }
    }
}

/// The answer to a device's connection.
#[derive(Debug, PartialEq, Eq)]
pub enum Auth {
    Approved,
    /// Known only by the bootstrap key: hello, firmware offers and reboot, nothing else.
    Pending,
    /// The HTTP status: 401 wrong key, 403 revoked or not a MAC, 429 too many
    /// pending devices, 503 the registry could not be saved.
    Refused(u16),
}

pub struct Config {
    pub path: PathBuf,
    /// `ROBOT_TOKEN`, in every image; empty refuses every device without its own key.
    pub bootstrap: String,
    /// The xAI voice of a new device.
    pub default_voice: String,
    /// The language of a new device (ISO 639-1).
    pub default_language: String,
    /// The day `seconds_today` counts in.
    pub timezone: chrono_tz::Tz,
    /// Written, approved, when the registry does not exist yet.
    pub seed: Option<Seed>,
}

pub struct Seed {
    pub mac: String,
    pub name: String,
    pub locked: bool,
}

/// Profile fields to change; None leaves a field as it is.
#[derive(Default)]
pub struct ProfileChange {
    pub voice: Option<String>,
    pub language: Option<String>,
    pub prompt_extra: Option<String>,
}

/// What a device said about itself, and its talk time. Kept after it
/// disconnects (`online` false), until it is revoked.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Live {
    /// The hub generation of the current connection.
    pub generation: u64,
    pub online: bool,
    pub connected_since: u64,
    /// The client address the proxy saw (`X-Forwarded-For`).
    pub peer: String,
    /// The firmware project from the hello: the hardware type.
    pub firmware: String,
    pub build: String,
    pub features: Vec<String>,
    /// The last `status`, known fields only.
    pub status: Option<Value>,
    pub status_at: u64,
    /// idle, listening, thinking or speaking.
    pub state: String,
    /// The date (configured timezone) `seconds_today` counts.
    pub day: String,
    pub seconds_today: u64,
    pub seconds_total: u64,
}

impl Live {
    pub fn has(&self, feature: &str) -> bool {
        self.features.iter().any(|f| f == feature)
    }
}

pub struct Fleet {
    path: PathBuf,
    bootstrap: String,
    default_voice: String,
    default_language: String,
    timezone: chrono_tz::Tz,
    clock: Clock,
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    devices: BTreeMap<String, Device>,
    /// MAC → when it was revoked.
    revoked: HashMap<String, f64>,
    live: HashMap<String, Live>,
}

impl Fleet {
    /// The registry in `config.path`, created with the seed when missing. A
    /// file that does not parse stops the bridge: reseeding would lose the
    /// devices and their keys.
    pub fn load(config: Config, clock: Option<Clock>) -> anyhow::Result<Self> {
        let clock = clock.unwrap_or_else(|| Arc::new(crate::grokbot::unix_now));
        let fleet = Self {
            path: config.path,
            bootstrap: config.bootstrap.trim().to_string(),
            default_voice: config.default_voice,
            default_language: config.default_language,
            timezone: config.timezone,
            clock,
            state: Mutex::new(State::default()),
        };
        let devices = match std::fs::read_to_string(&fleet.path) {
            Ok(raw) => serde_json::from_str::<BTreeMap<String, Device>>(&raw)
                .with_context(|| format!("{} is not a valid fleet registry; fix it or move it away", fleet.path.display()))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let mut devices = BTreeMap::new();
                if let Some(seed) = config.seed {
                    let mut device = fleet.new_device(&seed.name, Status::Approved, fleet.now() as u64);
                    device.locked = seed.locked;
                    devices.insert(normalise(&seed.mac), device);
                }
                fleet.save(&devices).context("write the fleet registry")?;
                devices
            }
            Err(error) => return Err(error).with_context(|| format!("read {}", fleet.path.display())),
        };
        fleet.state.lock().unwrap().devices = devices;
        Ok(fleet)
    }

    fn now(&self) -> f64 {
        (self.clock)()
    }

    /// The language of a new device, and of one the registry does not know.
    pub fn default_language(&self) -> &str {
        &self.default_language
    }

    fn today(&self) -> String {
        day(self.now(), self.timezone)
    }

    fn new_device(&self, name: &str, status: Status, now: u64) -> Device {
        Device {
            name: name.to_string(),
            status,
            key_sha256: None,
            key_pending_sha256: None,
            locked: false,
            voice: self.default_voice.clone(),
            language: self.default_language.clone(),
            prompt_extra: String::new(),
            first_seen: now,
            last_seen: now,
            approved_at: (status == Status::Approved).then_some(now),
        }
    }

    fn save(&self, devices: &BTreeMap<String, Device>) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = self.path.with_extension("tmp");
        let mut text = serde_json::to_vec_pretty(devices).map_err(std::io::Error::other)?;
        text.push(b'\n');
        std::fs::write(&temporary, text)?;
        std::fs::rename(&temporary, &self.path)
    }

    /// Save `devices`, then make them the registry. Err: nothing changed.
    fn commit(&self, state: &mut State, devices: BTreeMap<String, Device>) -> Result<(), String> {
        if let Err(error) = self.save(&devices) {
            error!(%error, path = %self.path.display(), "fleet registry not saved");
            return Err("The fleet registry could not be saved; nothing changed.".into());
        }
        state.devices = devices;
        Ok(())
    }

    /// Change one device; `change` also sees the whole registry (to check names).
    fn update(&self, mac: &str, change: impl FnOnce(&mut Device, &BTreeMap<String, Device>) -> Result<(), String>) -> Result<(), String> {
        let mac = normalise(mac);
        let mut state = self.state.lock().unwrap();
        let mut devices = state.devices.clone();
        let device = devices.get_mut(&mac).ok_or_else(|| format!("No device {mac}."))?;
        change(device, &state.devices)?;
        self.commit(&mut state, devices)
    }

    /// Whether `key` opens `mac` (docs/fleet.md). An unknown MAC with the
    /// bootstrap key is recorded as pending.
    pub fn authenticate(&self, mac: &str, key: &str) -> Auth {
        let mac = normalise(mac);
        if !valid_mac(&mac) {
            return Auth::Refused(403);
        }
        let now = self.now();
        let bootstrap = !self.bootstrap.is_empty() && constant_time_eq(key.as_bytes(), self.bootstrap.as_bytes());
        let hash = key_hash(key);
        let mut state = self.state.lock().unwrap();
        state.revoked.retain(|_, at| now - *at < REVOKED_COOLDOWN_S);
        if state.revoked.contains_key(&mac) {
            return Auth::Refused(403);
        }
        // (status, the key is its confirmed one, the key is the one being sent, it has no confirmed key yet)
        let known = state.devices.get(&mac).map(|device| {
            (device.status, same_hash(&device.key_sha256, &hash), same_hash(&device.key_pending_sha256, &hash), device.key_sha256.is_none())
        });
        match known {
            Some((Status::Approved, _, true, _)) => {
                // It holds the key being sent, even if its `enrolled` was lost:
                // that key is its own now. Not saved is not fatal (it stays pending).
                let mut devices = state.devices.clone();
                if let Some(device) = devices.get_mut(&mac) {
                    device.key_sha256 = device.key_pending_sha256.take();
                }
                let _ = self.commit(&mut state, devices);
                Auth::Approved
            }
            Some((Status::Approved, confirmed, _, keyless)) => {
                if confirmed || (bootstrap && keyless) { Auth::Approved } else { Auth::Refused(401) }
            }
            Some((Status::Pending, _, _, _)) if bootstrap => {
                // Seen again: it does not expire yet. Not saved is not fatal.
                let mut devices = state.devices.clone();
                if let Some(device) = devices.get_mut(&mac) {
                    device.last_seen = now as u64;
                }
                let _ = self.commit(&mut state, devices);
                Auth::Pending
            }
            None if bootstrap => {
                let mut devices = state.devices.clone();
                devices.retain(|_, d| d.status == Status::Approved || now - d.last_seen as f64 <= PENDING_TTL_S);
                if devices.values().filter(|d| d.status == Status::Pending).count() >= MAX_PENDING {
                    return Auth::Refused(429);
                }
                devices.insert(mac, self.new_device("", Status::Pending, now as u64));
                match self.commit(&mut state, devices) {
                    Ok(()) => Auth::Pending,
                    Err(_) => Auth::Refused(503),
                }
            }
            _ => Auth::Refused(401),
        }
    }

    pub fn device(&self, mac: &str) -> Option<Device> {
        self.state.lock().unwrap().devices.get(&normalise(mac)).cloned()
    }

    /// A device by MAC or by name.
    pub fn find(&self, reference: &str) -> Option<(String, Device)> {
        let wanted = normalise(reference);
        let state = self.state.lock().unwrap();
        if let Some(device) = state.devices.get(&wanted) {
            return Some((wanted, device.clone()));
        }
        state
            .devices
            .iter()
            .find(|(_, device)| !device.name.is_empty() && device.name == wanted)
            .map(|(mac, device)| (mac.clone(), device.clone()))
    }

    pub fn is_approved(&self, mac: &str) -> bool {
        self.device(mac).is_some_and(|device| device.status == Status::Approved)
    }

    pub fn locked(&self, mac: &str) -> bool {
        self.device(mac).is_some_and(|device| device.locked)
    }

    pub fn approve(&self, mac: &str, name: &str) -> Result<(), String> {
        let mac = normalise(mac);
        let now = self.now() as u64;
        self.update(&mac, |device, all| {
            if device.status == Status::Approved {
                return Err("This device is already approved.".into());
            }
            check_name(all, &mac, name)?;
            device.status = Status::Approved;
            device.name = name.to_string();
            device.approved_at = Some(now);
            Ok(())
        })
    }

    pub fn rename(&self, mac: &str, name: &str) -> Result<(), String> {
        let mac = normalise(mac);
        self.update(&mac, |device, all| {
            approved(device)?;
            check_name(all, &mac, name)?;
            device.name = name.to_string();
            Ok(())
        })
    }

    pub fn set_profile(&self, mac: &str, change: ProfileChange) -> Result<(), String> {
        self.update(mac, |device, _| {
            approved(device)?;
            if let Some(voice) = change.voice {
                if !valid_word(&voice) {
                    return Err("A voice is an xAI voice id, such as eve, ara, rex, sal or leo.".into());
                }
                device.voice = voice;
            }
            if let Some(language) = change.language {
                if !valid_language(&language) {
                    return Err("A language is an ISO 639-1 code, such as fr or en.".into());
                }
                device.language = language;
            }
            if let Some(extra) = change.prompt_extra {
                let extra = extra.split_whitespace().collect::<Vec<_>>().join(" ");
                if extra.chars().count() > MAX_PROMPT_EXTRA {
                    return Err(format!("prompt_extra is at most {MAX_PROMPT_EXTRA} characters."));
                }
                device.prompt_extra = extra;
            }
            Ok(())
        })
    }

    pub fn set_locked(&self, mac: &str, locked: bool) -> Result<(), String> {
        self.update(mac, |device, _| {
            approved(device)?;
            device.locked = locked;
            Ok(())
        })
    }

    /// Lock or unlock every approved device; their MACs.
    pub fn set_locked_all(&self, locked: bool) -> Result<Vec<String>, String> {
        let mut state = self.state.lock().unwrap();
        let mut devices = state.devices.clone();
        let mut macs = Vec::new();
        for (mac, device) in devices.iter_mut().filter(|(_, d)| d.status == Status::Approved) {
            device.locked = locked;
            macs.push(mac.clone());
        }
        self.commit(&mut state, devices)?;
        Ok(macs)
    }

    /// A fresh key for an approved device, to send it over the link. Only its
    /// hash is kept; the key itself must never be logged.
    pub fn issue_key(&self, mac: &str) -> Result<String, String> {
        let key = format!("{}{}", uuid::Uuid::new_v4().simple(), uuid::Uuid::new_v4().simple());
        let hash = key_hash(&key);
        self.update(mac, |device, _| {
            approved(device)?;
            device.key_pending_sha256 = Some(hash);
            Ok(())
        })?;
        Ok(key)
    }

    /// The device stored the key whose `key_id` is `id`. True also when that
    /// key is confirmed already (its first login with it did that) and no other
    /// waits; false, and nothing changes, for any other key.
    pub fn confirm_key(&self, mac: &str, id: &str) -> bool {
        let id = id.trim().to_lowercase();
        if id.len() < 8 {
            return false;
        }
        if self.device(mac).is_some_and(|d| d.key_pending_sha256.is_none() && d.key_sha256.as_deref().is_some_and(|h| h.starts_with(&id))) {
            return true;
        }
        self.update(mac, |device, _| match device.key_pending_sha256.take() {
            Some(hash) if hash.starts_with(&id) => {
                device.key_sha256 = Some(hash);
                Ok(())
            }
            _ => Err("No key waits for this id.".into()),
        })
        .is_ok()
    }

    /// Approved, and without a confirmed key or with one not confirmed yet.
    pub fn needs_key(&self, mac: &str) -> bool {
        self.device(mac).is_some_and(|d| d.status == Status::Approved && (d.key_sha256.is_none() || d.key_pending_sha256.is_some()))
    }

    /// Forget the device's key: the bootstrap key opens it again, and it gets a new key.
    pub fn reset_key(&self, mac: &str) -> Result<(), String> {
        self.update(mac, |device, _| {
            approved(device)?;
            device.key_sha256 = None;
            device.key_pending_sha256 = None;
            Ok(())
        })
    }

    /// Remove the device; its MAC is refused for 10 minutes.
    pub fn revoke(&self, mac: &str) -> Result<(), String> {
        let mac = normalise(mac);
        let now = self.now();
        let mut state = self.state.lock().unwrap();
        let mut devices = state.devices.clone();
        if devices.remove(&mac).is_none() {
            return Err(format!("No device {mac}."));
        }
        self.commit(&mut state, devices)?;
        state.live.remove(&mac);
        state.revoked.insert(mac, now);
        Ok(())
    }

    /// A device said hello on connection `generation` (its hub generation).
    pub fn connected(&self, mac: &str, generation: u64, peer: &str, hello: &Value) {
        let firmware = hello["firmware"].as_str().filter(|p| valid_project(p)).unwrap_or("").to_string();
        let mut features: Vec<String> = hello["features"]
            .as_array()
            .map(|list| list.iter().filter_map(Value::as_str).filter(|f| !f.is_empty() && f.len() <= 32).take(MAX_FEATURES).map(str::to_string).collect())
            .unwrap_or_default();
        if features.is_empty() && firmware == STACKCHAN_PROJECT {
            features = LEGACY_STACKCHAN.iter().map(|f| f.to_string()).collect();
        }
        let now = self.now() as u64;
        let mut state = self.state.lock().unwrap();
        let live = state.live.entry(normalise(mac)).or_default();
        live.generation = generation;
        live.online = true;
        live.connected_since = now;
        live.peer = peer.chars().take(64).collect();
        live.firmware = firmware;
        live.build = hello["build"].as_str().unwrap_or("").chars().take(16).collect();
        live.features = features;
        live.status = None;
        live.status_at = 0;
        live.state = "idle".into();
    }

    /// Connection `generation` ended; a newer one keeps the device online.
    pub fn disconnected(&self, mac: &str, generation: u64) {
        if let Some(live) = self.state.lock().unwrap().live.get_mut(&normalise(mac))
            && live.generation == generation
        {
            live.online = false;
            live.generation = 0;
            live.state.clear();
        }
    }

    pub fn set_status(&self, mac: &str, status: &Value) {
        let kept: Map<String, Value> = STATUS_FIELDS
            .iter()
            .filter_map(|key| match status.get(*key) {
                Some(Value::Number(n)) => Some((key.to_string(), Value::Number(n.clone()))),
                Some(Value::String(s)) => Some((key.to_string(), Value::String(s.chars().take(32).collect()))),
                _ => None,
            })
            .collect();
        let now = self.now() as u64;
        if let Some(live) = self.state.lock().unwrap().live.get_mut(&normalise(mac)) {
            live.status = Some(Value::Object(kept));
            live.status_at = now;
        }
    }

    pub fn set_state(&self, mac: &str, value: &str) {
        if let Some(live) = self.state.lock().unwrap().live.get_mut(&normalise(mac)) {
            live.state = value.to_string();
        }
    }

    /// Conversation time, per day in the configured timezone.
    pub fn add_seconds(&self, mac: &str, seconds: u64) {
        let today = self.today();
        if let Some(live) = self.state.lock().unwrap().live.get_mut(&normalise(mac)) {
            *live = fresh(live, &today);
            live.seconds_today += seconds;
            live.seconds_total += seconds;
        }
    }

    pub fn live(&self, mac: &str) -> Option<Live> {
        let today = self.today();
        self.state.lock().unwrap().live.get(&normalise(mac)).map(|live| fresh(live, &today))
    }

    pub fn has_feature(&self, mac: &str, feature: &str) -> bool {
        self.live(mac).is_some_and(|live| live.has(feature))
    }

    pub fn online(&self, mac: &str) -> bool {
        self.live(mac).is_some_and(|live| live.online)
    }

    /// Every device in the registry, with its live state when it connected since the start.
    pub fn entries(&self) -> Vec<(String, Device, Option<Live>)> {
        let today = self.today();
        let state = self.state.lock().unwrap();
        state
            .devices
            .iter()
            .map(|(mac, device)| (mac.clone(), device.clone(), state.live.get(mac).map(|live| fresh(live, &today))))
            .collect()
    }
}

fn approved(device: &Device) -> Result<(), String> {
    if device.status == Status::Approved { Ok(()) } else { Err("Approve this device first (fleet_approve).".into()) }
}

fn check_name(all: &BTreeMap<String, Device>, mac: &str, name: &str) -> Result<(), String> {
    if !valid_name(name) {
        return Err(format!("A name is 1 to 32 characters among a-z, 0-9 and '-'; {name:?} is not."));
    }
    if all.iter().any(|(other, device)| other != mac && device.name == name) {
        return Err(format!("The name {name} is already taken."));
    }
    Ok(())
}

fn same_hash(slot: &Option<String>, hash: &str) -> bool {
    slot.as_deref().is_some_and(|stored| constant_time_eq(stored.as_bytes(), hash.as_bytes()))
}

/// `live` with its daily count reset when `today` is a new day.
fn fresh(live: &Live, today: &str) -> Live {
    let mut live = live.clone();
    if live.day != today {
        live.day = today.to_string();
        live.seconds_today = 0;
    }
    live
}

fn day(now: f64, timezone: chrono_tz::Tz) -> String {
    chrono::DateTime::from_timestamp(now as i64, 0)
        .map(|time| time.with_timezone(&timezone).date_naive().to_string())
        .unwrap_or_default()
}

/// SHA-256 of a key, hex.
pub fn key_hash(key: &str) -> String {
    Sha256::digest(key.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

/// What `enroll` and `enrolled` carry instead of the key: 8 hex digits of its hash.
pub fn key_id(key: &str) -> String {
    key_hash(key)[..8].to_string()
}

pub fn normalise(mac: &str) -> String {
    mac.trim().to_lowercase()
}

/// `aa:bb:cc:dd:ee:ff`, lowercase.
pub fn valid_mac(mac: &str) -> bool {
    mac.len() == 17 && mac.split(':').count() == 6 && mac.split(':').all(|p| p.len() == 2 && p.chars().all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c)))
}

pub fn valid_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 32 && name.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// A firmware project name, safe as a file name: `[a-z0-9_-]`, 1 to 32 characters.
pub fn valid_project(project: &str) -> bool {
    !project.is_empty() && project.len() <= 32 && project.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn valid_word(word: &str) -> bool {
    valid_project(word)
}

pub fn valid_language(code: &str) -> bool {
    (2..=5).contains(&code.len()) && code.chars().all(|c| c.is_ascii_lowercase() || c == '-')
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    const MAC: &str = "02:00:00:00:00:01";
    const OTHER: &str = "aa:bb:cc:dd:ee:01";
    const BOOT: &str = "bootstrap-key";

    struct Fixture {
        fleet: Fleet,
        now: Arc<AtomicU64>,
        dir: tempfile::TempDir,
    }

    fn config(dir: &std::path::Path, bootstrap: &str) -> Config {
        Config {
            path: dir.join("fleet.json"),
            bootstrap: bootstrap.into(),
            default_voice: "eve".into(),
            default_language: "en".into(),
            timezone: chrono_tz::UTC,
            seed: Some(Seed { mac: MAC.to_uppercase(), name: "stackchan".into(), locked: true }),
        }
    }

    fn fixture() -> Fixture {
        fixture_with(|_| {})
    }

    /// 1_800_000_000 is Friday 15 January 2027, 08:00 UTC.
    fn fixture_with(change: impl FnOnce(&mut Config)) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let now = Arc::new(AtomicU64::new(1_800_000_000));
        let clock = now.clone();
        let mut config = config(dir.path(), BOOT);
        change(&mut config);
        let fleet = Fleet::load(config, Some(Arc::new(move || clock.load(Ordering::SeqCst) as f64))).unwrap();
        Fixture { fleet, now, dir }
    }

    impl Fixture {
        fn advance(&self, seconds: u64) {
            self.now.fetch_add(seconds, Ordering::SeqCst);
        }
        fn reload(&self) -> Fleet {
            Fleet::load(config(self.dir.path(), BOOT), None).unwrap()
        }
    }

    #[test]
    fn the_first_start_seeds_the_stackchan_approved() {
        let f = fixture();
        let device = f.fleet.device(MAC).unwrap();
        assert_eq!(device.status, Status::Approved);
        assert_eq!(device.name, "stackchan");
        assert!(device.locked);
        assert_eq!(device.key_state(), "bootstrap");
        assert_eq!(f.reload().device(MAC), Some(device));
        assert_eq!(f.fleet.authenticate(MAC, BOOT), Auth::Approved);
    }

    #[test]
    fn an_unknown_device_waits_with_the_bootstrap_key_only() {
        let f = fixture();
        assert_eq!(f.fleet.authenticate(OTHER, "wrong"), Auth::Refused(401));
        assert!(f.fleet.device(OTHER).is_none());
        assert_eq!(f.fleet.authenticate(OTHER, BOOT), Auth::Pending);
        assert_eq!(f.fleet.device(OTHER).unwrap().status, Status::Pending);
        assert_eq!(f.fleet.authenticate(OTHER, BOOT), Auth::Pending);
        assert!(!f.fleet.is_approved(OTHER));
        assert_eq!(f.fleet.authenticate("not-a-mac", BOOT), Auth::Refused(403));
    }

    #[test]
    fn a_device_goes_from_pending_to_its_own_key_then_away() {
        let f = fixture();
        f.fleet.authenticate(OTHER, BOOT);
        f.fleet.approve(OTHER, "cuisine").unwrap();
        assert_eq!(f.fleet.authenticate(OTHER, BOOT), Auth::Approved);
        assert!(f.fleet.needs_key(OTHER));

        let key = f.fleet.issue_key(OTHER).unwrap();
        assert_eq!(key.len(), 64);
        assert_eq!(f.fleet.authenticate(OTHER, &key), Auth::Approved);
        assert!(!f.fleet.confirm_key(OTHER, "00000000"));
        assert!(f.fleet.confirm_key(OTHER, &key_id(&key)));
        assert_eq!(f.fleet.device(OTHER).unwrap().key_state(), "confirmed");
        assert!(!f.fleet.needs_key(OTHER));
        assert_eq!(f.fleet.authenticate(OTHER, BOOT), Auth::Refused(401));
        assert_eq!(f.fleet.authenticate(OTHER, &key), Auth::Approved);

        // Rotation: both keys open until the new one is confirmed (by
        // `enrolled` or by its first login, as here).
        let rotated = f.fleet.issue_key(OTHER).unwrap();
        assert_eq!(f.fleet.authenticate(OTHER, &key), Auth::Approved);
        assert_eq!(f.fleet.authenticate(OTHER, &rotated), Auth::Approved);
        assert!(f.fleet.confirm_key(OTHER, &key_id(&rotated)));
        assert_eq!(f.fleet.authenticate(OTHER, &key), Auth::Refused(401));

        f.fleet.revoke(OTHER).unwrap();
        assert_eq!(f.fleet.authenticate(OTHER, &rotated), Auth::Refused(403));
        assert_eq!(f.fleet.authenticate(OTHER, BOOT), Auth::Refused(403));
        f.advance(601);
        assert_eq!(f.fleet.authenticate(OTHER, &rotated), Auth::Refused(401));
        assert_eq!(f.fleet.authenticate(OTHER, BOOT), Auth::Pending);
    }

    #[test]
    fn a_late_enrolled_confirms_nothing() {
        let f = fixture();
        let first = f.fleet.issue_key(MAC).unwrap();
        let second = f.fleet.issue_key(MAC).unwrap();
        assert!(!f.fleet.confirm_key(MAC, &key_id(&first)));
        assert_eq!(f.fleet.authenticate(MAC, &first), Auth::Refused(401));
        assert!(f.fleet.confirm_key(MAC, &key_id(&second)));
        assert_eq!(f.fleet.authenticate(MAC, &second), Auth::Approved);
    }

    #[test]
    fn a_login_with_the_pending_key_confirms_it() {
        let f = fixture();
        let first = f.fleet.issue_key(MAC).unwrap();
        assert!(f.fleet.confirm_key(MAC, &key_id(&first)));
        // Rotation; the device stores the new key but its `enrolled` is lost.
        let second = f.fleet.issue_key(MAC).unwrap();
        assert_eq!(f.fleet.authenticate(MAC, &second), Auth::Approved);
        let device = f.fleet.device(MAC).unwrap();
        assert_eq!(device.key_state(), "confirmed");
        assert_eq!(device.key_sha256, Some(key_hash(&second)));
        assert!(!f.fleet.needs_key(MAC));
        assert_eq!(f.reload().device(MAC), Some(device));
        assert_eq!(f.fleet.authenticate(MAC, &first), Auth::Refused(401));
        assert_eq!(f.fleet.authenticate(MAC, BOOT), Auth::Refused(401));
        assert_eq!(f.fleet.authenticate(MAC, &second), Auth::Approved);
        // Its `enrolled` comes late: confirmed already, not a stray key.
        assert!(f.fleet.confirm_key(MAC, &key_id(&second)));
        assert!(!f.fleet.confirm_key(MAC, &key_id(&first)));
        assert_eq!(f.fleet.device(MAC).unwrap().key_state(), "confirmed");
    }

    #[test]
    fn a_first_key_is_confirmed_by_its_first_login() {
        let f = fixture();
        let key = f.fleet.issue_key(MAC).unwrap();
        assert_eq!(f.fleet.authenticate(MAC, &key), Auth::Approved);
        assert_eq!(f.fleet.device(MAC).unwrap().key_state(), "confirmed");
        assert_eq!(f.fleet.authenticate(MAC, BOOT), Auth::Refused(401));
    }

    #[test]
    fn a_reset_key_lets_the_bootstrap_key_in_again() {
        let f = fixture();
        let key = f.fleet.issue_key(MAC).unwrap();
        assert!(f.fleet.confirm_key(MAC, &key_id(&key)));
        f.fleet.reset_key(MAC).unwrap();
        assert_eq!(f.fleet.authenticate(MAC, &key), Auth::Refused(401));
        assert_eq!(f.fleet.authenticate(MAC, BOOT), Auth::Approved);
        assert!(f.fleet.needs_key(MAC));
    }

    #[test]
    fn the_registry_never_holds_a_key_in_clear() {
        let f = fixture();
        let key = f.fleet.issue_key(MAC).unwrap();
        let saved = std::fs::read_to_string(f.dir.path().join("fleet.json")).unwrap();
        assert!(!saved.contains(&key));
        assert!(saved.contains(&key_hash(&key)));
    }

    #[test]
    fn pending_devices_are_capped_and_expire() {
        let f = fixture();
        for i in 0..MAX_PENDING {
            assert_eq!(f.fleet.authenticate(&format!("aa:bb:cc:dd:ee:{i:02x}"), BOOT), Auth::Pending);
        }
        assert_eq!(f.fleet.authenticate("aa:bb:cc:dd:ff:00", BOOT), Auth::Refused(429));
        f.advance(24 * 3600 + 1);
        assert_eq!(f.fleet.authenticate("aa:bb:cc:dd:ff:00", BOOT), Auth::Pending);
        assert!(f.fleet.device("aa:bb:cc:dd:ee:00").is_none());
        assert!(f.fleet.is_approved(MAC));
    }

    #[test]
    fn names_are_checked() {
        let f = fixture();
        f.fleet.authenticate(OTHER, BOOT);
        assert!(f.fleet.approve(OTHER, "Cuisine!").is_err());
        assert!(f.fleet.approve(OTHER, "stackchan").is_err());
        assert!(f.fleet.approve(OTHER, "").is_err());
        f.fleet.approve(OTHER, "cuisine").unwrap();
        assert!(f.fleet.approve(OTHER, "cuisine").is_err());
        assert!(f.fleet.rename(MAC, "cuisine").is_err());
        f.fleet.rename(MAC, "bureau").unwrap();
        assert_eq!(f.fleet.find("BUREAU").map(|(mac, _)| mac).as_deref(), Some(MAC));
        assert_eq!(f.fleet.find(&MAC.to_uppercase()).unwrap().1.name, "bureau");
        assert!(f.fleet.find("nobody").is_none());
    }

    #[test]
    fn profiles_are_checked() {
        let f = fixture();
        let change = ProfileChange {
            voice: Some("ara".into()),
            language: Some("en".into()),
            prompt_extra: Some("  Tu es   dans la cuisine. ".into()),
        };
        f.fleet.set_profile(MAC, change).unwrap();
        let device = f.fleet.device(MAC).unwrap();
        assert_eq!((device.voice.as_str(), device.language.as_str()), ("ara", "en"));
        assert_eq!(device.prompt_extra, "Tu es dans la cuisine.");
        assert!(f.fleet.set_profile(MAC, ProfileChange { language: Some("français".into()), ..Default::default() }).is_err());
        assert!(f.fleet.set_profile(MAC, ProfileChange { prompt_extra: Some("x".repeat(501)), ..Default::default() }).is_err());
        assert_eq!(f.fleet.device(MAC).unwrap().language, "en");
    }

    #[test]
    fn locks_one_or_all() {
        let f = fixture();
        f.fleet.authenticate(OTHER, BOOT);
        f.fleet.approve(OTHER, "cuisine").unwrap();
        assert_eq!(f.fleet.set_locked_all(false).unwrap().len(), 2);
        assert!(!f.fleet.locked(MAC));
        f.fleet.set_locked(OTHER, true).unwrap();
        assert!(f.fleet.locked(OTHER) && !f.fleet.locked(MAC));
        assert!(f.reload().locked(OTHER));
    }

    #[test]
    fn a_corrupt_registry_stops_the_start() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("fleet.json"), "{").unwrap();
        assert!(Fleet::load(config(dir.path(), BOOT), None).is_err());
    }

    #[test]
    fn a_change_that_cannot_be_saved_changes_nothing() {
        let f = fixture();
        f.fleet.authenticate(OTHER, BOOT);
        // A directory where the temporary file goes makes every write fail.
        std::fs::create_dir(f.dir.path().join("fleet.tmp")).unwrap();
        assert!(f.fleet.approve(OTHER, "cuisine").is_err());
        assert_eq!(f.fleet.device(OTHER).unwrap().status, Status::Pending);
        assert_eq!(f.fleet.authenticate("aa:bb:cc:dd:ee:02", BOOT), Auth::Refused(503));
    }

    #[test]
    fn without_a_bootstrap_key_only_own_keys_open() {
        let dir = tempfile::tempdir().unwrap();
        let fleet = Fleet::load(config(dir.path(), ""), None).unwrap();
        assert_eq!(fleet.authenticate(MAC, ""), Auth::Refused(401));
        assert_eq!(fleet.authenticate(OTHER, ""), Auth::Refused(401));
        let key = fleet.issue_key(MAC).unwrap();
        assert_eq!(fleet.authenticate(MAC, &key), Auth::Approved);
    }

    use serde_json::json;

    fn hello(firmware: &str, features: &[&str]) -> Value {
        json!({"type": "hello", "firmware": firmware, "build": "0011223344556677", "features": features})
    }

    #[test]
    fn the_old_stackchan_firmware_keeps_its_head_and_camera() {
        let f = fixture();
        f.fleet.connected(MAC, 1, "203.0.113.7", &hello("botstack-fw", &[]));
        assert!(f.fleet.has_feature(MAC, "head") && f.fleet.has_feature(MAC, "head_touch") && f.fleet.has_feature(MAC, "camera"));
        let live = f.fleet.live(MAC).unwrap();
        assert_eq!((live.firmware.as_str(), live.build.as_str(), live.peer.as_str()), ("botstack-fw", "0011223344556677", "203.0.113.7"));
        assert!(live.online);
    }

    #[test]
    fn a_device_has_only_the_features_it_lists() {
        let f = fixture();
        f.fleet.connected(OTHER, 1, "", &hello("botstack-p4", &["status", "screen:round-720"]));
        assert!(f.fleet.has_feature(OTHER, "status"));
        assert!(!f.fleet.has_feature(OTHER, "head") && !f.fleet.has_feature(OTHER, "camera"));
        f.fleet.connected(OTHER, 2, "", &hello("../../etc/passwd", &[]));
        assert_eq!(f.fleet.live(OTHER).unwrap().firmware, "");
    }

    #[test]
    fn an_older_connection_ending_leaves_the_newer_one_online() {
        let f = fixture();
        f.fleet.connected(MAC, 1, "", &hello("botstack-fw", &[]));
        f.fleet.connected(MAC, 2, "", &hello("botstack-fw", &[]));
        f.fleet.disconnected(MAC, 1);
        assert!(f.fleet.online(MAC));
        f.fleet.disconnected(MAC, 2);
        assert!(!f.fleet.online(MAC));
        assert_eq!(f.fleet.live(MAC).unwrap().firmware, "botstack-fw");
    }

    #[test]
    fn status_keeps_the_known_fields_only() {
        let f = fixture();
        f.fleet.connected(MAC, 1, "", &hello("botstack-fw", &[]));
        f.fleet.set_status(MAC, &json!({"type": "status", "rssi": -61, "heap_internal": 81234, "reset_reason": "panic", "head": "ok", "password": "x"}));
        let live = f.fleet.live(MAC).unwrap();
        assert_eq!(live.status, Some(json!({"rssi": -61, "heap_internal": 81234, "reset_reason": "panic", "head": "ok"})));
        f.fleet.set_state(MAC, "listening");
        assert_eq!(f.fleet.live(MAC).unwrap().state, "listening");
    }

    #[test]
    fn talk_time_counts_per_day() {
        let f = fixture();
        f.fleet.connected(MAC, 1, "", &hello("botstack-fw", &[]));
        f.fleet.add_seconds(MAC, 90);
        f.fleet.add_seconds(MAC, 30);
        assert_eq!(f.fleet.live(MAC).unwrap().seconds_today, 120);
        f.advance(24 * 3600);
        let live = f.fleet.live(MAC).unwrap();
        assert_eq!((live.seconds_today, live.seconds_total), (0, 120));
        assert_eq!(f.fleet.entries()[0].2.as_ref().unwrap().seconds_today, 0);
    }

    #[test]
    fn the_day_follows_the_configured_timezone() {
        for (timezone, day, later) in [
            (chrono_tz::UTC, "2027-01-15", 120),
            // UTC+13 in January: 08:00 UTC is 21:00, four hours later is the next day.
            (chrono_tz::Pacific::Auckland, "2027-01-15", 0),
        ] {
            let f = fixture_with(|config| config.timezone = timezone);
            f.fleet.connected(MAC, 1, "", &hello("botstack-fw", &[]));
            f.fleet.add_seconds(MAC, 120);
            assert_eq!(f.fleet.live(MAC).unwrap().day, day);
            f.advance(4 * 3600);
            assert_eq!(f.fleet.live(MAC).unwrap().seconds_today, later, "{timezone}");
        }
    }

    #[test]
    fn without_a_seed_the_registry_starts_empty() {
        let f = fixture_with(|config| config.seed = None);
        assert!(f.fleet.entries().is_empty());
        assert!(!f.fleet.is_approved(MAC));
        assert_eq!(f.fleet.authenticate(MAC, BOOT), Auth::Pending);
    }

    #[test]
    fn a_new_device_gets_the_default_language() {
        let f = fixture_with(|config| config.default_language = "de".into());
        assert_eq!(f.fleet.device(MAC).unwrap().language, "de");
        f.fleet.authenticate(OTHER, BOOT);
        assert_eq!(f.fleet.device(OTHER).unwrap().language, "de");
        assert_eq!(f.fleet.default_language(), "de");
    }

    #[test]
    fn a_revoked_device_is_forgotten() {
        let f = fixture();
        f.fleet.connected(MAC, 1, "", &hello("botstack-fw", &[]));
        f.fleet.revoke(MAC).unwrap();
        assert!(f.fleet.live(MAC).is_none());
        assert!(f.fleet.entries().is_empty());
    }
}
