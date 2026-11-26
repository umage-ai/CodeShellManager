//! CodeShellManager services.
//!
//! Everything here is a function of its inputs (no process spawn, no file or network I/O),
//! except [`git::run`] and [`git::watch`], the git process layer and the `.git` watcher,
//! which block and must be called from a worker thread.
//! The security-critical rules of the WPF app live here (see the repo's CLAUDE.md):
//!
//! * [`argv`]: MSVCRT argv quoting and splitting. Never interpolate a value into a command line.
//! * [`launch`]: ssh / wsl.exe / run-command argument builders and launch validation.
//! * [`wsl`]: `wsl -l -v` parsing and the `\wsl$` UNC mirror.
//! * [`git`]: git command lines, `status --porcelain=v2` parsing, wording, worktree rules.
//! * [`open_with`]: "Open with..." templates, validation and sanitising.
//! * [`wt_profiles`]: Windows Terminal `settings.json` profiles (parsing is pure; [`wt_profiles::load_profiles`] reads files).
//! * [`autostart`]: the OS's start-at-sign-in entry (Run key, LaunchAgent, XDG autostart; does I/O).
//! * [`explorer_menu`]: the per-user off switch of Explorer's "Open in CodeShellManager" entry (registry; does I/O).
//! * [`project_docs`]: the README/CLAUDE/AGENTS docs of a folder (reads files; call from a worker).
//! * [`update`]: the update check's one HTTPS request (blocking; call from a worker).
//! * [`claude_usage`]: Claude Code token and cost analytics (parsing and pricing are pure;
//!   `discover` and `tail` read transcripts and run on workers).
//!
//! The input types defined locally ([`launch::LaunchSpec`], [`open_with::OpenWithEntry`],
//! [`git::WorktreeInfo`], ...) are the minimum these modules need; unify them with `csm-core`
//! models later.
#![forbid(unsafe_code)]
#![deny(clippy::indexing_slicing)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

pub mod addons;
pub mod argv;
pub mod autostart;
pub mod claude_usage;
pub mod explorer_menu;
pub mod git;
pub mod highlight;
pub mod launch;
pub mod open_with;
pub mod plugins;
pub mod ssh_ports;
pub mod project_docs;
pub mod secrets;
pub mod update;
pub mod winpath;
pub mod wsl;
pub mod wt_profiles;
