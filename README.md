# CodeShellManager (Rust)

Cross-platform rewrite of [CodeShellManager](../CodeShellManager): a multi-terminal host for
Claude Code and other CLI tools. egui + wgpu, Windows / macOS / Linux.

Plan, decisions and phases: [docs/plan.md](docs/plan.md).

```sh
pwsh scripts/get-conpty.ps1          # once: bundled ConPTY into .conpty/ (not committed)
cargo build
cargo test
cargo run --release -p term-spike    # Phase 0 terminal spike
```

Phase 0 spike: usage, measured results and the manual-check script are in
[docs/phase0-spike.md](docs/phase0-spike.md).
