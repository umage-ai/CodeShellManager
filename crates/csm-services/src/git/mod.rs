//! Git: command lines (argv only), output parsing, user-facing wording and worktree rules.
//!
//! [`run`] and [`watch`] are the I/O half (they spawn git and watch `.git`; call them from a
//! worker thread); everything else is pure, and no process is spawned there. The invariants this carries over from the WPF `GitService`:
//!
//! * Values go in as argv elements and the process is exec'd directly; no shell. Under WSL the
//!   form is `-e sh -lc 'exec "$0" "$@"' git ...` ([`args::build_wsl_git_command_line`]).
//! * `--` before repo-controlled positionals; `switch --no-guess`; `branch -d` not `-D`;
//!   `worktree remove` without `--force`.
//! * `status --porcelain=v2 --branch` is the one call that feeds the chip, the poll and the menu.

pub mod args;
pub mod run;
pub mod status;
pub mod text;
pub mod watch;
pub mod worktrees;

pub use args::*;
pub use status::{GitFileChange, GitOpResult, GitRepoStatus, git_info, parse_status_v2, unquote_c_path};
pub use text::{combine_output, failure_hint, fetch_summary, menu_header, summarize_pull, sync_text};
pub use worktrees::{WorktreeInfo, parse_worktree_list};
