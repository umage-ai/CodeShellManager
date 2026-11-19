//! The OSC tap: a second `vte` parser that sees every byte before the emulator does and turns
//! the app-specific OSC sequences into typed events.
//!
//! It stops (`Perform::terminated`) right after any sequence it cares about, so the caller can
//! feed exactly the bytes up to that point to the emulator and then act — which is what keeps a
//! 7501 probe reply after the emulator's replies to earlier bytes and before its replies to
//! later ones (e.g. Claude's DA1 sentinel). Colour OSCs stop the tap too, so a colour query that
//! follows a colour change in the same chunk is answered from the changed palette.
//!
//! Payloads are untrusted. Nothing here can panic on input: fields are length-capped and decoded
//! lossily; validation of the values is the consumer's job (csm-core in the real product).

use alacritty_terminal::vte::{Params, Perform};

/// Longest OSC payload passed on (the OSC 7501 spec's own sequence cap).
pub const MAX_OSC_PAYLOAD: usize = 4096;
/// Field caps for OSC 9001 (keys are short words; values are capped generously and the
/// consumer applies the real per-key limits).
pub const MAX_9001_KEY: usize = 32;
pub const MAX_9001_VALUE: usize = 1024;

/// An OSC 133 semantic-prompt mark (FinalTerm).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PromptMark {
    /// `A`: a prompt starts.
    PromptStart,
    /// `B`: the prompt ends, user input starts.
    CommandStart,
    /// `C`: the command was submitted and its output starts.
    CommandExecuted,
    /// `D[;exit]`: the command finished.
    CommandFinished { exit_code: Option<i32> },
}

/// Something the tap recognised, in stream order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TapEvent {
    /// `OSC 9001 ; key=value ; … ST` — raw (trimmed, capped) pairs, in order.
    ShellIntegration(Vec<(String, String)>),
    /// `OSC 7501 ; <payload> ST` — a status report, raw payload (validated by the consumer).
    ProgramStatus(String),
    /// `OSC 7501 ; ? ST` — the feature probe.
    ProgramStatusProbe,
    /// `OSC 133 ; A|B|C|D … ST`.
    Prompt(PromptMark),
    /// `OSC 7 ; file://host/path ST` — the raw URI.
    Cwd(String),
}

#[derive(Debug, Default)]
pub struct OscTap {
    pub(crate) events: Vec<TapEvent>,
    /// A colour-changing OSC was seen (4/10/11/12/104/110/111/112).
    pub(crate) colors_changed: bool,
    /// win32-input-mode requested by the ConPTY (`CSI ? 9001 h`, cleared by `l`).
    pub(crate) win32_input_mode: bool,
    /// An XTVERSION query (`CSI > q` / `CSI > 0 q`) to answer after this segment.
    pub(crate) xtversion_query: bool,
    stop: bool,
}

impl OscTap {
    pub(crate) fn reset_stop(&mut self) {
        self.stop = false;
    }
}

fn lossy(bytes: &[u8], cap: usize) -> String {
    let bytes = bytes.get(..cap).unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

fn join(params: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, p) in params.iter().enumerate() {
        if i > 0 {
            out.push(b';');
        }
        out.extend_from_slice(p);
    }
    out
}

fn parse_9001(fields: &[&[u8]]) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for field in fields {
        let Some(eq) = field.iter().position(|&b| b == b'=') else { continue };
        let (k, v) = field.split_at(eq);
        let v = v.get(1..).unwrap_or_default();
        let key = lossy(k, MAX_9001_KEY).trim().to_string();
        if key.is_empty() {
            continue;
        }
        out.push((key, lossy(v, MAX_9001_VALUE).trim().to_string()));
    }
    out
}

fn parse_133(params: &[&[u8]]) -> Option<PromptMark> {
    let kind = params.first()?.first()?;
    Some(match kind {
        b'A' => PromptMark::PromptStart,
        b'B' => PromptMark::CommandStart,
        b'C' => PromptMark::CommandExecuted,
        b'D' => {
            PromptMark::CommandFinished { exit_code: params.get(1).and_then(|p| std::str::from_utf8(p).ok()).and_then(|s| s.trim().parse().ok()) }
        }
        _ => return None,
    })
}

impl Perform for OscTap {
    fn osc_dispatch(&mut self, params: &[&[u8]], _bell_terminated: bool) {
        let Some((&code, rest)) = params.split_first() else { return };
        let total: usize = rest.iter().map(|p| p.len() + 1).sum();
        match code {
            b"9001" => {
                if total <= MAX_OSC_PAYLOAD {
                    self.events.push(TapEvent::ShellIntegration(parse_9001(rest)));
                }
                self.stop = true;
            }
            b"7501" => {
                if total <= MAX_OSC_PAYLOAD {
                    let payload = join(rest);
                    if payload.trim_ascii() == b"?" {
                        self.events.push(TapEvent::ProgramStatusProbe);
                    } else {
                        self.events.push(TapEvent::ProgramStatus(lossy(&payload, MAX_OSC_PAYLOAD)));
                    }
                }
                self.stop = true;
            }
            b"133" => {
                if let Some(mark) = parse_133(rest) {
                    self.events.push(TapEvent::Prompt(mark));
                }
                self.stop = true;
            }
            b"7" => {
                if total <= MAX_OSC_PAYLOAD {
                    self.events.push(TapEvent::Cwd(lossy(&join(rest), MAX_OSC_PAYLOAD)));
                }
                self.stop = true;
            }
            b"4" | b"10" | b"11" | b"12" | b"104" | b"110" | b"111" | b"112" => {
                self.colors_changed = true;
                self.stop = true;
            }
            _ => {}
        }
    }

    fn csi_dispatch(&mut self, params: &Params, intermediates: &[u8], _ignore: bool, action: char) {
        // XTVERSION. alacritty_terminal does not answer it, and programs use the answer to
        // decide whether to ask about features (Claude Code only asks DECRQM 2026, synchronized
        // output, after an XTVERSION reply).
        if intermediates == b">" && action == 'q' && params.iter().all(|p| p.first().is_none_or(|&v| v == 0)) {
            self.xtversion_query = true;
            self.stop = true;
            return;
        }
        if intermediates == b"?" && (action == 'h' || action == 'l') {
            for p in params.iter() {
                if p.first() == Some(&9001) {
                    self.win32_input_mode = action == 'h';
                }
            }
        }
    }

    fn terminated(&self) -> bool {
        self.stop
    }
}
