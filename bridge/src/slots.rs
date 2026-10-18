//! The cap on simultaneous conversations (`MAX_CONVERSATIONS`): each one
//! holds an xAI realtime session billed by the minute. A `Slot` gives its
//! place back when it is dropped, however the conversation ends.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

pub struct Slots {
    active: Arc<AtomicUsize>,
    max: usize,
}

/// One conversation's place.
#[derive(Debug)]
pub struct Slot {
    active: Arc<AtomicUsize>,
}

impl Slots {
    pub fn new(max: usize) -> Self {
        Self { active: Arc::new(AtomicUsize::new(0)), max }
    }

    /// A place, or None when `max` conversations are open.
    pub fn try_take(&self) -> Option<Slot> {
        self.active
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| (n < self.max).then_some(n + 1))
            .ok()?;
        Some(Slot { active: self.active.clone() })
    }

    pub fn active(&self) -> usize {
        self.active.load(Ordering::SeqCst)
    }

    pub fn max(&self) -> usize {
        self.max
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caps_and_frees() {
        let slots = Slots::new(2);
        let first = slots.try_take().unwrap();
        let _second = slots.try_take().unwrap();
        assert!(slots.try_take().is_none());
        drop(first);
        assert_eq!(slots.active(), 1);
        assert!(slots.try_take().is_some());
    }

    #[test]
    fn zero_refuses_every_conversation() {
        assert!(Slots::new(0).try_take().is_none());
    }

    #[test]
    fn a_slot_inside_a_dropped_owner_is_freed() {
        struct Conversation {
            _slot: Slot,
        }
        let slots = Slots::new(1);
        {
            let _conversation = Conversation { _slot: slots.try_take().unwrap() };
            assert_eq!(slots.active(), 1);
        }
        assert_eq!(slots.active(), 0);
    }
}
