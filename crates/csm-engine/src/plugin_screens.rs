//! Add-on screens, engine side (docs/addons.md, "Screens"): which screens running add-ons
//! declared, which instances are open, and the validated view tree each one shows.
//!
//! Threads. A plugin's `screen/render` and `screen/patch` arrive on its supervisor thread, which
//! parses and validates them (`csm_core::addons::screen`) and swaps an `Arc<ScreenTree>` into the
//! [`ScreenStore`] under a lock held only for the swap. The UI reads the store once a frame
//! (an `Arc` clone) and never waits on a plugin. The engine thread never blocks here either: the
//! `screen/open` request waits for its answer on a thread of its own.
//!
//! Renders are published at most 30 a second per instance. A render or patch that comes sooner is
//! applied to the instance's latest tree at once and published when the interval has passed, so
//! the latest always wins and nothing queues.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use csm_core::addons::screen::{RateGate, ScreenContext, ScreenDecl, ScreenEvent, ScreenScope, ScreenTree, UpdateError, MAX_INSTANCES};
use serde::Serialize;
use serde_json::{Value, json};

use crate::events::{Event, Events};
use crate::plugins::{Link, ReqError};
use crate::tool::ToolError;

/// How long a plugin has to answer `screen/open`.
pub const OPEN_TIMEOUT: Duration = Duration::from_secs(10);

/// Where one open screen stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ScreenPhase {
    /// `screen/open` was sent; the tab shows a spinner.
    Opening,
    /// A tree is on screen.
    Ready,
    /// The plugin did not answer `screen/open` in time, or answered with an error. Retry.
    NotAnswering,
    /// The add-on's process is not running. The last tree (if any) stays, greyed out.
    Stopped,
}

/// One screen an add-on declared, with the add-on's name.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DeclaredScreen {
    pub addon: String,
    pub addon_name: String,
    pub id: String,
    pub title: String,
    pub icon: String,
    pub scope: ScreenScope,
    pub summary: String,
}

/// One open instance, as the UI and `screen_list` see it (no tree: see [`ScreenStore::tree`]).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScreenInfo {
    pub instance: String,
    pub addon: String,
    pub addon_name: String,
    pub screen: String,
    pub title: String,
    pub icon: String,
    pub scope: ScreenScope,
    pub phase: ScreenPhase,
    /// In its own window rather than the view strip.
    pub popped_out: bool,
    /// Why the last tree was refused, or why the plugin did not answer.
    pub error: Option<String>,
    /// The revision on screen (0: none yet).
    pub rev: u64,
    pub context: ScreenContext,
}

struct Declared {
    generation: u64,
    addon_name: String,
    screens: Vec<ScreenDecl>,
}

struct Instance {
    info: ScreenInfo,
    generation: u64,
    /// The latest accepted tree (what the next patch applies to).
    current: Option<Arc<ScreenTree>>,
    /// What the UI draws; trails `current` by at most one render interval.
    published: Option<Arc<ScreenTree>>,
    dirty: bool,
    gate: RateGate,
    /// `change` and `visible` events that did not fit the plugin's queue; the latest of each
    /// (key, kind) is sent when there is room.
    held: Vec<ScreenEvent>,
}

#[derive(Default)]
struct Inner {
    declared: BTreeMap<String, Declared>,
    instances: BTreeMap<String, Instance>,
    next_id: u64,
}

/// What [`ScreenStore::open`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Opened {
    /// A new instance: the caller sends `screen/open`.
    Created(String),
    /// The same screen on the same context is already open (its phase says whether to retry).
    Existing(String, ScreenPhase),
}

/// How a render or patch from a plugin was handled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Applied {
    Yes,
    /// Not for a live instance of this process, or not newer than what is shown: ignored.
    Ignored,
    /// A patch whose base is not what is shown: ask for a full render.
    Refresh,
    /// Broke a cap or was malformed: refused, the previous tree stays.
    Refused(String),
}

/// The screens of every running add-on. Cheap to clone; all clones are the same store.
#[derive(Clone)]
pub struct ScreenStore {
    inner: Arc<Mutex<Inner>>,
    events: Events,
    epoch: Instant,
}

impl ScreenStore {
    pub(crate) fn new(events: Events) -> Self {
        ScreenStore { inner: Arc::new(Mutex::new(Inner::default())), events, epoch: Instant::now() }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn now_ms(&self) -> u64 {
        u64::try_from(self.epoch.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn changed(&self, instances: impl IntoIterator<Item = String>) {
        for instance in instances {
            self.events.emit(Event::ScreenChanged { instance });
        }
    }

    /// The screens the running add-ons declared, in add-on and declaration order.
    pub fn declared(&self) -> Vec<DeclaredScreen> {
        let g = self.lock();
        g.declared
            .iter()
            .flat_map(|(addon, d)| {
                d.screens.iter().map(move |s| DeclaredScreen {
                    addon: addon.clone(),
                    addon_name: d.addon_name.clone(),
                    id: s.id.clone(),
                    title: s.title.clone(),
                    icon: s.icon.clone(),
                    scope: s.scope,
                    summary: s.summary.clone(),
                })
            })
            .collect()
    }

    /// The open instances, in the order they were opened.
    pub fn instances(&self) -> Vec<ScreenInfo> {
        let g = self.lock();
        let mut v: Vec<(u64, ScreenInfo)> = g.instances.iter().map(|(id, i)| (id.trim_start_matches('i').parse().unwrap_or(0), i.info.clone())).collect();
        v.sort_by_key(|(n, _)| *n);
        v.into_iter().map(|(_, i)| i).collect()
    }

    pub(crate) fn instance_generation(&self, instance: &str) -> Option<u64> {
        self.lock().instances.get(instance).map(|i| i.generation)
    }

    pub fn instance(&self, instance: &str) -> Option<ScreenInfo> {
        self.lock().instances.get(instance).map(|i| i.info.clone())
    }

    /// The tree to draw: an `Arc` clone, so the lock is held for no longer than that.
    pub fn tree(&self, instance: &str) -> Option<Arc<ScreenTree>> {
        self.lock().instances.get(instance).and_then(|i| i.published.clone())
    }

    /// A process of `addon` answered `initialize` with these screens. Instances left by an earlier
    /// process whose screen is still declared are returned, to be opened again (they keep their
    /// tab and context); the others are closed.
    pub(crate) fn set_declared(&self, addon: &str, generation: u64, addon_name: &str, screens: Vec<ScreenDecl>) -> Vec<(String, String, ScreenContext)> {
        let (reopen, gone) = {
            let mut g = self.lock();
            if g.declared.get(addon).is_some_and(|d| d.generation > generation) {
                return Vec::new();
            }
            let ids: Vec<String> = screens.iter().map(|s| s.id.clone()).collect();
            if screens.is_empty() {
                g.declared.remove(addon);
            } else {
                g.declared.insert(addon.to_owned(), Declared { generation, addon_name: addon_name.to_owned(), screens: screens.clone() });
            }
            let mut reopen = Vec::new();
            let mut gone = Vec::new();
            for (id, inst) in g.instances.iter_mut().filter(|(_, i)| i.info.addon == addon) {
                if inst.generation > generation {
                    continue;
                }
                if ids.contains(&inst.info.screen) {
                    inst.generation = generation;
                    inst.info.phase = ScreenPhase::Opening;
                    inst.info.error = None;
                    inst.info.addon_name = addon_name.to_owned();
                    reopen.push((id.clone(), inst.info.screen.clone(), inst.info.context.clone()));
                } else {
                    gone.push(id.clone());
                }
            }
            for id in &gone {
                g.instances.remove(id);
            }
            (reopen, gone)
        };
        self.changed(reopen.iter().map(|(i, _, _)| i.clone()).chain(gone));
        reopen
    }

    /// The process of `addon` (not newer than `generation`) stopped: its screens are no longer
    /// offered, and its open instances show the "not running" placeholder.
    pub(crate) fn mark_stopped(&self, addon: &str, generation: u64) {
        let touched = {
            let mut g = self.lock();
            if g.declared.get(addon).is_some_and(|d| d.generation > generation) {
                return;
            }
            g.declared.remove(addon);
            let mut touched = Vec::new();
            for (id, inst) in g.instances.iter_mut().filter(|(_, i)| i.info.addon == addon && i.generation <= generation) {
                if inst.info.phase != ScreenPhase::Stopped {
                    inst.info.phase = ScreenPhase::Stopped;
                    // Whatever was published stays on screen (greyed out): publish the latest now.
                    inst.published = inst.current.clone();
                    inst.dirty = false;
                    inst.held.clear();
                    touched.push(id.clone());
                }
            }
            touched
        };
        self.changed(touched);
    }

    /// Every add-on that has screens declared or open.
    pub(crate) fn owners(&self) -> Vec<String> {
        let g = self.lock();
        let mut v: Vec<String> = g.declared.keys().cloned().chain(g.instances.values().map(|i| i.info.addon.clone())).collect();
        v.sort();
        v.dedup();
        v
    }

    /// The add-on was disabled or removed: its instances and screens go.
    pub(crate) fn remove_addon(&self, addon: &str) {
        let gone: Vec<String> = {
            let mut g = self.lock();
            g.declared.remove(addon);
            let ids: Vec<String> = g.instances.iter().filter(|(_, i)| i.info.addon == addon).map(|(id, _)| id.clone()).collect();
            for id in &ids {
                g.instances.remove(id);
            }
            ids
        };
        self.changed(gone);
    }

    /// Opens an instance of `screen` on `context`, or finds the one already open. Refused when the
    /// add-on already has [`MAX_INSTANCES`] open.
    pub(crate) fn open(&self, addon: &str, screen: &str, context: ScreenContext) -> Result<Opened, ToolError> {
        let (id, ev) = {
            let mut g = self.lock();
            let Some(d) = g.declared.get(addon) else {
                return Err(ToolError::Refused(format!("the add-on `{addon}` is not running; enable it, or restart it in Settings > Add-ons")));
            };
            let (generation, addon_name) = (d.generation, d.addon_name.clone());
            let decl = d.screens.iter().find(|s| s.id == screen).cloned().ok_or_else(|| ToolError::NotFound(format!("the add-on `{addon}` has no screen `{screen}`")))?;
            if let Some((id, inst)) = g.instances.iter().find(|(_, i)| i.info.screen == screen && i.info.context == context) {
                return Ok(Opened::Existing(id.clone(), inst.info.phase));
            }
            if g.instances.values().filter(|i| i.info.addon == addon).count() >= MAX_INSTANCES {
                return Err(ToolError::Refused(format!("`{addon}` already has {MAX_INSTANCES} screens open; close one first")));
            }
            g.next_id = g.next_id.saturating_add(1);
            let id = format!("i{}", g.next_id);
            g.instances.insert(
                id.clone(),
                Instance {
                    info: ScreenInfo {
                        instance: id.clone(),
                        addon: addon.to_owned(),
                        addon_name,
                        screen: screen.to_owned(),
                        title: decl.title,
                        icon: decl.icon,
                        scope: decl.scope,
                        phase: ScreenPhase::Opening,
                        popped_out: false,
                        error: None,
                        rev: 0,
                        context,
                    },
                    generation,
                    current: None,
                    published: None,
                    dirty: false,
                    gate: RateGate::new(),
                    held: Vec::new(),
                },
            );
            (id.clone(), id)
        };
        self.changed([ev]);
        Ok(Opened::Created(id))
    }

    /// Puts a retried instance back to opening.
    pub(crate) fn reopening(&self, instance: &str) {
        {
            let mut g = self.lock();
            if let Some(i) = g.instances.get_mut(instance) {
                i.info.phase = ScreenPhase::Opening;
                i.info.error = None;
            }
        }
        self.changed([instance.to_owned()]);
    }

    /// Closes an instance; returns its add-on, to tell the plugin.
    pub(crate) fn close(&self, instance: &str) -> Option<String> {
        let addon = self.lock().instances.remove(instance).map(|i| i.info.addon);
        if addon.is_some() {
            self.changed([instance.to_owned()]);
        }
        addon
    }

    pub(crate) fn set_popped_out(&self, instance: &str, popped: bool) -> Result<(), ToolError> {
        {
            let mut g = self.lock();
            let i = g.instances.get_mut(instance).ok_or_else(|| ToolError::NotFound(format!("no open screen `{instance}`; `screen_list` shows them")))?;
            i.info.popped_out = popped;
        }
        self.changed([instance.to_owned()]);
        Ok(())
    }

    /// The answer to `screen/open`, or why there was none.
    pub(crate) fn opened(&self, addon: &str, generation: u64, instance: &str, result: Result<&Value, String>, not_answering: bool) -> Applied {
        let now = self.now_ms();
        let outcome = match result {
            Ok(v) => ScreenTree::from_render(v, addon, None).map_err(|e| e.to_string()),
            Err(e) => Err(e),
        };
        let applied = {
            let mut g = self.lock();
            let Some(inst) = g.instances.get_mut(instance).filter(|i| i.generation == generation && i.info.addon == addon) else { return Applied::Ignored };
            if inst.info.phase == ScreenPhase::Stopped {
                return Applied::Ignored;
            }
            match outcome {
                Ok(tree) => {
                    // A render that arrived before the answer may already be newer.
                    if inst.info.rev < tree.rev {
                        accept(inst, tree);
                        publish_due(inst, now, true);
                    } else {
                        inst.info.phase = ScreenPhase::Ready;
                    }
                    Applied::Yes
                }
                Err(msg) => {
                    inst.info.error = Some(msg.clone());
                    // A tree of the plugin's that was refused leaves the screen without one: the
                    // frame names the reason. A missing answer is the retry state.
                    if inst.current.is_none() || not_answering {
                        inst.info.phase = ScreenPhase::NotAnswering;
                    }
                    Applied::Refused(msg)
                }
            }
        };
        self.changed([instance.to_owned()]);
        applied
    }

    /// A `screen/render` (`patch: false`) or `screen/patch` from the process `generation` of
    /// `addon`. Validated outside the lock; the swap is the only thing done inside it.
    pub(crate) fn update(&self, addon: &str, generation: u64, params: &Value, patch: bool) -> Applied {
        let Some(instance) = params.get("instance").and_then(Value::as_str) else { return Applied::Refused("it needs an `instance`".into()) };
        let base = {
            let g = self.lock();
            match g.instances.get(instance).filter(|i| i.generation == generation && i.info.addon == addon && i.info.phase != ScreenPhase::Stopped) {
                Some(i) => (i.current.clone(), i.info.rev),
                None => return Applied::Ignored,
            }
        };
        let (current, rev) = base;
        let result = if patch {
            match &current {
                Some(t) => t.apply_patch(params, addon),
                None => return Applied::Refresh,
            }
        } else {
            ScreenTree::from_render(params, addon, (rev > 0).then_some(rev))
        };
        let now = self.now_ms();
        let applied = {
            let mut g = self.lock();
            let Some(inst) = g.instances.get_mut(instance).filter(|i| i.generation == generation && i.info.phase != ScreenPhase::Stopped) else { return Applied::Ignored };
            match result {
                Ok(tree) => {
                    accept(inst, tree);
                    publish_due(inst, now, false);
                    return Applied::Yes;
                }
                Err(UpdateError::Stale { .. }) => return Applied::Ignored,
                Err(UpdateError::BaseMismatch { .. }) => return Applied::Refresh,
                Err(UpdateError::Invalid(e)) => {
                    inst.info.error = Some(e.to_string());
                    Applied::Refused(e.to_string())
                }
            }
        };
        self.changed([instance.to_owned()]);
        applied
    }

    /// Publishes trees that were applied inside the last render interval and are due now.
    pub(crate) fn flush(&self) {
        let now = self.now_ms();
        let touched: Vec<String> = {
            let mut g = self.lock();
            g.instances.iter_mut().filter(|(_, i)| i.dirty).filter_map(|(id, i)| publish_due(i, now, false).then(|| id.clone())).collect()
        };
        self.changed(touched);
    }

    /// Holds a `change` or `visible` event that did not fit the queue (the latest of its key and
    /// kind wins). Returns false when the instance is gone.
    pub(crate) fn hold(&self, instance: &str, e: ScreenEvent) -> bool {
        let mut g = self.lock();
        let Some(i) = g.instances.get_mut(instance) else { return false };
        i.held.retain(|h| !(h.key == e.key && h.kind == e.kind));
        i.held.push(e);
        true
    }

    /// Held events and their instances' revisions, taken to be sent.
    pub(crate) fn take_held(&self, addon: &str) -> Vec<(String, u64, Vec<ScreenEvent>)> {
        let mut g = self.lock();
        g.instances
            .iter_mut()
            .filter(|(_, i)| i.info.addon == addon && !i.held.is_empty())
            .map(|(id, i)| (id.clone(), i.info.rev, std::mem::take(&mut i.held)))
            .collect()
    }

    /// The tree, revision and addon of `instance`, for sending an event.
    pub(crate) fn for_event(&self, instance: &str) -> Option<(String, u64, ScreenPhase)> {
        self.lock().instances.get(instance).map(|i| (i.info.addon.clone(), i.info.rev, i.info.phase))
    }
}

fn accept(inst: &mut Instance, tree: ScreenTree) {
    inst.info.rev = tree.rev;
    inst.info.error = None;
    inst.info.phase = ScreenPhase::Ready;
    inst.current = Some(Arc::new(tree));
    inst.dirty = true;
}

/// Publishes the latest tree if the render interval has passed (or `force`). True when it did.
fn publish_due(inst: &mut Instance, now_ms: u64, force: bool) -> bool {
    if !inst.dirty {
        return false;
    }
    if force || inst.gate.admit(now_ms) {
        inst.published = inst.current.clone();
        inst.dirty = false;
        return true;
    }
    false
}

/// What `screen/open` is about.
pub(crate) struct OpenRequest {
    pub addon: String,
    /// The process the instance belongs to.
    pub generation: u64,
    pub instance: String,
    pub screen: String,
    pub context: ScreenContext,
    pub theme: Option<String>,
}

/// Asks the plugin to open `instance`, on a thread of its own, and applies the answer. The
/// plugin has [`OPEN_TIMEOUT`]; past it (or on an error answer) the tab shows the retry state.
pub(crate) fn request_open(store: ScreenStore, link: Arc<Link>, req: OpenRequest) {
    let OpenRequest { addon, generation, instance, screen, context, theme } = req;
    let addon_for_error = addon.clone();
    let spawned = std::thread::Builder::new().name("csm-screen-open".into()).spawn({
        let (store, instance) = (store.clone(), instance.clone());
        move || {
            let mut params = json!({"screen": screen, "instance": instance, "context": context});
            if let (Some(t), Some(o)) = (theme, params.as_object_mut()) {
                o.insert("theme".into(), Value::String(t));
            }
            match link.request("screen/open", params, OPEN_TIMEOUT) {
                Ok(result) => {
                    if let Applied::Refused(msg) = store.opened(&addon, generation, &instance, Ok(&result), false) {
                        link.notify("screen/error", json!({"instance": instance, "message": msg}));
                    }
                }
                Err(ReqError::TimedOut) => {
                    store.opened(&addon, generation, &instance, Err(format!("{addon} did not answer within {} seconds", OPEN_TIMEOUT.as_secs())), true);
                }
                Err(ReqError::Refused(m) | ReqError::Failed(m)) => {
                    store.opened(&addon, generation, &instance, Err(m), true);
                }
            }
        }
    });
    if let Err(e) = spawned {
        log::error!("could not start a screen-open thread: {e}");
        store.opened(&addon_for_error, generation, &instance, Err("could not start".into()), true);
    }
}

/// Sends `event` to the plugin that owns `instance`. Never blocks: a full queue holds a `change`
/// or `visible` event (latest wins) and refuses any other with a message the UI shows.
pub(crate) fn send_event(store: &ScreenStore, link: Option<Arc<Link>>, instance: &str, event: ScreenEvent) -> Result<(), ToolError> {
    let Some((addon, rev, phase)) = store.for_event(instance) else {
        return Err(ToolError::NotFound(format!("no open screen `{instance}`")));
    };
    let Some(link) = link.filter(|_| phase != ScreenPhase::Stopped) else {
        return Err(ToolError::Refused(format!("the add-on `{addon}` is not running")));
    };
    let params = event.params(instance, rev);
    if link.notify("screen/event", params) {
        return Ok(());
    }
    if event.kind.coalesces() && store.hold(instance, event) {
        return Ok(());
    }
    Err(ToolError::Refused(format!("`{addon}` is busy; try again in a moment")))
}

/// Sends the events held back earlier, while there is room. Called from the supervisor loop.
pub(crate) fn flush_held(store: &ScreenStore, link: &Link, addon: &str) {
    for (instance, rev, events) in store.take_held(addon) {
        for (n, e) in events.iter().enumerate() {
            if !link.notify("screen/event", e.params(&instance, rev)) {
                // Still full: keep the rest for the next round.
                for rest in events.into_iter().skip(n) {
                    store.hold(&instance, rest);
                }
                return;
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use csm_core::addons::screen::EventKind;

    use super::*;

    fn decl(id: &str) -> ScreenDecl {
        ScreenDecl { id: id.into(), title: "T".into(), icon: String::new(), scope: ScreenScope::Global, summary: String::new() }
    }

    fn store() -> ScreenStore {
        let s = ScreenStore::new(Events::default());
        assert!(s.set_declared("demo", 1, "Demo", vec![decl("demo.main"), decl("demo.other")]).is_empty());
        s
    }

    fn render(instance: &str, rev: u64, text: &str) -> Value {
        json!({"instance": instance, "rev": rev, "root": {"type": "text", "text": text}})
    }

    fn open(s: &ScreenStore) -> String {
        match s.open("demo", "demo.main", ScreenContext::default()).unwrap() {
            Opened::Created(i) => i,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn open_dedups_by_screen_and_context_and_is_capped() {
        let s = store();
        let a = open(&s);
        assert!(matches!(s.open("demo", "demo.main", ScreenContext::default()).unwrap(), Opened::Existing(i, ScreenPhase::Opening) if i == a));
        let ctx = ScreenContext { project: Some("repo:x".into()), ..ScreenContext::default() };
        assert!(matches!(s.open("demo", "demo.main", ctx.clone()).unwrap(), Opened::Created(_)));
        assert!(matches!(s.open("demo", "demo.nope", ScreenContext::default()), Err(ToolError::NotFound(_))));
        assert!(matches!(s.open("ghost", "ghost.x", ScreenContext::default()), Err(ToolError::Refused(_))));
        for n in 0..MAX_INSTANCES {
            let _ = s.open("demo", "demo.other", ScreenContext { path: Some(format!("p{n}")), ..ScreenContext::default() });
        }
        assert_eq!(s.instances().len(), MAX_INSTANCES);
        assert!(matches!(s.open("demo", "demo.other", ScreenContext { path: Some("one more".into()), ..ScreenContext::default() }), Err(ToolError::Refused(m)) if m.contains("close one")));
        assert_eq!(s.instances()[0].instance, a, "instances list in the order they were opened");
    }

    #[test]
    fn the_open_answer_shows_a_tree_and_a_bad_one_does_not() {
        let s = store();
        let i = open(&s);
        let ok = json!({"rev": 1, "root": {"type": "text", "text": "hi"}});
        assert_eq!(s.opened("demo", 1, &i, Ok(&ok), false), Applied::Yes);
        assert_eq!(s.instance(&i).unwrap().phase, ScreenPhase::Ready);
        assert!(s.tree(&i).is_some());
        // An answer from an older process, or for another instance, is ignored.
        assert_eq!(s.opened("demo", 0, &i, Ok(&ok), false), Applied::Ignored);
        assert_eq!(s.opened("demo", 1, "nope", Ok(&ok), false), Applied::Ignored);
        let j = match s.open("demo", "demo.main", ScreenContext { path: Some("b".into()), ..ScreenContext::default() }).unwrap() {
            Opened::Created(j) => j,
            _ => panic!(),
        };
        let bad = json!({"rev": 1, "root": {"type": "text_input"}});
        assert!(matches!(s.opened("demo", 1, &j, Ok(&bad), false), Applied::Refused(m) if m.contains("key")));
        let info = s.instance(&j).unwrap();
        assert_eq!(info.phase, ScreenPhase::NotAnswering);
        assert!(info.error.is_some() && s.tree(&j).is_none());
        // A timeout is the retry state too.
        assert!(matches!(s.opened("demo", 1, &j, Err("late".into()), true), Applied::Refused(_)));
    }

    #[test]
    fn renders_and_patches_apply_in_order_and_refuse_bad_ones_keeping_the_old_tree() {
        let s = store();
        let i = open(&s);
        let first = json!({"rev": 1, "root": {"type": "column", "key": "c", "children": [{"type": "text", "key": "t", "text": "one"}]}});
        s.opened("demo", 1, &i, Ok(&first), false);
        // A patch that applies (the gate lets the first publish through at once; later ones wait).
        let p = json!({"instance": i, "rev": 2, "base": 1, "ops": [{"op": "replace", "key": "t", "node": {"type": "text", "key": "t", "text": "two"}}]});
        assert_eq!(s.update("demo", 1, &p, true), Applied::Yes);
        assert_eq!(s.instance(&i).unwrap().rev, 2);
        // Stale and wrong-base ones.
        assert_eq!(s.update("demo", 1, &p, true), Applied::Ignored);
        let wrong = json!({"instance": i, "rev": 9, "base": 5, "ops": []});
        assert_eq!(s.update("demo", 1, &wrong, true), Applied::Refresh);
        assert_eq!(s.update("demo", 1, &render(&i, 2, "x"), false), Applied::Ignored, "a render must be newer too");
        // A refused tree keeps the previous one and names the reason.
        let bad = json!({"instance": i, "rev": 10, "root": {"type": "text_input", "value": "x"}});
        assert!(matches!(s.update("demo", 1, &bad, false), Applied::Refused(_)));
        let info = s.instance(&i).unwrap();
        assert_eq!((info.rev, info.phase), (2, ScreenPhase::Ready));
        assert!(info.error.is_some());
        // The next good render clears the error.
        assert_eq!(s.update("demo", 1, &render(&i, 11, "fine"), false), Applied::Yes);
        assert!(s.instance(&i).unwrap().error.is_none());
        // Not for a live instance of this process.
        assert_eq!(s.update("demo", 7, &render(&i, 12, "x"), false), Applied::Ignored);
        assert_eq!(s.update("demo", 1, &render("zzz", 1, "x"), false), Applied::Ignored);
        assert!(matches!(s.update("demo", 1, &json!({"rev": 1}), false), Applied::Refused(_)));
        // A patch before any tree asks for a render.
        let k = match s.open("demo", "demo.main", ScreenContext { path: Some("k".into()), ..ScreenContext::default() }).unwrap() {
            Opened::Created(k) => k,
            _ => panic!(),
        };
        assert_eq!(s.update("demo", 1, &json!({"instance": k, "rev": 2, "base": 1, "ops": []}), true), Applied::Refresh);
    }

    #[test]
    fn publishing_is_rate_limited_and_the_latest_wins() {
        let s = store();
        let i = open(&s);
        s.opened("demo", 1, &i, Ok(&json!({"rev": 1, "root": {"type": "text", "text": "v1"}})), false);
        let shown = |s: &ScreenStore| match &s.tree(&i).unwrap().root.kind {
            csm_core::addons::screen::Kind::Text { text, .. } => text.clone(),
            _ => panic!(),
        };
        assert_eq!(shown(&s), "v1");
        // A burst inside the interval: the gate admitted the first publish a moment ago, so these are held.
        let mut seen = std::collections::BTreeSet::new();
        for rev in 2..40u64 {
            assert_eq!(s.update("demo", 1, &render(&i, rev, &format!("v{rev}")), false), Applied::Yes);
            seen.insert(shown(&s));
        }
        assert!(seen.len() < 20, "a burst is not published render by render ({} of 38 shown)", seen.len());
        assert_eq!(s.instance(&i).unwrap().rev, 39, "but every render was applied to the latest tree");
        // After the interval the latest is published.
        let deadline = Instant::now() + Duration::from_secs(5);
        while shown(&s) != "v39" {
            s.flush();
            assert!(Instant::now() < deadline, "the latest render was never published");
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    #[test]
    fn a_stopped_process_greys_its_screens_and_a_new_one_reopens_them() {
        let s = store();
        let i = open(&s);
        s.opened("demo", 1, &i, Ok(&json!({"rev": 1, "root": {"type": "text", "text": "x"}})), false);
        s.mark_stopped("demo", 1);
        assert!(s.declared().is_empty(), "a stopped add-on offers no screens");
        let info = s.instance(&i).unwrap();
        assert_eq!(info.phase, ScreenPhase::Stopped);
        assert!(s.tree(&i).is_some(), "the last tree stays");
        assert_eq!(s.update("demo", 1, &render(&i, 5, "late"), false), Applied::Ignored);
        // An older process stopping later does not touch a newer one's state.
        let again = s.set_declared("demo", 2, "Demo", vec![decl("demo.main")]);
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].0, i);
        assert_eq!(s.instance(&i).unwrap().phase, ScreenPhase::Opening);
        s.mark_stopped("demo", 1);
        assert_eq!(s.instance(&i).unwrap().phase, ScreenPhase::Opening);
        // A restart that no longer declares the screen closes it.
        s.mark_stopped("demo", 2);
        assert!(s.set_declared("demo", 3, "Demo", vec![decl("demo.other")]).is_empty());
        assert!(s.instance(&i).is_none());
    }

    #[test]
    fn close_remove_and_pop_out() {
        let s = store();
        let i = open(&s);
        s.set_popped_out(&i, true).unwrap();
        assert!(s.instance(&i).unwrap().popped_out);
        assert!(matches!(s.set_popped_out("zzz", true), Err(ToolError::NotFound(_))));
        assert_eq!(s.close(&i).as_deref(), Some("demo"));
        assert_eq!(s.close(&i), None);
        let j = open(&s);
        s.remove_addon("demo");
        assert!(s.instance(&j).is_none() && s.declared().is_empty());
    }

    #[test]
    fn events_coalesce_when_held_and_refuse_when_the_instance_is_gone() {
        let s = store();
        let i = open(&s);
        assert!(s.hold(&i, ScreenEvent::new("q", EventKind::Change, json!("a"))));
        assert!(s.hold(&i, ScreenEvent::new("q", EventKind::Change, json!("ab"))));
        assert!(s.hold(&i, ScreenEvent::new("r", EventKind::Change, json!("z"))));
        let held = s.take_held("demo");
        assert_eq!(held.len(), 1);
        assert_eq!(held[0].2.len(), 2, "one per key");
        assert_eq!(held[0].2.iter().find(|e| e.key == "q").unwrap().value, json!("ab"), "the latest wins");
        assert!(s.take_held("demo").is_empty());
        assert!(!s.hold("nope", ScreenEvent::new("q", EventKind::Change, json!(1))));
        assert!(matches!(send_event(&s, None, "nope", ScreenEvent::new("q", EventKind::Click, Value::Null)), Err(ToolError::NotFound(_))));
        assert!(matches!(send_event(&s, None, &i, ScreenEvent::new("q", EventKind::Click, Value::Null)), Err(ToolError::Refused(_))), "no link: the add-on is not running");
    }
}
