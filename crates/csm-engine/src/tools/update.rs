//! The update check (`update_check`, `update_dismiss`, the `CheckForUpdates` setting): the
//! WPF app's `UpdateService` (0.x), ported.
//!
//! - **What:** one GET of GitHub's releases of `umage-ai/CodeShellManager` (`releases/latest` for
//!   a final build, the release list for a pre-release build; `csm_core::update` has the rules),
//!   compared with the running version. A newer release is shown as a badge in the title bar that
//!   opens its release page (`open_url`). Nothing is downloaded or installed.
//! - **When (on its own):** 5 seconds after start (0.x waited 4), then once a day while the app
//!   runs; an hour after a failed check. Only with `CheckForUpdates` on (the default), only in a run
//!   that saves state (never `--clean` or an isolated run), and only for a released version 2
//!   build (a local `0.0.1` build never asks). `update_check` asks at once, whatever those say.
//! - **How:** on a worker thread, with a 10 s deadline (`csm_services::update`); the engine
//!   thread never waits. The request carries a generic User-Agent (`CodeShellManager/<version>`)
//!   and nothing else about the person.
//! - The result is runtime only: nothing is written to disk (0.x kept a cache file).

use std::sync::Arc;
use std::time::{Duration, Instant};

use csm_core::update::{self, Offer};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::engine::{Ctx, ParkedReply, Reply};
use crate::tool::{Effects, ToolError, ToolOutput, ToolSpec, Toolset, empty_schema, parse_params, schema_of};

/// GETs a GitHub API URL and returns the body (blocking; called on a worker).
pub type UpdateFetch = Arc<dyn Fn(&str) -> Result<String, String> + Send + Sync>;

/// How the update check runs. `Default` (tests, the demo, tools) never touches the network.
#[derive(Clone)]
pub struct UpdateOptions {
    /// The request. `None`: this engine has no network check (`update_check` says so).
    pub fetch: Option<UpdateFetch>,
    /// Check on a schedule (production). Even then only in a run that saves state.
    pub auto: bool,
    /// The running version, compared with the releases.
    pub current: String,
    /// The first scheduled check, after the engine starts.
    pub first_delay: Duration,
    /// Between scheduled checks.
    pub interval: Duration,
    /// After a failed scheduled check.
    pub retry: Duration,
}

impl Default for UpdateOptions {
    fn default() -> Self {
        UpdateOptions {
            fetch: None,
            auto: false,
            current: env!("CARGO_PKG_VERSION").to_owned(),
            first_delay: Duration::from_secs(5),
            interval: Duration::from_secs(24 * 3600),
            retry: Duration::from_secs(3600),
        }
    }
}

impl UpdateOptions {
    /// The production check: GitHub over HTTPS, on a schedule.
    pub fn production() -> Self {
        let current = env!("CARGO_PKG_VERSION");
        UpdateOptions { fetch: Some(Arc::new(move |url: &str| csm_services::update::fetch(url, current))), auto: true, ..Self::default() }
    }
}

/// What the last check found (runtime only).
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Checked {
    pub at_ms: i64,
    pub offer: Option<Offer>,
}

/// The check's runtime state.
#[derive(Debug, Default)]
pub(crate) struct UpdateRt {
    /// The last successful check.
    pub last: Option<Checked>,
    /// When the next scheduled check is due (set on the first tick).
    pub next_due: Option<Instant>,
    /// A scheduled check is running.
    pub busy: bool,
    /// The version whose badge the person dismissed (until a newer one is found or the app restarts).
    pub dismissed: Option<String>,
}

/// The title bar's badge: a newer release, not dismissed.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct UpdateView {
    /// The running version.
    pub current: String,
    /// The newer release (`2.1.0`).
    pub version: String,
    /// Its release page.
    pub url: String,
    pub prerelease: bool,
}

pub(super) fn specs() -> Vec<ToolSpec> {
    vec![
        ToolSpec {
            id: "update_check",
            title: "Check for updates",
            summary: "Ask GitHub now whether a newer CodeShellManager is released (a beta or rc build also sees newer pre-releases of its kind). Returns the running and the newest version and the release page; nothing is downloaded. Runs even with CheckForUpdates off.",
            toolset: Toolset::App,
            effects: Effects::IDEMPOTENT.union(Effects::ACCESSES_NETWORK),
            input_schema: empty_schema,
            run: update_check,
        },
        ToolSpec {
            id: "update_dismiss",
            title: "Dismiss the update notice",
            summary: "Hide the title bar's \"Update available\" badge for this version until the app restarts (a newer release shows it again).",
            toolset: Toolset::App,
            effects: Effects::UI,
            input_schema: schema_of::<DismissParams>,
            run: update_dismiss,
        },
    ]
}

/// The badge, if there is one to show.
pub(crate) fn view(state: &crate::state::State) -> Option<UpdateView> {
    let rt = &state.rt.update;
    let offer = rt.last.as_ref()?.offer.as_ref()?;
    if rt.dismissed.as_deref() == Some(offer.version.as_str()) {
        return None;
    }
    Some(UpdateView { current: state.options.update.current.clone(), version: offer.version.clone(), url: offer.url.clone(), prerelease: offer.prerelease })
}

/// Whether this run checks on its own now (the setting, the run, the build).
fn scheduled(state: &crate::state::State) -> bool {
    let o = &state.options.update;
    o.auto && o.fetch.is_some() && state.options.saves() && state.app.settings.check_for_updates && update::auto_check_applies(&o.current)
}

/// The schedule, on every engine wake-up.
pub(crate) fn tick(ctx: &mut Ctx<'_>) {
    if !scheduled(ctx.state) {
        return;
    }
    let now = Instant::now();
    let first = ctx.state.options.update.first_delay;
    let due = *ctx.state.rt.update.next_due.get_or_insert(now + first);
    if now < due || ctx.state.rt.update.busy {
        return;
    }
    ctx.state.rt.update.busy = true;
    if let Err(e) = start(ctx, None, true) {
        log::warn!("update check: {e}");
        ctx.state.rt.update.busy = false;
        ctx.state.rt.update.next_due = Some(now + ctx.state.options.update.retry);
    }
}

/// Starts one check on a worker. `reply`: answer an `update_check` call; `scheduled`: it is the
/// schedule's (which then sets the next one).
fn start(ctx: &mut Ctx<'_>, reply: Option<ParkedReply>, scheduled: bool) -> Result<(), ToolError> {
    let Some(fetch) = ctx.state.options.update.fetch.clone() else {
        return Err(ToolError::Failed("this copy of the app does not check for updates".into()));
    };
    let current = ctx.state.options.update.current.clone();
    let engine = ctx.engine.clone();
    let worker = move || {
        let result = match update::parse_version(&current) {
            None => Err(format!("the running version `{current}` is not a version number")),
            Some(v) => {
                let feed = update::feed_for(&v);
                fetch(feed.url()).and_then(|body| update::evaluate(&current, &body)).map(|offer| (feed, offer))
            }
        };
        engine.post(Box::new(move |ctx| finish(ctx, result, reply, scheduled)));
    };
    std::thread::Builder::new().name("csm-update-check".into()).spawn(worker).map(|_| ()).map_err(|e| ToolError::Failed(format!("could not start the update check: {e}")))
}

/// Back on the engine thread: keep the result, schedule the next check, answer the call.
fn finish(ctx: &mut Ctx<'_>, result: Result<(update::Feed, Option<Offer>), String>, reply: Option<ParkedReply>, scheduled: bool) {
    let rt = &mut ctx.state.rt.update;
    let o = &ctx.state.options.update;
    if scheduled {
        rt.busy = false;
        rt.next_due = Some(Instant::now() + if result.is_ok() { o.interval } else { o.retry });
    }
    let current = o.current.clone();
    let r: Reply = match result {
        Err(e) => {
            log::info!("update check: {e}");
            Err(ToolError::Failed(format!("could not check for updates: {e}")))
        }
        Ok((feed, offer)) => {
            match &offer {
                Some(of) => log::info!("update check: {} is available (running {current})", of.version),
                None => log::info!("update check: {current} is up to date"),
            }
            rt.last = Some(Checked { at_ms: crate::session::now_ms(), offer: offer.clone() });
            ctx.state.touch();
            let message = match &offer {
                Some(of) => format!("CodeShellManager {} is available (this is {current}).", of.version),
                None => format!("CodeShellManager {current} is up to date."),
            };
            Ok(ToolOutput::new(
                message,
                json!({
                    "current": current,
                    "update_available": offer.is_some(),
                    "latest": offer.as_ref().map(|o| o.version.clone()),
                    "prerelease": offer.as_ref().map(|o| o.prerelease),
                    "url": offer.as_ref().map(|o| o.url.clone()),
                    "feed": feed.id(),
                }),
            ))
        }
    };
    if let Some(reply) = reply {
        reply.send(r);
    }
}

fn update_check(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let _: Empty = parse_params(params)?;
    if ctx.state.options.update.fetch.is_none() {
        return Err(ToolError::Failed("this copy of the app does not check for updates".into()));
    }
    let reply = ctx.park();
    start(ctx, reply, false)?;
    Ok(ToolOutput::message("Checking for updates."))
}

#[derive(Deserialize, schemars::JsonSchema, Default)]
#[serde(deny_unknown_fields)]
struct Empty {}

#[derive(Deserialize, schemars::JsonSchema, Default)]
#[serde(default, deny_unknown_fields)]
struct DismissParams {
    /// The version whose notice to hide. Default: the one shown now.
    version: Option<String>,
}

fn update_dismiss(ctx: &mut Ctx<'_>, params: Value) -> Reply {
    let p: DismissParams = parse_params(params)?;
    let shown = view(ctx.state).map(|v| v.version);
    let Some(version) = p.version.or(shown) else {
        return Ok(ToolOutput::message("No update notice is shown."));
    };
    ctx.state.rt.update.dismissed = Some(version.clone());
    ctx.state.touch();
    Ok(ToolOutput::new(format!("The notice for {version} is hidden."), json!({"version": version})))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_options_never_touch_the_network() {
        let o = UpdateOptions::default();
        assert!(o.fetch.is_none() && !o.auto);
        assert_eq!(o.current, env!("CARGO_PKG_VERSION"));
        let p = UpdateOptions::production();
        assert!(p.fetch.is_some() && p.auto);
    }
}
