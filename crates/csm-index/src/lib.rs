//! `csm-index`: the search index, per-folder notes and session history, in one SQLite file.
//!
//! **One thread owns the connection.** The WPF app serialised every use of a shared
//! `SqliteConnection` with `DbGate` (issue #102: the connection is not thread-safe, concurrent
//! create/dispose corrupted its command list). Here there is nothing to gate: the connection
//! lives on the `csm-index` thread and everything else talks to it through [`Index`], a cheap
//! `Clone` handle over a channel.
//!
//! - **Output** ([`Index::feed`]) is fire-and-forget from the PTY reader thread: raw bytes are
//!   queued (bounded by bytes; over the bound a chunk is dropped and counted, never waited for),
//!   and the DB thread strips ANSI, cuts lines, batches and inserts them on a debounce.
//! - **Everything else** is a closure run on the DB thread with the answer delivered to a
//!   callback ([`Index::call`]), so an engine tool can park its reply and never block.
//! - **A query sees everything fed before it**: the thread flushes pending output first.
//! - Storage is versioned (`PRAGMA user_version`); a file that cannot be opened is moved aside
//!   and a fresh one created; if even that fails the index runs in memory.
#![forbid(unsafe_code)]
#![cfg_attr(test, allow(clippy::unwrap_used, clippy::expect_used, clippy::panic))]

mod clean;
mod db;
mod handle;

pub use clean::{LineCleaner, normalize_folder};
pub use db::{Db, IndexError, NoteHit, NoteRow, OutputHit, SCHEMA_VERSION, SchemaStats, fts_phrase};
pub use handle::{Index, IndexLocation, IndexOptions};
