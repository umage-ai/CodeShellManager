# CodeShellManager 2 (branch `v2`)

CodeShellManager 2 is the cross-platform Rust rewrite (egui + wgpu) of the WPF app. It lives on
the long-lived `v2` branch of `umage-ai/CodeShellManager`; `main` stays the WPF 1.x line and
takes its hotfixes as before. The rewrite's history (formerly `umage-ai/CodeShellManager-rs`)
is merged into this branch. **Read `docs/plan.md` first**: the architecture, decisions and
phases are there. `docs/wpf/lessons.md` is the WPF app's CLAUDE.md, the record of hard-won
behaviour (ConPTY, git, quoting, restart and shutdown races); when porting a feature, read its
section there and carry over the invariant, not the C# shape.

## Build and test

Rust 1.97 or newer (`rust-version` in Cargo.toml). The gate, which must pass before a push:

```sh
pwsh scripts/get-conpty.ps1                      # once: bundled ConPTY into .conpty/ (not committed)
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo xtask ci --skip-cargo                      # layers, source policy, tool catalogue, secrets
```

## Releases (beta)

- A `v2.*` tag on this branch (for example `v2.0.0-beta.1`) builds, signs and publishes a GitHub
  pre-release through `.github/workflows/release-v2.yml`: signed `codeshell.exe` and `csm.exe`,
  a signed MSI and a portable zip. Never create a tag without Allan's say-so.
- The MSI installs as **CodeShellManager 2 Beta** beside the WPF app (its own UpgradeCode,
  folder and Start-menu entry).
- Betas are GitHub pre-releases only: no winget or Chocolatey beta packages (Allan's decision,
  2026-11-20). At 2.0 final the existing packages (`codeshellmanager`, `UmageAI.CodeShellManager`)
  are updated to 2.0, and the final MSI takes the 1.x UpgradeCode so it replaces 1.x in place.
- A dry run (unsigned MSI and zip as a workflow artifact, no release):
  `gh workflow run release-v2.yml --ref v2`. A copy of the workflow sits on `main` only so that
  GitHub allows the dispatch; keep the two copies identical.
- An MSI version must be numeric: `2.0.0-beta.N` is built as MSI version `2.0.0.N`.

## Rules (mandatory)

- **Everything is a tool** (`docs/tools.md`). Every user-visible action is a `ToolSpec` in
  `csm-engine`, dispatched by id from the UI, palette, keymap, CLI (`csm <tool>`) and MCP
  (`csm mcp`). A button that does something no tool can do is unfinished. Effects are declared
  honestly; consent for MCP callers is derived from them. Widget-level input (keystrokes into the
  focused pane, scrolling, selection, resize) goes straight to the session's terminal handle and
  is not a tool; programmatic input is (`session_send_text`).
- **Remote:** `github.com/umage-ai/CodeShellManager`. Version 2 work goes to `v2`, never to
  `main` (the WPF line), and only after a merge that passes the gate above. Agents working on
  feature branches never push and never add remotes.
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
  the user's own command is the payload (and then follow the WSL/SSH rules in `docs/wpf/lessons.md`).
- **Load native libraries by full path only** (`conpty.dll` beside the exe, `kernel32` from
  System32). Never by bare name.
- **Untrusted vs app-authored input to a PTY are different types.** Untrusted text (clipboard,
  drops, run output) is pasted with bracketed-paste markers, never written raw.
- Colours, sizes and spacing come from theme tokens, never literals in screen code.
