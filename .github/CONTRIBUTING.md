# Contributing to CodeShellManager

Thanks for considering a contribution. CodeShellManager is a native host for many terminal CLIs
(Claude Code, Codex, Gemini, pwsh, ssh…), written in Rust. Bug fixes and focused features are
welcome.

## Before you start

- For bug fixes: open an issue first only if you want to confirm the diagnosis. Otherwise, just
  send the PR.
- For features: open an issue (or a Discussion) and get a thumbs-up before investing time. The bar
  for "is this in scope" is "would the maintainers use it daily?".
- Read [docs/plan.md](../docs/plan.md) for the architecture and the reasons behind it,
  [docs/tools.md](../docs/tools.md) for the tool registry, and [CLAUDE.md](../CLAUDE.md) for the
  rules in short. [docs/wpf/lessons.md](../docs/wpf/lessons.md) records hard-won behaviour from the
  0.x WPF app (ConPTY, git, quoting, restart and shutdown races); when you touch one of those
  areas, read its section and keep the invariant.

## Build and run

Rust 1.97 (the `rust-version` in `Cargo.toml`). CI uses exactly 1.97, so use `cargo +1.97`
locally to get the same clippy lints. On Windows you also need PowerShell 7.

```sh
pwsh scripts/get-conpty.ps1             # once, Windows: the bundled ConPTY into .conpty/ (never committed)
cargo +1.97 run -p codeshell            # the app, on your real state
cargo +1.97 run -p codeshell -- --clean # the app from a blank slate; reads and writes no state
cargo +1.97 run -p codeshell -- --demo  # demo data (the screenshots' data, all fictional)
cargo +1.97 run -p csm-cli -- session-list   # the CLI, against the running app
```

Without the bundled ConPTY, sessions run on Windows' inbox ConPTY; `csm app-info` and
Settings → About say which one is in use. Use `--clean` (or `--config-dir <dir>`) whenever you work
on lifecycle, restore or settings code, so your real sessions are not disturbed.

On Linux, egui and wgpu need `libxkbcommon-dev libwayland-dev libx11-dev libxrandr-dev libxi-dev
libgl1-mesa-dev libgtk-3-dev` (see `.github/workflows/ci.yml`).

## The gate

A change must pass all of these before it is merged:

```sh
cargo +1.97 test --workspace --no-fail-fast
cargo +1.97 clippy --workspace --all-targets -- -D warnings
cargo +1.97 xtask ci
```

`cargo xtask ci` runs the repository checks (and, without `--skip-cargo`, the tests and clippy
again): `layers` (crate dependencies follow the layer map), `policy` (source rules, such as no
process spawning or file I/O in UI frame code and `unsafe` only in the ConPTY module),
`tools --check` (the generated catalogue in `docs/tools.md` matches the registry; run
`cargo xtask tools` after adding a tool) and `secrets`. CI runs the same on Windows, macOS and
Linux, plus `cargo deny check` for licences and advisories.

Enable the secrets check as a pre-commit hook with `git config core.hooksPath .githooks`.

UI tests use `egui_kittest`. To refresh the screenshots in `docs/screens/`:

```sh
CSM_SCREENS_OUT=docs/screens cargo +1.97 test -p csm-ui --test screenshots --test board_subagents
```

## The rules

Enforced in review and, where possible, by `xtask`:

- **Everything is a tool.** Every user-visible action is a `ToolSpec` in `csm-engine`, dispatched
  by id from the UI, the palette, the keymap, the CLI (`csm <tool>`) and MCP (`csm mcp`). A button
  that does something no tool can do is unfinished. Declare a tool's effects honestly: consent for
  MCP callers is derived from them. Keystrokes into the focused pane, scrolling, selection and
  resize are widget input, not tools; programmatic input is (`session_send_text`). See
  [docs/tools.md](../docs/tools.md).
- **Layering.** `csm-core` (pure) → `csm-pty`, `csm-term` → `csm-services` → `csm-engine` →
  `csm-ui` → `apps/*`. Lower crates never depend on higher ones; only `csm-ui` and the apps use
  egui.
- **`unsafe`** is denied in the workspace; every crate root has `#![forbid(unsafe_code)]` except
  `csm-pty`, whose Windows ConPTY module alone may use it, each block with a `// SAFETY:` comment.
- **Never crash on input.** No `unwrap`, `expect`, `panic!` or indexing on values derived from PTY
  output, state files, imports, OSC payloads or git output. Tests may unwrap.
- **Never block the UI thread.** No process spawn, file or network I/O, or lock waits in egui
  frame code: send the work to a background thread and poll a channel.
- **Never interpolate a value into a command line.** Build argv and exec directly; no shell unless
  the user's own command is the payload.
- **Load native libraries by full path only**, never by bare name.
- **Untrusted text into a PTY is pasted** (bracketed paste), never typed raw.
- Colours, sizes and spacing come from theme tokens, never literals in screen code.

## Branches, commits and pull requests

- Branch names: `feat/<short-name>`, `fix/<short-name>`, `chore/<short-name>`, `docs/<short-name>`.
- Commit subjects say what changed in plain words; the body explains *why*, since the diff already
  shows *what*.
- Target `main`. Keep a PR small and focused; ship an unrelated fix in its own PR.
- Use the PR template. CI must be green.
- Don't squash commits by hand before merge; the maintainer picks the merge strategy.

## Issues and labels

- `bug`: applied by the bug report template
- `enhancement`: applied by the feature request template

Dependabot PRs get `dependencies` automatically. For usage questions, use
[Discussions](https://github.com/umage-ai/CodeShellManager/discussions) rather than issues.

## Code style

- Match what's already there. Comments explain *why*, not *what*.
- `rustfmt.toml` holds the target formatting, but it has not been applied to the whole workspace
  yet: don't reformat files you aren't otherwise changing.

## License

By contributing, you agree your contribution is licensed under the same license as the project
(see [LICENSE](../LICENSE)).
