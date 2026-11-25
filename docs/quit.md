# Quitting

`app_quit` (the window close, the tray menu, `csm app-quit`, MCP) runs one sequence in
`csm-engine` (`lifecycle.rs` `quit`, `quit_pool.rs`, `claude_config.rs`). It never blocks the UI
thread: a worker thread does the waiting and the shutdown board follows `QuitProgress` events.

1. Every session that is not Claude is stopped at once.
2. Claude's config files (`~/.claude.json`, and the `.claude.json` of each Claude config folder
   found) are copied into `claude-config-backups` (the last three copies of each are kept, and
   the last three torn files kept for a look). In a default install the folder is in the local
   app data (`%LOCALAPPDATA%\CodeShellManager2\claude-config-backups`, so it does not roam); with a
   portable install or `--config-dir` it is beside `state.json`. A copy is named after the config's
   folder and a short hash of its full path, so two profiles in folders both called `.claude` never
   share a copy, and on Unix the folder is 0700 and the files 0600. Not done with `--clean`, and
   never for a file that is missing or already unreadable.
3. Claude sessions are asked to exit (the pseudo console is closed) in a bounded pool: up to
   `QuitClaudeConcurrency` at a time (Settings > General; default 4, 1 to 8; 1 is the 0.x
   behaviour, one after another). As one finishes (exited cleanly, stopped after its own
   `shutdown_budget` of 15 s, or already gone) the next in sidebar order starts. Starts are spaced
   by `ClaudeLaunchStaggerMs / slots` (500 ms at the defaults) so exits do not land in the same
   instant; with one slot the flat stagger follows each clean exit instead.
4. Only after the last Claude has exited, each config file is read again. That includes the old
   Claude of a restart that is still in flight: the quit waits for restart workers (their own exit
   wait, the pause after it and two seconds, at most) before it checks the file, and logs it when
   one is still going after that. A file that still parses (valid JSON object) is left alone. One
   that is torn, empty or gone is put back from its copy (the torn one is kept as
   `*.corrupt-<ms>.json` beside the copies) and the quit logs it.
5. The state is saved and `QuitReady` is signalled. If saving keeps failing (a full disk) the quit
   still ends after one last try; see "When saving fails" below.

A restart whose worker thread cannot be started does not leave the session stuck: the old process
is stopped on the spot, the session goes to sleep, the restart reports the failure and the
"a restart is already running" guard is released.

"Force close now" (`app_quit { force: true }`, also while a quit is already running) stops every
wait: sessions in flight and queued are stopped and reported as forced.

## Why a pool, and what it costs

The Claude CLI rewrites `~/.claude.json` unlocked on exit (see `docs/wpf/lessons.md`, "Settings",
`ClaudeLaunchStaggerMs`), so overlapping exits can lose each other's updates and, rarely, leave
the file truncated. One at a time is safe but a quit over about 70 Claude sessions took 5 to 8
minutes. With four at a time it takes roughly a minute and a half (70 sessions at 2.3 to 4.7 s
each is about 18 waves; a wedged session adds up to 15 s to its slot only). The accepted
trade-off: an individual update to the config (a startup counter, a project's last-used
details) can be lost; a corrupt file cannot survive, because of the copy in step 2 and the check
in step 4. If you would rather not take the risk, set the number to 1.

The board's budget bar (`quit_budget_ms` in the snapshot) is the worst case: waves of
`shutdown_budget` plus the start gaps. The board's explanation line, the "next" and "waiting for
Claude to exit" row texts and the number of rows closing at once follow `quit_slots`.

## When saving fails, and when state.json cannot be read

Persistence fails loudly and safely (`persist.rs`, `warnings.rs`):

- **A save that cannot be written** (a full disk, a locked folder) used to be only logged. Now the
  saver reports it (`SaveHealth`), keeps the newest state pending and retries after 2 s, doubling
  up to 60 s, until a save works or a newer state replaces it. While it fails, a red strip under
  the title bar says "Your sessions are NOT being saved" with the error (`Snapshot::warnings`,
  `state_save_failed`; the Board's `tasks.json` has its own, `tasks_save_failed`). The first
  save that works removes the strip and shows a toast. The quit gets one last try and ends.
- **A `state.json` that is there but cannot be read** (a sharing violation from a backup tool or
  antivirus, access denied) is not the same as a corrupt one. The read is retried four times
  (200 ms apart); if it still fails the run starts **read-only**: empty, saving nothing, moving
  nothing aside, so the real file is never replaced by an empty state. A strip says so
  (`state_read_only`); close what holds the file and restart. A file that is read but does not
  parse is unchanged: moved aside as `*.bad-<ms>`, the `.bak` is tried (issue #166). The same
  rule holds for `tasks.json`, `~/.claude.json` copies and the code review store: only "not
  found" means "no file".
- **The 0.x API key** (`AnthropicApiKey`) is not used by version 2. It is never written to
  `state.json`, its `.bak`, the snapshots or an export; on start it is removed from those files
  if an older build left one there. Importing a 0.x backup still reads it, into memory only.
