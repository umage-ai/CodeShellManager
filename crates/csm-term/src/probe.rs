//! Answering the OSC 7501 feature probe (`ESC ] 7501 ; ? ST`).
//!
//! Port of `ProgramStatusProbeGate` / the reply encodings (CLAUDE.md "Program status (OSC
//! 7501)"). On the bundled ConPTY the reply is the raw `ESC ] 7501 ; ? ESC \` — OpenConsole
//! passes OSC input through. The inbox conhost drops OSC on input, so there the same characters
//! are sent as win32-input-mode key events, and only when the ConPTY asked for that mode; even
//! then it loses the race against conhost's own DA1 answer, which is why the host normally
//! leaves inbox at `Off`.

use std::time::{Duration, Instant};

/// How (whether) the probe is answered. The host picks this from the PTY backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProbeReply {
    #[default]
    Off,
    /// Raw OSC reply (bundled ConPTY, Unix PTYs).
    Raw,
    /// win32-input-mode encoded reply (inbox ConPTY experiments only).
    Win32InputMode,
}

pub const PROBE_REPLY: &[u8] = b"\x1b]7501;?\x1b\\";

/// Each character of `text` as a win32-input-mode key event: `ESC [ 0;0;<char>;1;0;1 _`.
pub fn encode_win32_input_mode(text: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len() * 16);
    for &b in text {
        out.extend_from_slice(format!("\x1b[0;0;{b};1;0;1_").as_bytes());
    }
    out
}

/// At most one reply per [`ProbeGate::MIN_INTERVAL`], and [`ProbeGate::MAX_REPLIES`] per session,
/// so a program (or a `cat` of a file) that repeats the probe cannot make the terminal type
/// stray input forever.
#[derive(Debug, Default)]
pub struct ProbeGate {
    replies: u32,
    last: Option<Instant>,
}

impl ProbeGate {
    pub const MAX_REPLIES: u32 = 3;
    pub const MIN_INTERVAL: Duration = Duration::from_secs(5);

    /// The bytes to write for a probe seen at `now`, or `None` when it must not be answered.
    pub fn reply(&mut self, now: Instant, mode: ProbeReply, win32_input_mode: bool) -> Option<Vec<u8>> {
        let bytes = match mode {
            ProbeReply::Off => return None,
            ProbeReply::Raw => PROBE_REPLY.to_vec(),
            ProbeReply::Win32InputMode if win32_input_mode => encode_win32_input_mode(PROBE_REPLY),
            ProbeReply::Win32InputMode => return None,
        };
        if self.replies >= Self::MAX_REPLIES {
            return None;
        }
        if let Some(last) = self.last
            && now.saturating_duration_since(last) < Self::MIN_INTERVAL
        {
            return None;
        }
        self.replies += 1;
        self.last = Some(now);
        Some(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_and_session_caps() {
        let mut g = ProbeGate::default();
        let t0 = Instant::now();
        assert_eq!(g.reply(t0, ProbeReply::Raw, false).unwrap(), PROBE_REPLY);
        assert!(g.reply(t0 + Duration::from_secs(1), ProbeReply::Raw, false).is_none());
        assert!(g.reply(t0 + Duration::from_secs(6), ProbeReply::Raw, false).is_some());
        assert!(g.reply(t0 + Duration::from_secs(12), ProbeReply::Raw, false).is_some());
        assert!(g.reply(t0 + Duration::from_secs(60), ProbeReply::Raw, false).is_none());
    }

    #[test]
    fn off_and_win32_gating() {
        let mut g = ProbeGate::default();
        let t = Instant::now();
        assert!(g.reply(t, ProbeReply::Off, true).is_none());
        assert!(g.reply(t, ProbeReply::Win32InputMode, false).is_none());
        let r = g.reply(t, ProbeReply::Win32InputMode, true).unwrap();
        assert!(r.starts_with(b"\x1b[0;0;27;1;0;1_\x1b[0;0;93;1;0;1_"));
    }
}
