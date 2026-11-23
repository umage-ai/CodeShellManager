//! The pairing slot: one open offer at a time, at most [`MAX_PAIRING_ATTEMPTS`] attempts, one
//! pairing channel at a time, and an expiry (design note, sections 4.2 and 8). Pure: the caller
//! passes the time in and supplies the random bytes.

use csm_remote_proto::limits::{MAX_PAIRING_ATTEMPTS, PAIRING_SLOT_MAX_SECS};

/// The open pairing offer, as the add-on holds it. Its Debug output never shows the PSK.
#[derive(Clone, PartialEq, Eq)]
pub struct Slot {
    pub pairing_id: [u8; 16],
    pub psk: [u8; 32],
    /// Unix seconds after which the slot is closed.
    pub expires_at: u64,
    /// Pairing channels opened against this slot (each counts, even if it then fails).
    pub attempts: u8,
    /// A pairing channel is open now (the relay allows only one).
    pub active: bool,
}

/// Why a pairing channel may not open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    NoSlot,
    Expired,
    WrongSlot,
    Busy,
    AttemptsUsed,
}

impl std::fmt::Debug for Slot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Slot")
            .field("pairing_id", &"<16 bytes>")
            .field("psk", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .field("attempts", &self.attempts)
            .field("active", &self.active)
            .finish()
    }
}

impl Slot {
    pub fn open(pairing_id: [u8; 16], psk: [u8; 32], now: u64) -> Self {
        Self { pairing_id, psk, expires_at: now.saturating_add(u64::from(PAIRING_SLOT_MAX_SECS)), attempts: 0, active: false }
    }

    /// Whether a pairing channel that names `id` may open now. On success the attempt is counted
    /// and the channel is marked active.
    pub fn admit(&mut self, id: &[u8; 16], now: u64) -> Result<(), Refusal> {
        if now >= self.expires_at {
            return Err(Refusal::Expired);
        }
        if *id != self.pairing_id {
            return Err(Refusal::WrongSlot);
        }
        if self.active {
            return Err(Refusal::Busy);
        }
        if self.attempts >= MAX_PAIRING_ATTEMPTS {
            return Err(Refusal::AttemptsUsed);
        }
        self.attempts += 1;
        self.active = true;
        Ok(())
    }

    pub fn seconds_left(&self, now: u64) -> u32 {
        u32::try_from(self.expires_at.saturating_sub(now)).unwrap_or(u32::MAX)
    }

    pub fn expired(&self, now: u64) -> bool {
        now >= self.expires_at
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: [u8; 16] = [3; 16];

    #[test]
    fn one_channel_at_a_time_and_five_attempts() {
        let mut s = Slot::open(ID, [0; 32], 100);
        assert_eq!(s.admit(&ID, 100), Ok(()));
        assert_eq!(s.admit(&ID, 100), Err(Refusal::Busy), "a second channel while one is open");
        s.active = false;
        for _ in 1..MAX_PAIRING_ATTEMPTS {
            assert_eq!(s.admit(&ID, 100), Ok(()));
            s.active = false;
        }
        assert_eq!(s.admit(&ID, 100), Err(Refusal::AttemptsUsed), "the sixth attempt is refused");
    }

    #[test]
    fn debug_output_never_shows_the_psk() {
        let s = Slot::open(ID, [0xAB; 32], 100);
        let text = format!("{s:?}");
        assert!(!text.contains("171"), "the decimal form of the PSK byte must not appear: {text}");
        assert!(!text.to_lowercase().contains("abab"), "the hex form must not appear: {text}");
        assert!(text.contains("redacted"));
    }

    #[test]
    fn expiry_and_wrong_slot_are_refused() {
        let mut s = Slot::open(ID, [0; 32], 100);
        assert_eq!(s.admit(&[9; 16], 100), Err(Refusal::WrongSlot));
        assert_eq!(s.admit(&ID, 100 + u64::from(PAIRING_SLOT_MAX_SECS)), Err(Refusal::Expired));
        assert!(s.expired(100 + u64::from(PAIRING_SLOT_MAX_SECS)));
        assert_eq!(s.seconds_left(100), PAIRING_SLOT_MAX_SECS);
    }
}
