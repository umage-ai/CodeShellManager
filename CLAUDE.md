# CodeShellManager-rs

Rust rewrite of the WPF app in `../CodeShellManager`. **Read `docs/plan.md` first**: the
architecture, decisions and phases are there. The WPF repo's `CLAUDE.md` is the record of
hard-won behaviour (ConPTY, git, quoting, restart and shutdown races); when porting a feature,
read its section there and carry over the invariant, not the C# shape.

## Rules (mandatory)

- **Local commits only.** Never push, never add a remote unless the maintainer asks.
- **Layering:** `csm-core` (pure) → `csm-pty`, `csm-term` → `csm-services` → `csm-engine` →
  `csm-ui` → `apps/*`. Lower crates never depend on higher ones; only `csm-ui` and apps use egui.
- **`unsafe`:** workspace lint is `deny`; every crate root has `#![forbid(unsafe_code)]` except
  `csm-pty`, whose Windows ConPTY module alone may `#[allow(unsafe_code)]`, each block with a
  `// SAFETY:` comment.
- **Never crash on input.** No `unwrap`/`expect`/`panic!`/indexing on values derived from PTY
  output, state files, imports, OSC payloads or git output. Tests may unwrap.
- **Never block the UI thread.** No process spawn, file or network I/O, or lock waits in egui
  frame code: send work to a background thread, poll a channel.
- **Never interpolate a value into a command line.** Build argv; exec directly; no shell unless
  the user's own command is the payload (and then follow the WPF repo's WSL/SSH rules).
- **Load native libraries by full path only** (`conpty.dll` beside the exe, `kernel32` from
  System32). Never by bare name.
- **Untrusted vs app-authored input to a PTY are different types.** Untrusted text (clipboard,
  drops, run output) is pasted with bracketed-paste markers, never written raw.
- Colours, sizes and spacing come from theme tokens, never literals in screen code.
