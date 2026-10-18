//! Live connections by device, so a Grok Bot reply, push or photo request
//! can reach the robot.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::sync::mpsc;

use crate::grokbot::{Kind, Speaker};

pub enum Command {
    /// A Grok Bot reply or pushed message for this conversation.
    Say { text: String, kind: Kind },
    /// Verbatim speech is ready (see `speech`). `epoch` drops it after a tap.
    Played { epoch: u64, pcm: Vec<i16>, texts: Vec<String> },
    /// Take a photo for Grok Bot; the upload carries `key` (see `App::take_photo`).
    Photo { key: String },
    /// The lock was switched on or off (see `lock`).
    Lock(bool),
    /// The device was approved while connected: it may talk, and gets its key.
    Approved,
    /// Send the device a new key of its own (fleet_rotate_key).
    Enroll,
    /// Restart between conversations (fleet_reboot).
    Reboot,
    /// The device was revoked: this connection ends.
    Revoked,
    /// The robot connected again: this older connection ends (and with it
    /// any xAI session it still holds).
    Replaced,
}

#[derive(Default)]
pub struct Hub {
    sessions: Mutex<HashMap<String, (u64, mpsc::UnboundedSender<Command>)>>,
    next: AtomicU64,
}

impl Hub {
    /// A newer connection for the same device replaces the older one, which is told to end.
    pub fn register(&self, device_id: &str, sender: mpsc::UnboundedSender<Command>) -> u64 {
        let generation = self.next.fetch_add(1, Ordering::SeqCst);
        let older = self
            .sessions
            .lock()
            .unwrap()
            .insert(device_id.to_lowercase(), (generation, sender));
        if let Some((_, older)) = older {
            let _ = older.send(Command::Replaced);
        }
        generation
    }

    /// False when the robot is not connected.
    pub fn send(&self, device_id: &str, command: Command) -> bool {
        let sessions = self.sessions.lock().unwrap();
        sessions.get(&device_id.to_lowercase()).is_some_and(|(_, sender)| sender.send(command).is_ok())
    }

    pub fn is_online(&self, device_id: &str) -> bool {
        self.sessions.lock().unwrap().contains_key(&device_id.to_lowercase())
    }

    pub fn unregister(&self, device_id: &str, generation: u64) {
        let mut sessions = self.sessions.lock().unwrap();
        if sessions.get(&device_id.to_lowercase()).is_some_and(|(g, _)| *g == generation) {
            sessions.remove(&device_id.to_lowercase());
        }
    }
}

impl Speaker for Hub {
    fn speak(&self, device_id: &str, _request_id: Option<&str>, text: &str, kind: Kind) -> bool {
        self.send(device_id, Command::Say { text: text.to_string(), kind })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_connection_ends_the_older_one() {
        let hub = Hub::default();
        let (old_tx, mut old_rx) = mpsc::unbounded_channel();
        let old = hub.register("AA:BB", old_tx);
        let (new_tx, mut new_rx) = mpsc::unbounded_channel();
        hub.register("aa:bb", new_tx);
        assert!(matches!(old_rx.try_recv(), Ok(Command::Replaced)));
        hub.unregister("aa:bb", old);  // the older link ending must not drop the newer one
        assert!(hub.send("aa:bb", Command::Lock(true)));
        assert!(matches!(new_rx.try_recv(), Ok(Command::Lock(true))));
    }

    #[test]
    fn knows_who_is_online() {
        let hub = Hub::default();
        assert!(!hub.is_online("aa:bb"));
        let (tx, _rx) = mpsc::unbounded_channel();
        let generation = hub.register("AA:BB", tx);
        assert!(hub.is_online("aa:bb"));
        assert!(hub.send("aa:bb", Command::Reboot));
        hub.unregister("aa:bb", generation);
        assert!(!hub.is_online("aa:bb"));
    }
}
