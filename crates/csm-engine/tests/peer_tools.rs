//! The `peer_*` tools and who may call them. A recording host stands in for the real server
//! (`csm-peer`, one layer up), so these tests are about the engine's rules: server mode is
//! refused, before any consent card, for every caller but the app's own UI; closing a door is
//! allowed for everyone; none of it is a setting; and a paired Full device may answer consent
//! cards while a lower level may not.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::{H, test_app};
use csm_engine::{
    AuditEvent, Caller, DeviceInfo, EnableRequest, Event, PairAnswer, PairingStarted, PeerHost, PeerStatus, Revoke, ToolError,
};
use csm_peer_proto::msg::Level;
use serde_json::{Value, json};

#[derive(Default)]
struct Log {
    calls: Vec<String>,
    audit: Vec<AuditEvent>,
}

#[derive(Clone, Default)]
struct Recorder(Arc<Mutex<Log>>);

impl Recorder {
    fn calls(&self) -> Vec<String> {
        self.0.lock().unwrap().calls.clone()
    }
    fn audit(&self) -> Vec<AuditEvent> {
        self.0.lock().unwrap().audit.clone()
    }
    fn note(&self, what: &str) {
        self.0.lock().unwrap().calls.push(what.to_owned());
    }
}

impl PeerHost for Recorder {
    fn status(&self) -> PeerStatus {
        PeerStatus { available: true, ..PeerStatus::default() }
    }
    fn enable(&self, req: EnableRequest, by: &str) -> Result<PeerStatus, ToolError> {
        self.note(&format!("enable {} by {by}", req.addr));
        Ok(PeerStatus { available: true, enabled: true, listening: Some(req.addr.to_string()), ..PeerStatus::default() })
    }
    fn disable(&self, by: &str) -> PeerStatus {
        self.note(&format!("disable by {by}"));
        PeerStatus { available: true, ..PeerStatus::default() }
    }
    fn pair_start(&self, by: &str) -> Result<PairingStarted, ToolError> {
        self.note(&format!("pair_start by {by}"));
        Ok(PairingStarted { link: "csm://pair#x".into(), address: "tcp://127.0.0.1:1".into(), expires_in_secs: 600 })
    }
    fn pair_answer(&self, a: PairAnswer, by: &str) -> Result<PeerStatus, ToolError> {
        self.note(&format!("pair_answer {} by {by}", a.allow));
        Ok(self.status())
    }
    fn devices(&self) -> Vec<DeviceInfo> {
        Vec::new()
    }
    fn revoke(&self, t: Revoke, by: &str) -> Result<usize, ToolError> {
        self.note(&format!("revoke {t:?} by {by}"));
        Ok(1)
    }
    fn audit(&self, event: AuditEvent) {
        self.0.lock().unwrap().audit.push(event);
    }
    fn shutdown(&self) {}
}

fn rig() -> (H, Recorder) {
    let rec = Recorder::default();
    let host: Arc<dyn PeerHost> = Arc::new(rec.clone());
    let h = H::start(test_app(), |o| o.peer_host = host);
    (h, rec)
}

fn mcp() -> Caller {
    Caller::External { client: "an-agent".into(), session_id: Some("s1".into()) }
}

fn peer(level: Level) -> Caller {
    Caller::Peer { device_id: "abcdefghijklmnopqrstuvwxyz".chars().take(26).collect(), name: "Laptop".into(), level }
}

const ENABLE: &str = r#"{"address":"10.8.0.2","port":47300}"#;

fn params(s: &str) -> Value {
    serde_json::from_str(s).unwrap()
}

#[test]
fn the_tools_that_open_a_way_in_are_refused_for_every_caller_but_the_ui() {
    let (h, rec) = rig();
    let callers = [Caller::Cli, mcp(), Caller::External { client: "x".into(), session_id: None }, peer(Level::Full), peer(Level::Viewer), peer(Level::Operator)];
    for caller in callers {
        for (tool, p) in [
            ("peer_server_enable", params(ENABLE)),
            ("peer_server_enable", params(r#"{"address":"0.0.0.0","all_interfaces":true}"#)),
            ("peer_pair_start", json!({})),
            ("peer_pair_answer", json!({"allow": true, "check_code": "1234"})),
        ] {
            let r = h.engine.call(caller.clone(), tool, p);
            match r {
                Err(ToolError::Refused(m)) => assert!(m.contains("app's own window"), "{m}"),
                other => panic!("{tool} for {caller:?} must be refused, got {other:?}"),
            }
        }
    }
    assert!(rec.calls().is_empty(), "the host was never reached: {:?}", rec.calls());
    let refused = rec.audit().into_iter().filter(|e| matches!(e, AuditEvent::Refused { .. })).count();
    assert!(refused >= 24, "every refusal is logged, got {refused}");
}

#[test]
fn a_refusal_comes_before_any_consent_card() {
    let (h, rec) = rig();
    let events = h.engine.subscribe();
    let r = h.engine.call(mcp(), "peer_server_enable", params(ENABLE));
    assert!(matches!(r, Err(ToolError::Refused(_))), "{r:?}");
    while let Ok(ev) = events.recv_timeout(Duration::from_millis(100)) {
        assert!(!matches!(ev, Event::ConsentRequested { .. }), "no card for a refused call");
    }
    assert!(rec.calls().is_empty());
}

#[test]
fn the_ui_reaches_the_host() {
    let (h, rec) = rig();
    let out = h.ui("peer_server_enable", params(ENABLE)).unwrap();
    assert_eq!(out.data["listening"], "10.8.0.2:47300");
    h.ui("peer_pair_start", json!({})).unwrap();
    h.ui("peer_pair_answer", json!({"allow": false})).unwrap();
    assert_eq!(rec.calls(), ["enable 10.8.0.2:47300 by ui", "pair_start by ui", "pair_answer false by ui"]);
}

#[test]
fn only_the_full_level_exists_so_far() {
    let (h, _) = rig();
    let r = h.ui("peer_pair_answer", json!({"allow": true, "check_code": "1", "level": "operator"}));
    assert!(matches!(r, Err(ToolError::InvalidParams(_))), "{r:?}");
}

#[test]
fn turning_things_off_is_allowed_for_everyone() {
    let (h, rec) = rig();
    for caller in [Caller::Ui, Caller::Cli, mcp(), peer(Level::Full)] {
        h.engine.call(caller.clone(), "peer_server_disable", json!({})).unwrap_or_else(|e| panic!("disable for {caller:?}: {e:?}"));
        h.engine.call(caller.clone(), "peer_device_revoke", json!({"all": true})).unwrap_or_else(|e| panic!("revoke for {caller:?}: {e:?}"));
        h.engine.call(caller, "peer_device_revoke", json!({"id": "someid"})).unwrap();
    }
    assert_eq!(rec.calls().len(), 12);
    let r = h.ui("peer_device_revoke", json!({}));
    assert!(matches!(r, Err(ToolError::InvalidParams(_))));
    let r = h.ui("peer_device_revoke", json!({"id": "a", "all": true}));
    assert!(matches!(r, Err(ToolError::InvalidParams(_))));
}

#[test]
fn reads_need_no_consent_and_show_no_secret() {
    let (h, _) = rig();
    let s = h.engine.call(mcp(), "peer_status", json!({})).unwrap();
    assert_eq!(s.data["available"], true);
    for key in ["link", "psk", "secret", "csm://", "\"key\""] {
        assert!(!s.data.to_string().contains(key), "status must not carry `{key}`");
    }
    assert!(h.engine.call(mcp(), "peer_device_list", json!({})).is_ok());
}

#[test]
fn the_address_is_checked_before_the_host_hears_of_it() {
    let (h, rec) = rig();
    for bad in [
        json!({"address": "0.0.0.0"}),
        json!({"address": "::"}),
        json!({"address": "[::]", "port": 5}),
        json!({"address": "224.0.0.1"}),
        json!({"address": "255.255.255.255"}),
    ] {
        let r = h.ui("peer_server_enable", bad.clone());
        assert!(matches!(r, Err(ToolError::Refused(_))), "{bad}: {r:?}");
    }
    for bad in [json!({"address": "office.example"}), json!({"address": ""}), json!({"address": "1.2.3"}), json!({"address": "10.0.0.1", "port": 70000})] {
        assert!(h.ui("peer_server_enable", bad.clone()).is_err(), "{bad}");
    }
    assert!(h.ui("peer_server_enable", json!({"address": "10.0.0.1", "unknown": 1})).is_err());
    assert!(rec.calls().is_empty());
    h.ui("peer_server_enable", json!({"address": "0.0.0.0", "port": 4000, "all_interfaces": true})).unwrap();
    h.ui("peer_server_enable", json!({"address": "10.0.0.1"})).unwrap();
    assert_eq!(rec.calls(), ["enable 0.0.0.0:4000 by ui", "enable 10.0.0.1:47300 by ui"], "the default port is 47300");
}

#[test]
fn none_of_it_is_a_setting() {
    let (h, rec) = rig();
    for key in ["PeerServerEnabled", "peer_server_enabled", "RemoteAccess", "PeerServer", "PeerListenAddress", "PeerPort"] {
        for caller in [Caller::Ui, Caller::Cli, mcp()] {
            let r = h.engine.call(caller, "settings_set", json!({"key": key, "value": true}));
            assert!(matches!(r, Err(ToolError::NotFound(_))), "{key}: {r:?}");
        }
    }
    let all = h.ui("settings_get", json!({})).unwrap().data.to_string().to_lowercase();
    assert!(!all.contains("peer") && !all.contains("remoteaccess"), "settings carry no remote-access key");
    assert!(rec.calls().is_empty());
}

#[test]
fn a_peer_call_that_changes_something_is_logged_and_a_read_is_not() {
    let (h, rec) = rig();
    h.engine.call(peer(Level::Full), "session_list", json!({})).unwrap();
    assert!(rec.audit().iter().all(|e| !matches!(e, AuditEvent::PeerCall { .. })), "reads are not logged");
    h.engine.call(peer(Level::Full), "group_new", json!({"name": "From the laptop"})).unwrap();
    let calls: Vec<_> = rec.audit().into_iter().filter(|e| matches!(e, AuditEvent::PeerCall { .. })).collect();
    assert_eq!(calls.len(), 1);
    match &calls[0] {
        AuditEvent::PeerCall { device_name, tool, .. } => {
            assert_eq!(device_name, "Laptop");
            assert_eq!(tool, "group_new");
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_device_below_full_may_only_read() {
    let (h, _) = rig();
    assert!(h.engine.call(peer(Level::Viewer), "session_list", json!({})).is_ok());
    let r = h.engine.call(peer(Level::Viewer), "group_new", json!({"name": "x"}));
    assert!(matches!(r, Err(ToolError::Refused(m)) if m.contains("Full")), "refused for a viewer");
    assert!(h.engine.call(peer(Level::Full), "group_new", json!({"name": "x"})).is_ok());
}

fn ask_consent(h: &H) -> (u64, std::sync::mpsc::Receiver<Result<csm_engine::ToolOutput, ToolError>>) {
    let events = h.engine.subscribe();
    let path = h.dir.path().join("consent.csm").to_string_lossy().into_owned();
    let rx = h.engine.submit(mcp(), "state_export", json!({"path": path}));
    loop {
        match events.recv_timeout(Duration::from_secs(5)).expect("a consent card") {
            Event::ConsentRequested { request } => return (request["request_id"].as_u64().unwrap(), rx),
            _ => continue,
        }
    }
}

#[test]
fn a_full_device_answers_a_consent_card_and_the_first_answer_wins() {
    let (h, rec) = rig();
    let (id, rx) = ask_consent(&h);
    // A viewer cannot.
    let r = h.engine.call(peer(Level::Viewer), "consent_answer", json!({"request_id": id, "answer": "allow_once"}));
    assert!(matches!(r, Err(ToolError::Refused(_))), "{r:?}");
    assert!(rx.try_recv().is_err(), "still waiting");
    // A Full device can.
    h.engine.call(peer(Level::Full), "consent_answer", json!({"request_id": id, "answer": "allow_once"})).unwrap();
    assert!(rx.recv_timeout(Duration::from_secs(5)).unwrap().is_ok(), "the agent's call ran");
    // The app answering second finds the card gone.
    let late = h.ui("consent_answer", json!({"request_id": id, "answer": "deny"}));
    assert!(matches!(late, Err(ToolError::NotFound(_))), "{late:?}");
    assert!(rec.audit().iter().any(|e| matches!(e, AuditEvent::ConsentAnswered { allow: true, .. })));
}

#[test]
fn consent_answered_in_the_app_first_leaves_the_device_a_no_such_card() {
    let (h, _) = rig();
    let (id, rx) = ask_consent(&h);
    h.ui("consent_answer", json!({"request_id": id, "answer": "deny"})).unwrap();
    assert!(matches!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), Err(ToolError::Declined(_))));
    let late = h.engine.call(peer(Level::Full), "consent_answer", json!({"request_id": id, "answer": "allow_once"}));
    assert!(matches!(late, Err(ToolError::NotFound(_))), "{late:?}");
}

#[test]
fn nobody_else_can_answer_a_consent_card() {
    let (h, _) = rig();
    let (id, rx) = ask_consent(&h);
    for c in [Caller::Cli, mcp()] {
        let r = h.engine.call(c, "consent_answer", json!({"request_id": id, "answer": "allow_once"}));
        assert!(matches!(r, Err(ToolError::Refused(_))), "{r:?}");
    }
    assert!(rx.try_recv().is_err());
    h.ui("consent_answer", json!({"request_id": id, "answer": "deny"})).unwrap();
}

#[test]
fn a_run_without_a_host_has_no_remote_access_at_all() {
    let h = H::new();
    let s = h.ui("peer_status", json!({})).unwrap();
    assert_eq!(s.data["available"], false);
    assert!(matches!(h.ui("peer_server_enable", params(ENABLE)), Err(ToolError::Failed(_))));
    assert!(matches!(h.ui("peer_pair_start", json!({})), Err(ToolError::Failed(_))));
}

#[test]
fn a_peer_caller_is_treated_like_an_agent_for_the_rules() {
    // The IPC bridge deserializes a caller from the wire; `accept_caller` (csm-mcp) refuses this
    // shape. The engine side of the same promise: a Peer caller is only ever constructed in-process.
    let c: Caller = serde_json::from_value(json!({"kind": "peer", "device_id": "x", "name": "n", "level": "full"})).unwrap();
    assert!(c.is_external(), "a peer is treated like an agent for the rules that keep decisions with the person");
    assert_eq!(c.describe(), "peer:n (x)");
}
