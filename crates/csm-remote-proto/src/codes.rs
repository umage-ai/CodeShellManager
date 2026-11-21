//! Stable refusal codes. The string is what travels in JSON (`"code":"host_offline"`) and in the
//! WebSocket close reason; the number is the binary `Close` frame payload and the WebSocket close
//! code is `4000 + number`. Codes are append-only: a phone turns each into a plain message.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RefusalCode {
    /// Malformed or out-of-order message.
    BadRequest,
    /// The peer speaks a protocol version this relay does not.
    UnsupportedVersion,
    /// Wrong host secret, token not on the allow list, unknown or used-up pairing slot.
    NotAllowed,
    /// The operator's access control does not admit this host (not on the list, no enrolment token).
    NotEnrolled,
    /// The host the phone asked for has no live connection.
    HostOffline,
    /// A rate limit or the per-IP socket cap was hit.
    RateLimited,
    /// The relay is at a capacity limit (global host cap, channels per host).
    RelayFull,
    /// A frame or message over the size cap.
    FrameTooLarge,
    /// Nothing was sent for too long.
    IdleTimeout,
    /// The device was removed from the host's allow list.
    Revoked,
    /// A newer connection for the same host id replaced this one.
    HostReplaced,
    /// The relay refused a push request (endpoint not allowed, push disabled).
    PushRefused,
    /// The push service did not accept the push.
    PushFailed,
}

impl RefusalCode {
    pub const ALL: &'static [RefusalCode] = &[
        Self::BadRequest,
        Self::UnsupportedVersion,
        Self::NotAllowed,
        Self::NotEnrolled,
        Self::HostOffline,
        Self::RateLimited,
        Self::RelayFull,
        Self::FrameTooLarge,
        Self::IdleTimeout,
        Self::Revoked,
        Self::HostReplaced,
        Self::PushRefused,
        Self::PushFailed,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BadRequest => "bad_request",
            Self::UnsupportedVersion => "unsupported_version",
            Self::NotAllowed => "not_allowed",
            Self::NotEnrolled => "not_enrolled",
            Self::HostOffline => "host_offline",
            Self::RateLimited => "rate_limited",
            Self::RelayFull => "relay_full",
            Self::FrameTooLarge => "frame_too_large",
            Self::IdleTimeout => "idle_timeout",
            Self::Revoked => "revoked",
            Self::HostReplaced => "host_replaced",
            Self::PushRefused => "push_refused",
            Self::PushFailed => "push_failed",
        }
    }

    /// Wire number (never 0: 0 means "closed normally" in a `Close` frame).
    pub const fn as_u16(self) -> u16 {
        match self {
            Self::BadRequest => 1,
            Self::UnsupportedVersion => 2,
            Self::NotAllowed => 3,
            Self::NotEnrolled => 4,
            Self::HostOffline => 5,
            Self::RateLimited => 6,
            Self::RelayFull => 7,
            Self::FrameTooLarge => 8,
            Self::IdleTimeout => 9,
            Self::Revoked => 10,
            Self::HostReplaced => 11,
            Self::PushRefused => 12,
            Self::PushFailed => 13,
        }
    }

    pub fn from_u16(n: u16) -> Option<Self> {
        Self::ALL.iter().copied().find(|c| c.as_u16() == n)
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|c| c.as_str() == s)
    }

    /// The WebSocket close code the relay uses (private range 4000-4999).
    pub const fn ws_close_code(self) -> u16 {
        4000 + self.as_u16()
    }
}

impl std::fmt::Display for RefusalCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_and_names_round_trip_and_are_unique() {
        for c in RefusalCode::ALL {
            assert_eq!(RefusalCode::from_u16(c.as_u16()), Some(*c));
            assert_eq!(RefusalCode::parse(c.as_str()), Some(*c));
            assert_eq!(serde_json::to_string(c).unwrap(), format!("\"{}\"", c.as_str()));
        }
        let mut nums: Vec<_> = RefusalCode::ALL.iter().map(|c| c.as_u16()).collect();
        nums.sort_unstable();
        nums.dedup();
        assert_eq!(nums.len(), RefusalCode::ALL.len());
        assert_eq!(RefusalCode::from_u16(0), None);
    }
}
