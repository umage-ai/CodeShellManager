//! A remote window: the UI over a paired CodeShellManager (`docs/design/remote-csm.md`, sections 4.1,
//! 5 and 6, P1 step 7).
//!
//! The window runs the normal UI over [`crate::Backend`] of a remote host. This module holds the
//! parts that differ, as plain data and pure functions so they can be tested without a socket:
//!
//! - [`RemoteStatus`]: the host's name and address and the state of the link, for the strip under
//!   the title bar and the window title.
//! - [`is_local_only`]: the tools whose effect is on the laptop, which a remote window hides and
//!   refuses (open folder, open with, backup, the connect dialog).
//! - [`check_link`]: a link the person Ctrl+clicks is opened in the laptop's browser, after the
//!   same http/https check the engine makes; a localhost link is flagged, because it points at the
//!   office's own machine.
//! - [`ConnectDialog`]: the state of "Connect to another CodeShellManager…" (the link, the device
//!   name, the check code, the paired offices), read from the engine's `peer_client_*` tools.

use serde_json::Value;

/// How the window's link to its host stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RemoteLink {
    /// The first connection is being made.
    Connecting,
    /// Connected: the host answers and its snapshot is shown.
    Connected,
    /// The link is down and the window is trying again (or the host has stopped).
    Offline,
    /// The host refused this laptop (revoked, another version). Retrying cannot help.
    Refused(String),
}

/// The host a remote window shows, and its link.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteStatus {
    /// The host's name as the host shows it ("OFFICE").
    pub host_name: String,
    /// The address the window dials, as text (`10.8.0.2:47300`).
    pub address: String,
    pub link: RemoteLink,
}

impl RemoteStatus {
    /// The strip's text. Always names the host and the address, so a remote window is never
    /// taken for a local one.
    pub fn bar_text(&self) -> String {
        match &self.link {
            RemoteLink::Connecting => format!("Connecting to {} · {}", self.host_name, self.address),
            RemoteLink::Connected => format!("Connected to {} · {}", self.host_name, self.address),
            RemoteLink::Offline => format!("{} offline · reconnecting · {}", self.host_name, self.address),
            RemoteLink::Refused(why) => format!("{} refused this laptop: {why}", self.host_name),
        }
    }

    /// The window title, prefixed with the host's name.
    pub fn window_title(&self) -> String {
        format!("{} · CodeShellManager", self.host_name)
    }

    /// Whether the window can act on the host right now (calls are sent).
    pub fn is_live(&self) -> bool {
        self.link == RemoteLink::Connected
    }
}

/// The tools whose effect is on the laptop, not the office. A remote window hides them from its
/// menus and palette and refuses them when called: they would open a folder, run an editor, read
/// or write a backup on the laptop, or pair another office from a window that cannot answer.
pub const LOCAL_ONLY: &[&str] = &[
    "open_folder",
    "open_with",
    "open_with_set",
    "open_with_detect",
    "state_export",
    "state_import",
    "state_import_wpf",
    "peer_connect_open",
    "remote_window_open",
    "peer_client_pair",
    "peer_client_pair_cancel",
    "peer_host_forget",
];

/// Whether a remote window hides and refuses this tool.
pub fn is_local_only(tool: &str) -> bool {
    LOCAL_ONLY.contains(&tool)
}

/// What a remote window says when it refuses a local-only action.
pub fn refusal_text(what: &str) -> String {
    format!("{what} runs on this laptop, so it is not available in a remote window.")
}

/// Where a link opens, decided before the browser is started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// An ordinary http or https address: open it at once.
    Public(String),
    /// A loopback address: it names the office's own machine. Ask first.
    Localhost(String),
}

/// Checks a link the way the engine does (absolute http or https, no whitespace or quotes), then
/// tells a loopback address apart. `None`: the link is not one this app opens.
pub fn check_link(url: &str) -> Option<LinkTarget> {
    let ok = csm_engine::runs::launchable_url(url)?;
    if is_loopback(&ok) { Some(LinkTarget::Localhost(ok)) } else { Some(LinkTarget::Public(ok)) }
}

/// Whether an http(s) URL's host is `localhost`, `127.0.0.1`, `::1` or `*.localhost`.
pub fn is_loopback(url: &str) -> bool {
    let Some((_, rest)) = url.split_once("://") else { return false };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    // The host follows the last `@` (no user information); a bracketed IPv6 address ends at `]`.
    let hostport = authority.rsplit('@').next().unwrap_or("");
    let host = match hostport.strip_prefix('[') {
        Some(v6) => v6.split(']').next().unwrap_or(""),
        None => hostport.split(':').next().unwrap_or(""),
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    host == "localhost" || host == "127.0.0.1" || host == "::1" || host.ends_with(".localhost")
}

/// The warning shown before a loopback link opens: it is the office's machine, not the laptop's.
pub fn localhost_warning(host_name: &str, url: &str) -> (String, String) {
    (
        format!("This link points to {host_name}"),
        format!("{url} is on {host_name}'s own machine (localhost), not on this laptop. It opens in this laptop's browser, where nothing may be listening."),
    )
}

// ---- the connect dialog ----------------------------------------------------------------------

/// Where a pairing stands, as the dialog shows it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PairPhase {
    #[default]
    Idle,
    /// Dialling the office and running the handshake.
    Connecting,
    /// The four digits to compare with the office's screen; the office's person must allow it.
    CheckCode(String),
    /// Allowed: the office is stored under this name.
    Paired(String),
    /// Refused or failed, with the reason.
    Failed(String),
    Cancelled,
}

/// One paired office in the dialog's list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRow {
    pub host_id: String,
    pub name: String,
    pub address: String,
    pub last_connected: Option<u64>,
}

/// The state of "Connect to another CodeShellManager…".
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConnectDialog {
    /// The pairing link as pasted.
    pub link: String,
    /// The name this laptop is shown under (empty: the office names it after the computer).
    pub name: String,
    pub phase: PairPhase,
    /// The paired offices, from `peer_hosts_list`.
    pub hosts: Vec<HostRow>,
    /// Whether `hosts` has been read at least once.
    pub hosts_loaded: bool,
    /// A refusal to show above the form (a bad link, an unreadable host list).
    pub error: Option<String>,
}

impl ConnectDialog {
    /// Whether "Pair" may be pressed: a link is pasted and no pairing is running.
    pub fn can_pair(&self) -> bool {
        !self.link.trim().is_empty() && !self.busy()
    }

    /// Whether a pairing runs (its status is polled while it does).
    pub fn busy(&self) -> bool {
        matches!(self.phase, PairPhase::Connecting | PairPhase::CheckCode(_))
    }

    /// Takes one `peer_client_pair_status` (or `peer_client_pair`) answer. An unknown state changes
    /// nothing: the dialog keeps what it showed.
    pub fn apply_status(&mut self, data: &Value) {
        let Some(state) = data.get("state").and_then(Value::as_str) else { return };
        let text = |k: &str| data.get(k).and_then(Value::as_str).map(str::to_owned);
        self.phase = match state {
            "idle" => PairPhase::Idle,
            "connecting" => PairPhase::Connecting,
            "check_code" => PairPhase::CheckCode(text("check_code").unwrap_or_default()),
            "paired" => PairPhase::Paired(text("host_name").unwrap_or_else(|| "the office".into())),
            "failed" => PairPhase::Failed(text("message").unwrap_or_else(|| "The pairing did not happen.".into())),
            "cancelled" => PairPhase::Cancelled,
            _ => return,
        };
    }

    /// Takes a `peer_hosts_list` answer (an array of host rows). A damaged row is skipped; an
    /// answer that is not an array changes nothing.
    pub fn apply_hosts(&mut self, data: &Value) {
        let Some(rows) = data.as_array() else { return };
        self.hosts = rows
            .iter()
            .filter_map(|r| {
                Some(HostRow {
                    host_id: r.get("host_id")?.as_str()?.to_owned(),
                    name: r.get("name")?.as_str()?.to_owned(),
                    address: r.get("address").and_then(Value::as_str).unwrap_or("").to_owned(),
                    last_connected: r.get("last_connected").and_then(Value::as_u64),
                })
            })
            .collect();
        self.hosts_loaded = true;
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;
    use serde_json::json;

    fn status(link: RemoteLink) -> RemoteStatus {
        RemoteStatus { host_name: "OFFICE".into(), address: "10.8.0.2:47300".into(), link }
    }

    #[test]
    fn the_strip_names_the_host_and_the_address_in_every_state() {
        assert_eq!(status(RemoteLink::Connected).bar_text(), "Connected to OFFICE · 10.8.0.2:47300");
        assert_eq!(status(RemoteLink::Connecting).bar_text(), "Connecting to OFFICE · 10.8.0.2:47300");
        assert_eq!(status(RemoteLink::Offline).bar_text(), "OFFICE offline · reconnecting · 10.8.0.2:47300");
        assert_eq!(status(RemoteLink::Refused("revoked".into())).bar_text(), "OFFICE refused this laptop: revoked");
        assert_eq!(status(RemoteLink::Connected).window_title(), "OFFICE · CodeShellManager");
        assert!(status(RemoteLink::Connected).is_live());
        assert!(!status(RemoteLink::Offline).is_live());
    }

    #[test]
    fn the_laptop_only_tools_are_hidden_and_the_office_ones_are_not() {
        for id in ["open_folder", "open_with", "state_export", "state_import", "peer_connect_open", "remote_window_open"] {
            assert!(is_local_only(id), "{id}");
        }
        for id in ["session_list", "session_send_text", "app_quit", "git_status", "open_url", "peer_status"] {
            assert!(!is_local_only(id), "{id}");
        }
        assert!(refusal_text("Open folder").contains("not available in a remote window"));
    }

    #[test]
    fn links_are_checked_like_the_engine_and_loopback_is_flagged() {
        assert_eq!(check_link("https://example.com/a?b=1"), Some(LinkTarget::Public("https://example.com/a?b=1".into())));
        assert_eq!(check_link("file:///C:/secret.txt"), None);
        assert_eq!(check_link("javascript:alert(1)"), None);
        assert_eq!(check_link("https://example.com/ x"), None, "whitespace is refused");
        assert!(matches!(check_link("http://localhost:3000/"), Some(LinkTarget::Localhost(_))));
        assert!(matches!(check_link("http://127.0.0.1:8080"), Some(LinkTarget::Localhost(_))));
        assert!(matches!(check_link("http://[::1]:8080/x"), Some(LinkTarget::Localhost(_))));
        assert!(matches!(check_link("https://app.localhost/"), Some(LinkTarget::Localhost(_))));
        assert!(matches!(check_link("https://localhost.example.com/"), Some(LinkTarget::Public(_))), "only the host name counts");
        assert!(matches!(check_link("https://user@127.0.0.1.example.org/"), Some(LinkTarget::Public(_))));
        assert!(matches!(check_link("https://evil.example/?x=localhost"), Some(LinkTarget::Public(_))));
        let (title, body) = localhost_warning("OFFICE", "http://localhost:3000/");
        assert_eq!(title, "This link points to OFFICE");
        assert!(body.contains("OFFICE's own machine"));
    }

    #[test]
    fn the_dialog_follows_the_pairing_answers() {
        let mut d = ConnectDialog { link: "csm://pair#abc".into(), ..Default::default() };
        assert!(d.can_pair());
        d.apply_status(&json!({"state": "connecting"}));
        assert!(d.busy() && !d.can_pair());
        d.apply_status(&json!({"state": "check_code", "check_code": "4821"}));
        assert_eq!(d.phase, PairPhase::CheckCode("4821".into()));
        d.apply_status(&json!({"state": "paired", "host_name": "OFFICE", "host_id": "h1"}));
        assert_eq!(d.phase, PairPhase::Paired("OFFICE".into()));
        assert!(!d.busy() && d.can_pair());
        d.apply_status(&json!({"state": "failed", "message": "the office refused"}));
        assert_eq!(d.phase, PairPhase::Failed("the office refused".into()));
        d.apply_status(&json!({"state": "cancelled"}));
        assert_eq!(d.phase, PairPhase::Cancelled);
        d.apply_status(&json!({"state": "something new"}));
        assert_eq!(d.phase, PairPhase::Cancelled, "an unknown state changes nothing");
        d.link = "  ".into();
        assert!(!d.can_pair(), "a blank link cannot be paired");
    }

    #[test]
    fn the_host_list_skips_damaged_rows() {
        let mut d = ConnectDialog::default();
        d.apply_hosts(&json!([
            {"host_id": "h1", "name": "OFFICE", "address": "tcp://10.8.0.2:47300", "last_connected": 5},
            {"name": "no id"},
            {"host_id": "h2", "name": "Home", "paired_at": 1}
        ]));
        assert!(d.hosts_loaded);
        assert_eq!(d.hosts.len(), 2);
        assert_eq!(d.hosts[0].last_connected, Some(5));
        assert_eq!(d.hosts[1].address, "");
        d.apply_hosts(&json!("nonsense"));
        assert_eq!(d.hosts.len(), 2, "a list that is not an array changes nothing");
    }
}
