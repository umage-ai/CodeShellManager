//! The demo engine's snapshot converts to the owned peer wire types and reads back unchanged.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use csm_peer_proto::Snapshot as WireSnapshot;

#[test]
fn the_demo_engine_snapshot_converts_and_reads_back() {
    let engine = csm_ui::demo_engine().expect("the demo engine starts");
    let snap = engine.view();

    let wire = WireSnapshot::from(&*snap);

    assert_eq!(wire.version, snap.version);
    assert_eq!(wire.sessions.len(), snap.sessions.len());
    assert_eq!(wire.groups.len(), snap.groups.len());
    assert_eq!(wire.inbox, snap.inbox);
    assert_eq!(wire.layout_key, snap.layout_key);
    assert_eq!(wire.open_with.len(), snap.open_with.len());

    let text = serde_json::to_string(&wire).unwrap();
    let back: WireSnapshot = serde_json::from_str(&text).unwrap();
    assert_eq!(back, wire);
}
