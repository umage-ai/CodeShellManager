# Quitting

`app_quit` (the window close, the tray menu, `csm app_quit`, MCP) runs one sequence in
`csm-engine` (`lifecycle.rs` `quit`, `quit_pool.rs`, `claude_config.rs`). It never blocks the UI
thread: a worker thread does the waiting and the shutdown board follows `QuitProgress` events.

1. Every session that is not Claude is stopped at once.
2. Claude's config files (`~/.claude.json`, and the `.claude.json` of each Claude config folder
   found) are copied into `claude-config-backups` beside `state.json` (the last three copies of
   each are kept). Not done with `--clean`, and never for a file that is missing or already
   unreadable.
3. Claude sessions are asked to exit (the pseudo console is closed) in a bounded pool: up to
   `QuitClaudeConcurrency` at a time (Settings > General; default 4, 1 to 8; 1 is the 0.x
   behaviour, one after another). As one finishes (exited cleanly, stopped after its own
   `shutdown_budget` of 15 s, or already gone) the next in sidebar order starts. Starts are spaced
   by `ClaudeLaunchStaggerMs / slots` (500 ms at the defaults) so exits do not land in the same
   instant; with one slot the flat stagger follows each clean exit instead.
4. Only after the last Claude has exited, each config file is read again. A file that still parses
   (valid JSON object) is left alone. One that is torn, empty or gone is put back from its copy
   (the torn one is kept as `*.corrupt.json` beside the copies) and the quit logs it.
5. The state is saved and `QuitReady` is signalled.

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
