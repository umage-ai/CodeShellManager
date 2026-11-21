//! The Review add-on: a process add-on with one screen (docs/addons.md, "Screens"). It shows the
//! changed files of a session's repository, the diff of each, and any file as code, read-only, and
//! lets the person comment on lines. The threads live in the add-on's own data folder; agents list,
//! read, reply to and mark them addressed through the `review.*` tools. Only the person resolves.
//!
//! Git is read through CSM's own tools (`git_diff`, `git_show_file`, `git_ls_files`, `git_status`
//! style) over `csm/call`; this program starts no process and opens no file but its data folder.
//!
//! Modules: [`anchor`] (where a comment belongs, and re-finding it), [`store`] (the threads on
//! disk), [`prompt`] ("Send to agent"), [`view`] (the view tree) and [`host`] (the protocol).
#![forbid(unsafe_code)]
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable, clippy::indexing_slicing)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing))]

pub mod anchor;
pub mod host;
pub mod prompt;
pub mod store;
pub mod view;
