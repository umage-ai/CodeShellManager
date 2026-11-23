//! Session lifecycle on the engine thread: launch, spawn, exit, sleep, wake, close, restart,
//! the restore loop, quit, and the after-step publish (snapshot, events, waits).
//!
//! Invariants carried over from the WPF app (its CLAUDE.md):
//! - Slow work (spawning, waiting for an exit, the Claude stagger) runs on workers and re-enters
//!   with `engine.post`. Nothing here blocks.
//! - Exits are judged with `has_exited`, never "do we hold a handle".
//! - Every relaunch path (restore, wake, restart) ends a failure with the session **asleep**,
//!   never deleted.
//! - Claude launches are spaced by a flat `ClaudeLaunchStaggerMs` (never an adaptive wait), and a
//!   Claude restart waits for the old process to exit, then the same flat delay, before the new
//!   one starts: two `claude` processes rewriting `~/.claude.json` at once lose updates.
//! - A restart re-checks "is the app closing" after its waits and before relaunching, so it can
//!   never start a process nothing will dispose.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

use csm_core::models::{ProgramStatusProbeMode, RecentlyClosedEntry, SessionStatus};
use csm_core::program_status_payload::{self as psp, ProgramState};
use csm_core::shell_integration as si;
use csm_core::time::Timestamp;
use csm_pty::{Backend, Pty, PtySize, SpawnOptions};
use csm_term::{GridSize, ProbeReply, PromptMark, TapEvent, TermEvent};
use serde_json::json;

use crate::engine::{Ctx, Engine, ParkedReply};
use crate::events::Event;
use crate::session::{Phase, SessionIo, SessionRt, StartedCb, now_ms};
use crate::snapshot;
use crate::state::{Progress, State};
use crate::status::Status;
use crate::tool::{ToolError, ToolOutput};

/// `MainViewModel.MaxRecentlyClosed`.
pub(crate) const MAX_RECENTLY_CLOSED: usize = 10;
/// At most one status toast per session per this interval (`ProgramStatusToastInterval`).
const TOAST_INTERVAL: Duration = Duration::from_secs(5);

pub(crate) fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub(crate) fn session_index(state: &State, id: &str) -> Option<usize> {
    state.app.sessions.iter().position(|s| s.id == id)
}

fn display_name(state: &State, id: &str) -> String {
    state
        .app
        .sessions
        .iter()
        .find(|s| s.id == id)
        .map(|s| if s.name.trim().is_empty() { s.default_display_name() } else { s.name.clone() })
        .unwrap_or_else(|| id.to_owned())
}

/// Tells the search index which session a stream of output belongs to, and keeps the config
/// snapshot a closed session can be reopened from. Both are queued, never waited for.
pub(crate) fn record_in_index(ix: &csm_index::Index, name: &str, s: &csm_core::models::ShellSession) {
    let folder = s.working_folder.as_str();
    ix.note_session(&s.id, name, folder, &s.group_id);
    let entry = RecentlyClosedEntry::from_session(s, Timestamp::from_epoch_ms(now_ms()), new_id);
    if let Ok(json) = serde_json::to_string(&entry) {
        ix.record_history(&s.id, name, folder, &json);
    }
}

fn is_claude(state: &State, id: &str) -> bool {
    state.app.sessions.iter().find(|s| s.id == id).is_some_and(|s| crate::launch::is_claude_command(&s.command))
}

// ── Start, tick, exit ────────────────────────────────────────────────────────────────────

/// First thing on the engine thread: pick the ConPTY, resolve it off-thread, restore sessions.
pub(crate) fn on_start(ctx: &mut Ctx<'_>) {
    csm_pty::set_prefer_bundled(ctx.state.app.settings.use_bundled_con_pty);
    let cell = ctx.state.rt.conpty.clone();
    let _ = thread::Builder::new().name("csm-conpty-select".into()).spawn(move || {
        let text = match csm_pty::conpty_selection() {
            Some((b, why)) => format!("{} ({why})", b.label()),
            None => Backend::Unix.label().to_owned(),
        };
        log::info!("PTY: {text}");
        let _ = cell.set(text);
    });
    crate::tools::open::on_start(ctx);
    crate::addons::reload(ctx);
    if ctx.state.options.restore_on_start {
        begin_restore(ctx);
    }
}

/// The restore loop: every session that was live is launched again (Claude ones resume their
/// conversation), staggered by the Claude gate; the rail shows progress until all have started.
pub(crate) fn begin_restore(ctx: &mut Ctx<'_>) {
    let ids: Vec<String> = ctx.state.app.sessions.iter().filter(|s| !s.is_dormant).map(|s| s.id.clone()).collect();
    if ids.is_empty() {
        return;
    }
    if !ctx.state.app.settings.auto_restore_sessions {
        for s in &mut ctx.state.app.sessions {
            s.is_dormant = true;
        }
        ctx.state.persist();
        return;
    }
    // Sessions from an add-on profile need the add-on catalogue, which is still being read on a
    // worker at start. They wait for it (`restore_waiting_addons`); the rest start now.
    let scanning = ctx.state.rt.addons.scanning;
    let mut now = Vec::new();
    for id in ids {
        if scanning && !profile_of(ctx.state, &id).is_empty() {
            ctx.state.rt.addon_restore_waiting.push(id);
        } else {
            now.push(id);
        }
    }
    restore_sessions(ctx, &now);
    ctx.state.persist();
}

/// Called when an add-on scan finishes: the sessions that waited for it start (or stay asleep)
/// now.
pub(crate) fn restore_waiting_addons(ctx: &mut Ctx<'_>) {
    let ids = std::mem::take(&mut ctx.state.rt.addon_restore_waiting);
    if ids.is_empty() {
        return;
    }
    restore_sessions(ctx, &ids);
    ctx.state.persist();
}

/// The add-on profile a session was launched from (empty for one the person started by hand).
fn profile_of(state: &State, id: &str) -> String {
    state.app.sessions.iter().find(|s| s.id == id).map(|s| s.addon_profile.clone()).unwrap_or_default()
}

/// Launches each session again, except one from an add-on profile that is off or has resume
/// "none": that one comes back asleep, so nothing from an add-on runs on restore unless its
/// profile says so.
fn restore_sessions(ctx: &mut Ctx<'_>, ids: &[String]) {
    for id in ids {
        let profile = profile_of(ctx.state, id);
        let rerun = ctx.state.app.sessions.iter().any(|s| s.id == *id && s.resume_rerun);
        let outcome = if rerun || crate::addons::restores_on_start(&ctx.state.rt.addons, &profile) {
            launch(ctx, id, true)
        } else {
            Err(format!("its add-on profile `{profile}` is off or does not restore on start"))
        };
        match outcome {
            Ok(()) => {
                ctx.state.rt.restore_pending.insert(id.clone());
            }
            Err(msg) => {
                log::info!("restore: {id} not started: {msg}");
                if let Some(i) = session_index(ctx.state, id)
                    && let Some(s) = ctx.state.app.sessions.get_mut(i)
                {
                    s.is_dormant = true;
                }
            }
        }
    }
    let pending = ctx.state.rt.restore_pending.len();
    if pending > 0 {
        let done = ctx.state.rt.restore.as_ref().map_or(0, |p| p.done);
        ctx.state.rt.restore = Some(Progress { done, total: done + pending });
    }
}

fn finish_restore_item(state: &mut State, id: &str) {
    if state.rt.restore_pending.remove(id) {
        if let Some(p) = &mut state.rt.restore {
            p.done = (p.done + 1).min(p.total);
        }
        if state.rt.restore_pending.is_empty() {
            state.rt.restore = None;
            log::info!("restore: done");
        }
        state.touch();
    }
}

/// Runs on every engine wake-up: fallback starts, the screen heuristic, wait timeouts, saves.
pub(crate) fn tick(ctx: &mut Ctx<'_>) {
    let now = Instant::now();
    let now_ms = now_ms();
    crate::usage_tail::tick(ctx.state);
    crate::tools::update::tick(ctx);
    crate::tools::storage::tick(ctx);

    // A session nobody is looking at starts at the fallback size.
    let due: Vec<(String, u64)> = ctx
        .state
        .rt
        .sessions
        .iter()
        .filter(|(_, rt)| rt.phase == Phase::Starting && now >= rt.start_deadline)
        .filter(|(_, rt)| !rt.io.spawn_requested.load(Ordering::Acquire))
        .map(|(id, rt)| (id.clone(), rt.io.instance))
        .collect();
    for (id, instance) in due {
        start_spawn(ctx, &id, instance);
    }

    // The screen heuristic, for Claude sessions that don't report their status. A plain shell
    // sitting at its prompt (`C:\work>`, `$ `) is idle, not "needs you": the WPF app flagged every
    // waiting shell, which with an inbox and toasts is noise.
    let claude_ids: std::collections::HashSet<String> =
        ctx.state.app.sessions.iter().filter(|s| crate::launch::is_claude_command(&s.command)).map(|s| s.id.clone()).collect();
    for (id, rt) in ctx.state.rt.sessions.iter_mut() {
        if rt.has_program_status || rt.phase != Phase::Live || !claude_ids.contains(id) {
            continue;
        }
        let alert = rt.io.detector().poll(now_ms);
        if let Some(a) = alert {
            rt.heuristic = Some(a.alert_type);
            rt.heuristic_message = Some(a.message);
            ctx.state.rt.dirty = true;
        }
    }

    // Profile status rules (add-ons): a session launched from a profile with status rules has its
    // screen read on a worker about once a second; the rule that matches sets its status.
    let rules_for: Vec<(String, Vec<csm_core::addons::StatusRule>)> = ctx
        .state
        .app
        .sessions
        .iter()
        .filter(|s| !s.addon_profile.is_empty())
        .filter_map(|s| crate::addons::status_rules(&ctx.state.rt.addons, &s.addon_profile).map(|r| (s.id.clone(), r)))
        .collect();
    // A profile whose add-on is now off (or has no rules) no longer sets a status: drop the last one.
    let unruled: Vec<String> = ctx
        .state
        .app
        .sessions
        .iter()
        .filter(|s| !s.addon_profile.is_empty() && crate::addons::status_rules(&ctx.state.rt.addons, &s.addon_profile).is_none())
        .map(|s| s.id.clone())
        .collect();
    for id in unruled {
        if let Some(rt) = ctx.state.rt.sessions.get_mut(&id)
            && rt.addon_status.take().is_some()
        {
            ctx.state.rt.dirty = true;
        }
    }
    for (id, rules) in rules_for {
        let Some(rt) = ctx.state.rt.sessions.get_mut(&id) else { continue };
        // One read at a time per session: a read still in flight is not joined by another.
        if rt.has_program_status || rt.phase != Phase::Live || now_ms - rt.addon_checked_ms < 1000 || rt.addon_busy.load(Ordering::Acquire) {
            continue;
        }
        rt.addon_checked_ms = now_ms;
        let io = rt.io.clone();
        let instance = io.instance;
        let engine = ctx.engine.clone();
        let busy = rt.addon_busy.clone();
        busy.store(true, Ordering::Release);
        let worker_busy = busy.clone();
        let spawned = std::thread::Builder::new().name("csm-addon-status".into()).spawn(move || {
            let text = crate::screen::screen(&mut io.lock_terminal());
            let status = crate::addons::screen_status(&rules, &text);
            worker_busy.store(false, Ordering::Release);
            engine.post(Box::new(move |ctx: &mut Ctx<'_>| {
                if let Some(rt) = ctx.state.rt.sessions.get_mut(&id)
                    && rt.io.instance == instance
                    && rt.addon_status != status
                {
                    rt.addon_status = status;
                    ctx.state.rt.dirty = true;
                }
            }));
        });
        if spawned.is_err() {
            busy.store(false, Ordering::Release);
        }
    }

    // Waits that ran out of time.
    if ctx.state.rt.waits.iter().any(|w| now >= w.deadline) {
        let waits = std::mem::take(&mut ctx.state.rt.waits);
        for w in waits {
            if now >= w.deadline {
                let status = ctx.state.rt.prev.session(&w.session_id).map_or("gone", |v| v.status.id());
                w.reply.send(Err(ToolError::TimedOut(format!("the session did not reach `{}` in time; it is {status}", until_id(w.until)))));
            } else {
                ctx.state.rt.waits.push(w);
            }
        }
    }

    // Git: track live sessions, start due status probes (on workers).
    crate::tools::git::tick(ctx);

    if ctx.state.rt.save_due.is_some_and(|d| now >= d) {
        ctx.state.rt.save_due = None;
        save_now(ctx.state);
    }
    if ctx.state.rt.tasks_save_due.is_some_and(|d| now >= d) {
        ctx.state.rt.tasks_save_due = None;
        save_tasks_now(ctx.state);
    }
}

fn until_id(u: crate::status::WaitUntil) -> &'static str {
    use crate::status::WaitUntil as W;
    match u {
        W::Idle => "idle",
        W::Done => "done",
        W::NeedsYou => "needs_you",
        W::Exit => "exit",
        W::AnyChange => "any_change",
    }
}

fn state_json(state: &State) -> Option<String> {
    let mut app = state.app.clone();
    app.ui.focused_session = state.rt.focused.clone();
    app.csm_rs_version = Some(crate::persist::STATE_FILE_VERSION);
    match app.to_json() {
        Ok(t) => Some(t),
        Err(e) => {
            log::error!("state: could not serialise: {e}");
            None
        }
    }
}

/// Hands the tasks to their saver thread (no file I/O here).
pub(crate) fn save_tasks_now(state: &mut State) {
    let Some(saver) = &state.rt.tasks_saver else { return };
    match state.rt.tasks.to_json() {
        Ok(t) => saver.save(t),
        Err(e) => log::error!("tasks: could not serialise: {e}"),
    }
}

/// Hands the current state to the saver thread (no file I/O here).
pub(crate) fn save_now(state: &mut State) {
    if state.rt.saver.is_none() {
        return;
    }
    if let Some(text) = state_json(state)
        && let Some(saver) = &state.rt.saver
    {
        saver.save(text);
    }
}

/// The engine thread is ending: kill what is left and write the state one last time.
pub(crate) fn on_exit(state: &mut State) {
    crate::runs::kill_everything(state);
    crate::port_forward::stop_all(&mut state.rt.forwards);
    // Usage stats: what still runs ends now (queued before the index's shutdown, so written).
    let index = state.rt.index.clone();
    for rt in state.rt.sessions.values_mut() {
        rt.end_usage(index.as_ref());
    }
    if let Some(mut u) = state.rt.usage.take() {
        u.shutdown();
    }
    if let Some(ix) = state.rt.index.take() {
        ix.shutdown();
    }
    let ids: Vec<String> = state.rt.sessions.keys().cloned().collect();
    for id in ids {
        if let Some(rt) = state.rt.sessions.remove(&id) {
            rt.io.closed.store(true, Ordering::Release);
            if let Some(p) = rt.io.pty.get() {
                p.kill();
            }
        }
    }
    for w in std::mem::take(&mut state.rt.waits) {
        w.reply.send(Err(ToolError::Failed("the app is closing".into())));
    }
    let text = if state.rt.saver.is_some() { state_json(state) } else { None };
    if let Some(saver) = state.rt.saver.take() {
        saver.finish(text);
    }
    let tasks = if state.rt.tasks_saver.is_some() { state.rt.tasks.to_json().ok() } else { None };
    if let Some(saver) = state.rt.tasks_saver.take() {
        saver.finish(tasks);
    }
}

// ── After every step: snapshot, events, waits ────────────────────────────────────────────

/// Rebuilds and republishes the snapshot when something changed, and derives the events from
/// the difference, so no tool can forget one: `SessionChanged` / `SessionRemoved` per session,
/// `InboxChanged`, `LayoutChanged`, status toasts, and `session_wait` completions.
pub(crate) fn after_step(state: &mut State, engine: &Engine) {
    publish_step(state, engine);
    // Replies parked during this step go out only now, after the snapshot that reflects them.
    crate::engine::flush_deferred_replies();
}

fn publish_step(state: &mut State, engine: &Engine) {
    if !state.rt.dirty {
        return;
    }
    state.rt.dirty = false;
    let prev = state.rt.prev.clone();
    let snap = Arc::new(snapshot::build(state, prev.version.wrapping_add(1)));
    let events = engine.events();

    for s in &snap.sessions {
        let before = prev.session(&s.id);
        if before == Some(s) {
            continue;
        }
        events.emit(Event::SessionChanged { session_id: s.id.clone() });
        let was = before.map(|b| b.status);
        if s.status.in_inbox() && was != Some(s.status) && was.is_some() {
            toast_status(state, engine, s);
        }
    }
    for p in &prev.sessions {
        if snap.session(&p.id).is_none() {
            events.emit(Event::SessionRemoved { session_id: p.id.clone() });
        }
    }
    if prev.inbox != snap.inbox {
        events.emit(Event::InboxChanged);
    }
    let layout_changed = prev.groups != snap.groups
        || prev.view != snap.view
        || prev.rail_tab != snap.rail_tab
        || prev.layouts != snap.layouts
        || prev.layout_key != snap.layout_key
        || prev.focused != snap.focused
        || prev.inbox_open != snap.inbox_open
        || prev.inbox_height != snap.inbox_height
        || prev.inspector_width != snap.inspector_width
        || prev.inspector != snap.inspector
        || prev.restore != snap.restore
        || prev.restart != snap.restart
        || prev.quit != snap.quit
        || prev.quit_budget_ms != snap.quit_budget_ms
        || prev.sessions.iter().map(|s| &s.id).ne(snap.sessions.iter().map(|s| &s.id));
    if layout_changed {
        events.emit(Event::LayoutChanged);
    }

    state.rt.prev = snap.clone();
    engine.publish(snap.clone());

    // Waits whose condition is now met (or whose session is gone).
    if !state.rt.waits.is_empty() {
        let waits = std::mem::take(&mut state.rt.waits);
        for w in waits {
            match snap.session(&w.session_id) {
                None => w.reply.send(Err(ToolError::NotFound("the session was closed while waiting".into()))),
                Some(v) if w.until.reached(v.status, w.start) => w.reply.send(Ok(wait_output(v))),
                Some(_) => state.rt.waits.push(w),
            }
        }
    }
}

pub(crate) fn wait_output(v: &snapshot::SessionView) -> ToolOutput {
    ToolOutput::new(
        format!("{} is {}.", v.name, v.status.label().to_lowercase()),
        json!({"session_id": v.id, "status": v.status, "progress": v.progress, "message": v.status_message}),
    )
}

fn toast_status(state: &mut State, engine: &Engine, s: &snapshot::SessionView) {
    let now = Instant::now();
    if state.rt.last_toast.get(&s.id).is_some_and(|t| now.duration_since(*t) < TOAST_INTERVAL) {
        return;
    }
    state.rt.last_toast.insert(s.id.clone(), now);
    let message = match s.status {
        Status::NeedsYouApproval => format!("{} needs your approval.", s.name),
        Status::NeedsYouQuestion => format!("{} needs you.", s.name),
        Status::Error => format!("{} reported an error.", s.name),
        _ => format!("{} finished.", s.name),
    };
    engine.events().toast(message);
}

// ── Launch and spawn ─────────────────────────────────────────────────────────────────────

/// Creates the runtime of a session in the `starting` state. The PTY is spawned when the UI
/// reports the first measured size, or at the fallback size after `first_size_timeout`.
pub(crate) fn launch(ctx: &mut Ctx<'_>, id: &str, restoring: bool) -> Result<(), String> {
    if ctx.state.rt.shutting_down {
        return Err("the app is closing".into());
    }
    let Some(i) = session_index(ctx.state, id) else {
        return Err("no such session".into());
    };
    let Some(s) = ctx.state.app.sessions.get(i) else {
        return Err("no such session".into());
    };
    if let Some(msg) = s.launch_validation_error() {
        return Err(msg.to_owned());
    }
    let name = if s.name.trim().is_empty() { s.default_display_name() } else { s.name.clone() };
    if let Some(mut old) = ctx.state.rt.sessions.remove(id) {
        old.end_usage(ctx.state.rt.index.as_ref());
        old.io.closed.store(true, Ordering::Release);
        if let Some(p) = old.io.pty.get() {
            p.kill();
        }
    }
    ctx.state.rt.instance_seq = ctx.state.rt.instance_seq.wrapping_add(1);
    let (cols, rows) = ctx.state.options.fallback_size;
    let index = ctx.state.rt.index.clone();
    if let Some(ix) = &index {
        record_in_index(ix, &name, s);
    }
    let look = csm_core::look::Look::from_session(s);
    let io = SessionIo::new(id, &name, ctx.state.rt.instance_seq, GridSize::new(cols, rows), ctx.engine.clone(), index);
    if !look.is_empty() {
        crate::tools::look::apply_to_terminal(&mut io.lock_terminal(), &look);
    }
    let deadline = Instant::now() + ctx.state.options.first_size_timeout;
    ctx.state.rt.sessions.insert(id.to_owned(), SessionRt::new(io, restoring, deadline));
    ctx.state.touch();
    Ok(())
}

/// Starts the spawn worker (once per runtime). Called when the UI reported a size, or by the
/// tick when the fallback deadline passed.
pub(crate) fn start_spawn(ctx: &mut Ctx<'_>, id: &str, instance: u64) {
    let state = &mut *ctx.state;
    let Some(rt) = state.rt.sessions.get(id) else { return };
    if rt.io.instance != instance || rt.phase != Phase::Starting {
        return;
    }
    rt.io.spawn_requested.store(true, Ordering::Release);
    let Some(session) = state.app.sessions.iter().find(|s| s.id == id) else { return };
    // `--name '<session name>'` for Claude (#76, opt-in): on the copy that is launched only.
    let session = if state.app.settings.pass_session_name_to_claude { crate::launch::with_claude_name(session) } else { session.clone() };
    let size = {
        let mut first = rt.io.first_size.lock().unwrap_or_else(PoisonError::into_inner);
        *first.get_or_insert_with(|| {
            let (c, r) = state.options.fallback_size;
            GridSize::new(c, r)
        })
    };
    // Another Claude session in this folder: a resume never adopts the folder's newest transcript.
    let sole_in_folder = !state.app.sessions.iter().any(|o| {
        o.id != session.id
            && o.kind == csm_core::models::SessionKind::Local
            && crate::launch::is_claude_command(&o.command)
            && csm_services::claude_usage::discover::same_folder(&o.working_folder, &session.working_folder)
    });
    let job = SpawnJob {
        io: rt.io.clone(),
        session,
        size,
        factory: state.options.pty.clone(),
        gate: state.rt.claude_gate.clone(),
        stagger: Duration::from_millis(u64::try_from(state.app.settings.claude_launch_stagger_ms).unwrap_or(0)),
        resume: rt.restoring && state.app.settings.auto_resume_claude,
        sole_in_folder,
        probe: state.app.settings.program_status_probe,
        mcp: state.app.settings.offer_mcp_to_claude.then(|| mcp_offer_dir(state)).flatten().and_then(crate::launch::McpOffer::beside_app),
        engine: ctx.engine.clone(),
    };
    let (id2, engine) = (id.to_owned(), ctx.engine.clone());
    let spawned = thread::Builder::new().name("csm-spawn".into()).spawn(move || {
        let result = job.run();
        engine.post(Box::new(move |ctx| on_spawned(ctx, &id2, instance, result)));
    });
    if let Err(e) = spawned {
        on_spawned(ctx, id, instance, Err(format!("could not start a worker thread: {e}")));
    }
}

struct SpawnJob {
    io: Arc<SessionIo>,
    session: csm_core::models::ShellSession,
    size: GridSize,
    factory: crate::state::PtyFactory,
    gate: Arc<Mutex<Option<Instant>>>,
    stagger: Duration,
    resume: bool,
    /// No other CSM Claude session works in this folder (a resume may adopt a newer transcript
    /// of the folder: a `/clear` in Claude).
    sole_in_folder: bool,
    probe: ProgramStatusProbeMode,
    /// Set when Settings → MCP offers our tools to Claude sessions.
    mcp: Option<crate::launch::McpOffer>,
    engine: Engine,
}

impl SpawnJob {
    /// On a worker: resolve argv, wait for the Claude slot, spawn, wire the reader and exit.
    fn run(self) -> Result<Spawned, String> {
        let plan = crate::launch::resolve_with(&self.session, self.resume, self.sole_in_folder, self.mcp.as_ref())?;
        let claude_session_id = plan.claude_session_id.clone();
        if plan.is_claude && !self.stagger.is_zero() {
            wait_for_claude_slot(&self.gate, self.stagger);
        }
        if self.io.closed.load(Ordering::Acquire) {
            return Err("the session was closed before it started".into());
        }
        let program = plan.argv.first().cloned().unwrap_or_default();
        let mut opts = SpawnOptions::new(plan.argv.into_iter().map(OsString::from).collect(), PtySize::new(self.size.cols, self.size.rows));
        opts.cwd = plan.cwd;
        if let Some(path) = plan.path {
            opts.env.push(("PATH".into(), Some(path)));
        }
        // Issue #162: never hand a session the launching terminal's session markers.
        opts.env.extend(crate::launch::inherited_env_changes());
        opts.env.push(("TERM".into(), Some("xterm-256color".into())));
        opts.env.push(("COLORTERM".into(), Some("truecolor".into())));
        opts.env.push(("CSM_SESSION_ID".into(), Some(self.session.id.clone().into())));
        log::info!("spawning session {} ({program}) at {}x{}", self.session.id, self.size.cols, self.size.rows);
        let pty = (self.factory)(opts).map_err(|e| format!("could not start `{program}`: {e}"))?;
        if self.io.closed.load(Ordering::Acquire) {
            pty.kill();
            return Err("the session was closed before it started".into());
        }
        let reply = match (self.probe, pty.backend()) {
            (ProgramStatusProbeMode::Off, _) => ProbeReply::Off,
            (ProgramStatusProbeMode::Auto, Backend::InboxConPty) => ProbeReply::Off,
            (ProgramStatusProbeMode::On, Backend::InboxConPty) => ProbeReply::Win32InputMode,
            _ => ProbeReply::Raw,
        };
        let current = {
            let mut t = self.io.lock_terminal();
            t.set_probe_reply(reply);
            t.size()
        };
        let _ = self.io.pty.set(pty.clone());
        // The pane may have been resized while the process was starting.
        if (current.cols, current.rows) != (self.size.cols, self.size.rows) {
            let _ = pty.resize(PtySize::new(current.cols, current.rows));
        }
        pty.subscribe(self.io.sink());
        let (id, instance, engine) = (self.io.id.clone(), self.io.instance, self.engine.clone());
        pty.on_exit(Box::new(move |code| {
            engine.post(Box::new(move |ctx| on_exited(ctx, &id, instance, code)));
        }));
        Ok(Spawned { pty, claude_session_id })
    }
}

/// A started process and the Claude conversation it runs, when known.
pub(crate) struct Spawned {
    pub pty: Arc<dyn Pty>,
    pub claude_session_id: Option<String>,
}

/// Flat stagger between Claude launches: reserve the next slot, sleep until it. Never adaptive.
fn wait_for_claude_slot(gate: &Mutex<Option<Instant>>, stagger: Duration) {
    let slot = {
        let mut next = gate.lock().unwrap_or_else(PoisonError::into_inner);
        let now = Instant::now();
        let slot = match *next {
            Some(n) if n > now => n,
            _ => now,
        };
        *next = Some(slot + stagger);
        slot
    };
    let wait = slot.saturating_duration_since(Instant::now());
    if !wait.is_zero() {
        thread::sleep(wait);
    }
}

pub(crate) fn on_spawned(ctx: &mut Ctx<'_>, id: &str, instance: u64, result: Result<Spawned, String>) {
    let current = ctx.state.rt.sessions.get(id).is_some_and(|rt| rt.io.instance == instance);
    if !current {
        if let Ok(p) = result {
            p.pty.kill();
        }
        return;
    }
    let callbacks = match result {
        Ok(Spawned { pty, claude_session_id }) => {
            // The Claude conversation it runs: the usage tools follow its transcript exactly.
            if let Some(cid) = claude_session_id
                && let Some(i) = session_index(ctx.state, id)
                && let Some(s) = ctx.state.app.sessions.get_mut(i)
                && s.claude_session_id.as_deref() != Some(cid.as_str())
            {
                s.claude_session_id = Some(cid);
                ctx.state.persist();
            }
            // Usage stats (WPF RecordSessionStart): counted per command, on the index thread.
            let command = ctx.state.app.sessions.iter().find(|s| s.id == id).map(|s| s.command.clone()).unwrap_or_default();
            if let Some(ix) = &ctx.state.rt.index {
                ix.record_usage_start(&command);
            }
            let Some(rt) = ctx.state.rt.sessions.get_mut(id) else { return };
            rt.usage = Some((command, Instant::now()));
            rt.started_ms = now_ms();
            rt.backend = Some(pty.backend());
            if pty.has_exited() {
                rt.phase = Phase::Exited { code: pty.exit_code() };
            } else if rt.phase == Phase::Starting {
                rt.phase = Phase::Live;
            }
            let cbs = std::mem::take(&mut rt.on_started);
            if let Some(i) = session_index(ctx.state, id)
                && let Some(s) = ctx.state.app.sessions.get_mut(i)
            {
                s.status = SessionStatus::Running;
            }
            cbs.into_iter().map(|cb| (cb, Ok(()))).collect::<Vec<_>>()
        }
        Err(msg) => {
            let name = display_name(ctx.state, id);
            log::warn!("session {id} failed to start: {msg}");
            let cbs = match ctx.state.rt.sessions.remove(id) {
                Some(mut rt) => std::mem::take(&mut rt.on_started),
                None => Vec::new(),
            };
            // Falls back to asleep, never deleted: the row stays visible and editable.
            if let Some(i) = session_index(ctx.state, id)
                && let Some(s) = ctx.state.app.sessions.get_mut(i)
            {
                s.is_dormant = true;
                s.status = SessionStatus::Idle;
            }
            ctx.events.toast(format!("Could not start {name}: {msg}"));
            ctx.state.persist();
            cbs.into_iter().map(|cb| (cb, Err(msg.clone()))).collect()
        }
    };
    finish_restore_item(ctx.state, id);
    ctx.state.touch();
    for (cb, r) in callbacks {
        cb(ctx, r);
    }
}

pub(crate) fn on_exited(ctx: &mut Ctx<'_>, id: &str, instance: u64, code: Option<i32>) {
    let index = ctx.state.rt.index.clone();
    let Some(rt) = ctx.state.rt.sessions.get_mut(id) else { return };
    if rt.io.instance != instance {
        return;
    }
    rt.end_usage(index.as_ref());
    rt.phase = Phase::Exited { code };
    // Remote Control lives in the process.
    rt.remote_control = None;
    rt.io.detector().dispose();
    // Working, blocked and idle records end with the process; done/error stay until seen.
    if rt.records.drop_transient() && rt.records.count() == 0 && rt.has_program_status {
        rt.rearm();
    }
    if let Some(i) = session_index(ctx.state, id)
        && let Some(s) = ctx.state.app.sessions.get_mut(i)
    {
        s.status = SessionStatus::Exited;
    }
    ctx.state.touch();
}

/// What the OSC tap saw, applied in stream order.
pub(crate) fn apply_term_events(ctx: &mut Ctx<'_>, id: &str, instance: u64, events: Vec<TermEvent>) {
    let Some(i) = session_index(ctx.state, id) else { return };
    let state = &mut *ctx.state;
    let Some(rt) = state.rt.sessions.get_mut(id) else { return };
    if rt.io.instance != instance {
        return;
    }
    let mut persist = false;
    for ev in events {
        match ev {
            TermEvent::Tap(TapEvent::ShellIntegration(pairs)) => {
                for (k, v) in pairs {
                    match k.as_str() {
                        "color" => {
                            if let Some(c) = si::try_normalize_color(&v)
                                && let Some(s) = state.app.sessions.get_mut(i)
                                && s.color_override.as_deref() != Some(c.as_str())
                            {
                                s.color_override = Some(c);
                                persist = true;
                            }
                        }
                        "git-branch" => {
                            rt.git_branch = si::sanitize_branch(&v);
                            rt.git_osc = true;
                        }
                        "git-dirty" => {
                            rt.git_dirty = si::parse_dirty(&v);
                            rt.git_osc = true;
                        }
                        "title" => {
                            if let Some(t) = si::sanitize_title(&v)
                                && let Some(s) = state.app.sessions.get_mut(i)
                                && s.name != t
                            {
                                s.name = t;
                                persist = true;
                            }
                        }
                        _ => {}
                    }
                }
            }
            TermEvent::Tap(TapEvent::ProgramStatus(payload)) => {
                let Some(report) = psp::try_parse(&payload) else { continue };
                let exited = matches!(rt.phase, Phase::Exited { .. });
                // Live states queued behind the exit must not stick forever.
                if exited && matches!(report.state, ProgramState::Working | ProgramState::Blocked | ProgramState::Idle) {
                    continue;
                }
                let is_clear = report.state == ProgramState::Clear;
                let (changed, _entered) = rt.records.apply_noting_attention(report);
                if !rt.has_program_status && !is_clear {
                    rt.stand_down();
                }
                if is_clear && changed && rt.records.count() == 0 && rt.has_program_status {
                    rt.rearm();
                }
            }
            TermEvent::Tap(TapEvent::Prompt(PromptMark::PromptStart)) => {
                if rt.records.drop_transient() && rt.records.count() == 0 && rt.has_program_status {
                    rt.rearm();
                }
            }
            TermEvent::Tap(TapEvent::Cwd(uri)) => {
                rt.cwd = Some(uri.chars().filter(|c| !c.is_control()).take(1024).collect());
            }
            TermEvent::Title(t) => rt.title = si::sanitize_title(&t),
            // OSC 52 copy (#156). The UI sets the clipboard on its own thread; the text never
            // rides an event. Queries (reads) are never answered (`csm_term`).
            TermEvent::ClipboardStore(text) => {
                if !state.app.settings.allow_osc52_clipboard {
                    log::debug!("session {id}: OSC 52 copy ignored (turned off in settings)");
                } else if text.len() <= csm_term::MAX_CLIPBOARD_STORE_BYTES {
                    ctx.engine.offer_clipboard(text);
                    ctx.events.emit(Event::ClipboardOffered { session_id: id.to_owned() });
                }
            }
            _ => {}
        }
    }
    if persist {
        state.persist();
    } else {
        state.touch();
    }
}

/// A real keystroke (or paste) reached the session: acknowledge unseen results.
pub(crate) fn user_interacted(ctx: &mut Ctx<'_>, id: &str, instance: u64) {
    if let Some(rt) = ctx.state.rt.sessions.get_mut(id)
        && rt.io.instance == instance
    {
        rt.io.clear_interaction();
        if rt.acknowledge() {
            ctx.state.touch();
        }
    }
}

// ── Teardown ─────────────────────────────────────────────────────────────────────────────

/// Removes a session's runtime and returns its PTY (the caller retires it). Anything waiting
/// for it to start is told it won't.
pub(crate) fn teardown(ctx: &mut Ctx<'_>, id: &str) -> Option<Arc<dyn Pty>> {
    // A session's run commands go with it (close, sleep, restart, quit), whole trees.
    crate::runs::stop_all(ctx.state, id);
    // Its SSH port forwards go too (close, sleep, restart, quit).
    crate::port_forward::teardown_session(ctx, id);
    let mut rt = ctx.state.rt.sessions.remove(id)?;
    rt.end_usage(ctx.state.rt.index.as_ref());
    rt.io.closed.store(true, Ordering::Release);
    finish_restore_item(ctx.state, id);
    ctx.state.touch();
    let pty = rt.io.pty.get().cloned();
    for cb in std::mem::take(&mut rt.on_started) {
        cb(ctx, Err("the session was stopped before it started".into()));
    }
    pty
}

/// Ends a PTY off the engine thread. Claude gets a graceful close and up to `wait` to exit (it
/// writes its config on the way out); anything else is killed at once. Either way it is killed
/// at the end, so the whole tree goes.
pub(crate) fn retire(pty: Arc<dyn Pty>, graceful: bool, wait: Duration) {
    if !graceful {
        pty.kill();
        return;
    }
    let spawned = thread::Builder::new().name("csm-retire".into()).spawn({
        let pty = pty.clone();
        move || {
            pty.close();
            wait_exit(pty.as_ref(), wait);
            pty.kill();
        }
    });
    if spawned.is_err() {
        pty.kill();
    }
}

/// Waits (on a worker) until the process has exited or `wait` passed. True when it exited.
/// [`wait_exit`] that also gives up as soon as `stop()` says so (a forced quit).
fn wait_exit_unless(pty: &dyn Pty, wait: Duration, stop: &dyn Fn() -> bool) -> bool {
    let deadline = Instant::now() + wait;
    while !pty.has_exited() {
        if Instant::now() >= deadline || stop() {
            return pty.has_exited();
        }
        thread::sleep(Duration::from_millis(20));
    }
    true
}

/// Sleeps up to `d`, waking early when `stop()` says so.
fn sleep_unless(d: Duration, stop: &dyn Fn() -> bool) {
    let deadline = Instant::now() + d;
    while Instant::now() < deadline && !stop() {
        thread::sleep(Duration::from_millis(20).min(deadline.saturating_duration_since(Instant::now())));
    }
}

pub(crate) fn wait_exit(pty: &dyn Pty, wait: Duration) -> bool {
    let deadline = Instant::now() + wait;
    while !pty.has_exited() {
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(20));
    }
    true
}

/// Why a live session is busy (`csm_core::busy_rules`), or `None`.
pub(crate) fn busy_reason(state: &State, id: &str) -> Option<String> {
    let rt = state.rt.sessions.get(id)?;
    if matches!(rt.phase, Phase::Exited { .. }) {
        return None;
    }
    let sum = rt.records.summarize();
    csm_core::busy_rules::reason(rt.has_program_status, sum.state, sum.progress, 0, rt.io.last_output(), now_ms())
}

// ── Close, sleep, wake, focus ────────────────────────────────────────────────────────────

pub(crate) fn close_session(ctx: &mut Ctx<'_>, id: &str, force: bool) -> Result<ToolOutput, ToolError> {
    let Some(i) = session_index(ctx.state, id) else {
        return Err(ToolError::NotFound(format!("no session `{id}`")));
    };
    let name = display_name(ctx.state, id);
    if !force && let Some(why) = busy_reason(ctx.state, id) {
        return Err(ToolError::Refused(format!("{name} is busy ({why}). Closing it would cut that off; pass force: true to close it anyway.")));
    }
    let claude = is_claude(ctx.state, id);
    if let Some(s) = ctx.state.app.sessions.get(i) {
        let entry = RecentlyClosedEntry::from_session(s, Timestamp::from_epoch_ms(now_ms()), new_id);
        ctx.state.app.recently_closed.insert(0, entry);
        if let Some(ix) = &ctx.state.rt.index {
            record_in_index(ix, &name, s);
        }
        ctx.state.app.recently_closed.truncate(MAX_RECENTLY_CLOSED);
    }
    let pty = teardown(ctx, id);
    if let Some(i) = session_index(ctx.state, id) {
        ctx.state.app.sessions.remove(i);
    }
    forget_session(ctx.state, id);
    if let Some(p) = pty {
        retire(p, claude, ctx.state.options.exit_wait);
    }
    ctx.state.persist();
    Ok(ToolOutput::new(format!("Closed {name}."), json!({"session_id": id})))
}

/// Drops every reference to a removed session (pins, focus, restart bookkeeping).
pub(crate) fn forget_session(state: &mut State, id: &str) {
    for l in state.app.ui.layouts.values_mut() {
        l.forget(id);
    }
    if state.rt.focused.as_deref() == Some(id) {
        state.rt.focused = None;
    }
    state.rt.restarting.remove(id);
    state.rt.inbox_since.remove(id);
    state.rt.last_toast.remove(id);
    state.rt.status_since.remove(id);
    // Its assigned tasks go back to the backlog (review and done keep who did them).
    if !state.rt.tasks.release_session(id, now_ms()).is_empty() {
        state.persist_tasks();
    }
}

/// Tells the add-on that opened a session (if it subscribed) that the session went to sleep or woke.
fn tell_owner(ctx: &Ctx<'_>, id: &str, kind: &str) {
    if let Some(owner) = ctx.state.app.sessions.iter().find(|s| s.id == id).map(|s| s.owner_addon.as_str()).filter(|o| !o.is_empty()) {
        ctx.engine.host.notify_session(owner, kind, id);
    }
}

pub(crate) fn sleep_session(ctx: &mut Ctx<'_>, id: &str) -> Result<ToolOutput, ToolError> {
    let Some(i) = session_index(ctx.state, id) else {
        return Err(ToolError::NotFound(format!("no session `{id}`")));
    };
    let name = display_name(ctx.state, id);
    if ctx.state.app.sessions.get(i).is_some_and(|s| s.is_dormant) {
        return Ok(ToolOutput::new(format!("{name} is already asleep."), json!({"session_id": id})));
    }
    if ctx.state.rt.restarting.contains(id) {
        return Err(ToolError::Refused(format!("{name} is restarting; try again when it has started.")));
    }
    let claude = is_claude(ctx.state, id);
    let pty = teardown(ctx, id);
    if let Some(s) = ctx.state.app.sessions.get_mut(i) {
        s.is_dormant = true;
        s.status = SessionStatus::Idle;
    }
    if let Some(p) = pty {
        retire(p, claude, ctx.state.options.exit_wait);
    }
    tell_owner(ctx, id, "session_asleep");
    ctx.state.persist();
    Ok(ToolOutput::new(format!("{name} is asleep. Its settings and place are kept."), json!({"session_id": id})))
}

/// Wakes a sleeping session (same path as restore). On a launch failure it stays asleep.
pub(crate) fn wake_session(ctx: &mut Ctx<'_>, id: &str, then: Option<StartedCb>) -> Result<ToolOutput, ToolError> {
    let Some(i) = session_index(ctx.state, id) else {
        return Err(ToolError::NotFound(format!("no session `{id}`")));
    };
    let name = display_name(ctx.state, id);
    let dormant = ctx.state.app.sessions.get(i).is_some_and(|s| s.is_dormant);
    if !dormant {
        if let Some(cb) = then {
            cb(ctx, Ok(()));
        }
        return Ok(ToolOutput::new(format!("{name} is already awake."), json!({"session_id": id})));
    }
    if let Some(s) = ctx.state.app.sessions.get_mut(i) {
        s.is_dormant = false;
    }
    match launch(ctx, id, true) {
        Ok(()) => {
            if let Some(cb) = then
                && let Some(rt) = ctx.state.rt.sessions.get_mut(id)
            {
                rt.on_started.push(cb);
            }
            tell_owner(ctx, id, "session_awake");
            ctx.state.persist();
            Ok(ToolOutput::new(format!("Waking {name}."), json!({"session_id": id})))
        }
        Err(msg) => {
            if let Some(s) = ctx.state.app.sessions.get_mut(i) {
                s.is_dormant = true;
            }
            Err(ToolError::Failed(format!("{name} could not be woken: {msg}")))
        }
    }
}

/// Focuses a session: acknowledges its results, switches the rail tab to its group when the
/// current tab hides it, stamps "last used" (not during the restore loop), and wakes it when
/// asleep.
pub(crate) fn focus(ctx: &mut Ctx<'_>, id: &str) -> Result<ToolOutput, ToolError> {
    let Some(i) = session_index(ctx.state, id) else {
        return Err(ToolError::NotFound(format!("no session `{id}`")));
    };
    let name = display_name(ctx.state, id);
    let mut woke = false;
    if ctx.state.app.sessions.get(i).is_some_and(|s| s.is_dormant) {
        wake_session(ctx, id, None)?;
        woke = true;
    }
    ctx.state.rt.focused = Some(id.to_owned());
    let restoring = ctx.state.rt.restore.is_some();
    let group = ctx.state.app.sessions.get(i).map(|s| s.group_id.clone()).unwrap_or_default();
    // Issue #137: the group's rail tab brings this session back.
    if !group.is_empty() {
        ctx.state.app.ui.group_focus.insert(group.clone(), id.to_owned());
    }
    if let Some(s) = ctx.state.app.sessions.get_mut(i)
        && !restoring
    {
        s.last_activity_at = Timestamp::from_epoch_ms(now_ms());
    }
    // A minimized pane comes back, and a different pane filling its layout's canvas gives way.
    for (key, l) in ctx.state.app.ui.layouts.iter_mut() {
        l.reveal(id, key == csm_core::models::ALL_LAYOUT_KEY || *key == group);
    }
    if snapshot::group_view_of(ctx.state) == snapshot::GroupView::Rail
        && let Some(tab) = &ctx.state.app.ui.rail_tab
        && *tab != group
    {
        ctx.state.app.ui.rail_tab = if group.is_empty() { None } else { Some(group) };
    }
    if let Some(rt) = ctx.state.rt.sessions.get_mut(id) {
        rt.acknowledge();
    }
    ctx.events.emit(Event::FocusRequested { session_id: id.to_owned() });
    ctx.state.persist();
    let message = if woke { format!("Waking and focusing {name}.") } else { format!("Focused {name}.") };
    Ok(ToolOutput::new(message, json!({"session_id": id})))
}

// ── Restart ──────────────────────────────────────────────────────────────────────────────

/// Why a restart may not start now (the guards live on the entry points, not the core).
pub(crate) fn restart_refusal(state: &State) -> Option<ToolError> {
    if state.rt.shutting_down {
        return Some(ToolError::Refused("the app is closing".into()));
    }
    if state.rt.restore.is_some() {
        return Some(ToolError::Refused(
            "sessions are still being restored; restart when the restore has finished (two loops starting Claude at once would race on its config)"
                .into(),
        ));
    }
    if state.rt.restart_active {
        return Some(ToolError::Refused("a restart is already running; wait for it to finish".into()));
    }
    None
}

/// Restarts `ids` one after another (never in parallel: each Claude exit is awaited before the
/// next launch). The caller has checked [`restart_refusal`]. Replies when all are done.
pub(crate) fn restart_many(ctx: &mut Ctx<'_>, ids: Vec<String>, reply: Option<ParkedReply>) {
    ctx.state.rt.restart_active = true;
    ctx.state.rt.restart_cancel = false;
    let total = ids.len();
    ctx.state.rt.restart = (total > 1).then_some(Progress { done: 0, total });
    ctx.state.touch();
    restart_next(ctx, ids.into(), reply, Vec::new());
}

fn restart_next(ctx: &mut Ctx<'_>, mut queue: VecDeque<String>, reply: Option<ParkedReply>, mut results: Vec<(String, Result<(), String>)>) {
    loop {
        let stop = ctx.state.rt.shutting_down || ctx.state.rt.restart_cancel;
        let Some(id) = queue.pop_front().filter(|_| !stop) else {
            let skipped = queue.len();
            finish_restart(ctx, reply, results, skipped);
            return;
        };
        // Re-resolved per iteration: an earlier restart may have failed into asleep, or the
        // person closed it meanwhile.
        let live = ctx.state.app.sessions.iter().any(|s| s.id == id && !s.is_dormant);
        if !live {
            results.push((id, Err("not running".into())));
            continue;
        }
        let id2 = id.clone();
        let cont: StartedCb = Box::new(move |ctx, r| {
            if let Some(p) = &mut ctx.state.rt.restart {
                p.done = (p.done + 1).min(p.total);
            }
            results.push((id2, r));
            restart_next(ctx, queue, reply, results);
        });
        restart_core(ctx, &id, cont);
        return;
    }
}

fn finish_restart(ctx: &mut Ctx<'_>, reply: Option<ParkedReply>, results: Vec<(String, Result<(), String>)>, skipped: usize) {
    ctx.state.rt.restart_active = false;
    ctx.state.rt.restart_cancel = false;
    ctx.state.rt.restart = None;
    ctx.state.touch();
    let ok = results.iter().filter(|(_, r)| r.is_ok()).count();
    let failed: Vec<_> = results.iter().filter_map(|(id, r)| r.as_ref().err().map(|e| json!({"session_id": id, "error": e}))).collect();
    let mut message = match (ok, results.len()) {
        (1, 1) => format!("Restarted {}.", results.first().map(|(id, _)| display_name(ctx.state, id)).unwrap_or_default()),
        (o, n) => format!("Restarted {o} of {n} sessions."),
    };
    if skipped > 0 {
        message.push_str(&format!(" Stopped; {skipped} not restarted."));
    }
    if !failed.is_empty() && results.len() == 1 {
        let err = failed.first().and_then(|f| f["error"].as_str()).unwrap_or("unknown error").to_owned();
        after_step(ctx.state, ctx.engine);
        if let Some(r) = reply {
            r.send(Err(ToolError::Failed(format!("The restart failed: {err}. The session is asleep; edit it and wake it."))));
        }
        return;
    }
    if results.len() > 1 {
        ctx.events.toast(message.clone());
    }
    after_step(ctx.state, ctx.engine);
    if let Some(r) = reply {
        r.send(Ok(ToolOutput::new(message, json!({"restarted": ok, "failed": failed, "skipped": skipped}))));
    }
}

/// One restart: tear down, wait for a Claude process to exit (then the flat stagger), relaunch
/// with resume. `then` runs once the new process started or the restart failed (the session is
/// then asleep). Holds no guard of its own.
pub(crate) fn restart_core(ctx: &mut Ctx<'_>, id: &str, then: StartedCb) {
    let claude = is_claude(ctx.state, id);
    let was_focused = ctx.state.rt.focused.as_deref() == Some(id);
    let pty = teardown(ctx, id);
    ctx.state.rt.restarting.insert(id.to_owned());
    if let Some(i) = session_index(ctx.state, id)
        && let Some(s) = ctx.state.app.sessions.get_mut(i)
    {
        s.status = SessionStatus::Idle;
    }
    ctx.state.touch();
    let wait = ctx.state.options.exit_wait;
    let quiesce = Duration::from_millis(u64::try_from(ctx.state.app.settings.claude_launch_stagger_ms).unwrap_or(0));
    let (engine, id) = (ctx.engine.clone(), id.to_owned());
    let worker = move || {
        if let Some(p) = pty {
            if claude {
                p.close();
                let clean = wait_exit(p.as_ref(), wait);
                log::info!("restart {id}: old claude exited cleanly={clean}");
                p.kill();
                thread::sleep(quiesce);
            } else {
                p.kill();
            }
        }
        engine.post(Box::new(move |ctx| relaunch_after_restart(ctx, &id, was_focused, then)));
    };
    if let Err(e) = thread::Builder::new().name("csm-restart".into()).spawn(worker) {
        log::error!("restart: no worker thread: {e}");
    }
}

fn relaunch_after_restart(ctx: &mut Ctx<'_>, id: &str, was_focused: bool, then: StartedCb) {
    ctx.state.rt.restarting.remove(id);
    ctx.state.touch();
    // The second shutdown check: the session is invisible to the quit's snapshot of live
    // sessions, so relaunching now would leave a process nothing disposes.
    if ctx.state.rt.shutting_down {
        then(ctx, Err("the app is closing".into()));
        return;
    }
    let Some(i) = session_index(ctx.state, id) else {
        then(ctx, Err("the session was closed during the restart".into()));
        return;
    };
    if ctx.state.app.sessions.get(i).is_some_and(|s| s.is_dormant) {
        then(ctx, Err("the session was put to sleep during the restart".into()));
        return;
    }
    match launch(ctx, id, true) {
        Ok(()) => {
            let id2 = id.to_owned();
            let cb: StartedCb = Box::new(move |ctx, r| {
                if r.is_ok() && was_focused && ctx.state.rt.focused.as_deref() == Some(id2.as_str()) {
                    ctx.events.emit(Event::FocusRequested { session_id: id2.clone() });
                }
                then(ctx, r);
            });
            if let Some(rt) = ctx.state.rt.sessions.get_mut(id) {
                rt.on_started.push(cb);
            }
        }
        Err(msg) => {
            if let Some(s) = ctx.state.app.sessions.get_mut(i) {
                s.is_dormant = true;
            }
            ctx.state.persist();
            then(ctx, Err(msg));
        }
    }
}

// ── Quit ─────────────────────────────────────────────────────────────────────────────────

/// The quit sequence: Claude sessions are closed one after another, each given up to
/// `shutdown_budget` (15 s; busy sessions were measured exiting in 2.3-4.7 s, see
/// docs/wpf/lessons.md) to exit by itself, with the flat settle between them, so one stuck session
/// never eats the others' time; everything else is killed at once. The whole budget (per session
/// times the Claude sessions) is in the snapshot for the board. Sessions stay in the state (they restore next time).
/// Ends with the state saved and [`Event::QuitReady`].
pub(crate) fn quit(ctx: &mut Ctx<'_>, force: bool, reply: Option<ParkedReply>) {
    if ctx.state.rt.shutting_down {
        if force {
            // A second, forced quit: the running one stops waiting for exits.
            ctx.state.rt.quit_force.store(true, std::sync::atomic::Ordering::Release);
            if let Some(r) = reply {
                r.send(Ok(ToolOutput::message("Force closing: the remaining sessions are stopped now.")));
            }
            return;
        }
        if let Some(r) = reply {
            r.send(Err(ToolError::Refused("the app is already closing (pass force: true to stop waiting)".into())));
        }
        return;
    }
    if force {
        ctx.state.rt.quit_force.store(true, std::sync::atomic::Ordering::Release);
    }
    ctx.state.rt.shutting_down = true;
    ctx.state.rt.restart_cancel = true;
    let ids: Vec<String> = ctx.state.app.sessions.iter().map(|s| s.id.clone()).collect();
    let mut targets: Vec<(String, Arc<dyn Pty>, bool)> = Vec::new();
    for id in ids {
        let claude = is_claude(ctx.state, &id);
        if let Some(p) = teardown(ctx, &id) {
            targets.push((id, p, claude));
        }
    }
    // Claude sessions first in sidebar order is fine; non-Claude ones are instant.
    targets.sort_by_key(|(_, _, claude)| *claude);
    let total = targets.len();
    // Claude sessions close one after another, so each gets its own wait (`shutdown_budget`)
    // and the whole quit may take that times their number, plus the stagger between them.
    let per_session = ctx.state.options.shutdown_budget;
    let settle = Duration::from_millis(u64::try_from(ctx.state.app.settings.claude_launch_stagger_ms).unwrap_or(0));
    let claudes = u32::try_from(targets.iter().filter(|(_, _, c)| *c).count()).unwrap_or(u32::MAX);
    let budget = per_session.saturating_add(settle).saturating_mul(claudes.max(1));
    ctx.state.rt.quit = Some(Progress { done: 0, total });
    ctx.state.rt.quit_budget_ms = Some(u64::try_from(budget.as_millis()).unwrap_or(u64::MAX));
    ctx.state.touch();
    let events = ctx.events.clone();
    let engine = ctx.engine.clone();
    let forced = ctx.state.rt.quit_force.clone();
    let worker = move || {
        let forced_now = || forced.load(std::sync::atomic::Ordering::Acquire);
        for (id, pty, claude) in targets {
            let t0 = Instant::now();
            let ms = |t: Instant| u64::try_from(t.elapsed().as_millis()).unwrap_or(u64::MAX);
            let exited = pty.has_exited();
            if !claude || exited || forced_now() {
                // Decide before the kill: after it, every PTY has exited. A non-Claude session is
                // simply ended (that's its clean close); a live Claude that "Force close now" (or
                // a forced quit) reached before its turn was forced, not clean.
                pty.kill();
                let phase = if !claude || exited { "clean" } else { "forced" };
                events.emit(Event::QuitProgress { session_id: id, phase: phase.into(), elapsed_ms: ms(t0) });
            } else {
                events.emit(Event::QuitProgress { session_id: id.clone(), phase: "closing".into(), elapsed_ms: 0 });
                pty.close();
                let clean = wait_exit_unless(pty.as_ref(), per_session, &forced_now);
                pty.kill();
                let phase = if clean { "clean" } else { "forced" };
                log::info!("quit: {id} {phase} exit={}ms", ms(t0));
                events.emit(Event::QuitProgress { session_id: id, phase: phase.into(), elapsed_ms: ms(t0) });
                if clean && !forced_now() {
                    sleep_unless(settle, &forced_now);
                }
            }
            engine.post(Box::new(|ctx| {
                if let Some(p) = &mut ctx.state.rt.quit {
                    p.done = (p.done + 1).min(p.total);
                }
                ctx.state.touch();
            }));
        }
        engine.post(Box::new(move |ctx| {
            after_step(ctx.state, ctx.engine);
            save_now(ctx.state);
            ctx.events.emit(Event::QuitReady);
            if let Some(r) = reply {
                let msg = if ctx.state.options.saves() {
                    "Every session is closed and the state is saved."
                } else {
                    "Every session is closed (nothing is saved in this mode)."
                };
                r.send(Ok(ToolOutput::new(msg, json!({"sessions": total}))));
            }
        }));
    };
    if let Err(e) = thread::Builder::new().name("csm-quit".into()).spawn(worker) {
        log::error!("quit: no worker thread: {e}");
        ctx.events.emit(Event::QuitReady);
    }
}

/// Where per-session MCP configs go: beside the state file, or a temp folder when nothing is saved.
fn mcp_offer_dir(state: &State) -> Option<std::path::PathBuf> {
    match state.options.save_path.as_deref().and_then(std::path::Path::parent) {
        Some(d) if state.options.saves() => Some(d.join("mcp")),
        _ => Some(std::env::temp_dir().join(format!("{}-mcp", crate::persist::APP_DIR))),
    }
}
