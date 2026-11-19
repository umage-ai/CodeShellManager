# CodeShellManager (Rust)

Cross-platform rewrite of [CodeShellManager](../CodeShellManager): a multi-terminal host for
Claude Code and other CLI tools. egui + wgpu, Windows / macOS / Linux.

Plan, decisions and phases: [docs/plan.md](docs/plan.md).

```sh
pwsh scripts/get-conpty.ps1          # once: bundled ConPTY into .conpty/ (not committed)
cargo build
cargo test
cargo run -p codeshell               # the app, on your state (state.json in the user data folder)
cargo run -p codeshell -- --clean    # the app from a blank slate; reads and writes no state
cargo run -p codeshell -- --config-dir <dir>   # state.json in <dir> (or set CSM_CONFIG_DIR)
cargo run -p codeshell -- --demo     # the mock-up's data (screenshots, design review)
cargo run -p csm-cli -- session-list # the CLI, against the running app; `csm mcp` for MCP
cargo run --release -p term-spike    # Phase 0 terminal spike
```

`codeshell`'s build script copies `conpty.dll` and `OpenConsole.exe` from `.conpty/<arch>/`
beside the executable; without them sessions run on the inbox ConPTY (`csm app-info` says which).
Closing the window quits through `app_quit`: sessions get time to exit (the shutdown board) and
the window closes when the engine reports `QuitReady`.

Phase 0 spike: usage, measured results and the manual-check script are in
[docs/phase0-spike.md](docs/phase0-spike.md).
