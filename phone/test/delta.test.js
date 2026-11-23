// The session list's rules (design 7.2): snapshots and their parts, deltas on consecutive seqs,
// split deltas, the inbox on the shared seq, and a gap or a delta before any snapshot asking for one.

import assert from "node:assert/strict";
import { test } from "node:test";
import { SessionList } from "../js/delta.js";
import { parseServerMsg } from "../js/protocol.js";
import { utf8 } from "../js/bytes.js";

/** @param {unknown} obj */
function msg(obj) {
  const parsed = parseServerMsg(utf8(JSON.stringify(obj)));
  assert.ok(parsed, `must parse: ${JSON.stringify(obj).slice(0, 80)}`);
  return parsed;
}

const S = (/** @type {string} */ id, status = "working") => ({ id, name: `n ${id}`, folder: "/w", status });

test("a one-part snapshot replaces the list and sets the seq", () => {
  const l = new SessionList();
  const r = l.apply(msg({ ev: "snapshot", seq: 4, part: 1, parts: 1, sessions: [S("a"), S("b")] }));
  assert.equal(r.changed, true);
  assert.equal(l.seq, 4);
  assert.deepEqual(l.list().map((s) => s.id), ["a", "b"]);
});

test("a snapshot in parts applies only when the last part has arrived", () => {
  const l = new SessionList();
  l.apply(msg({ ev: "snapshot", seq: 2, part: 1, parts: 3, sessions: [S("a")] }));
  assert.equal(l.list().length, 0, "nothing shown before the last part");
  l.apply(msg({ ev: "snapshot", seq: 2, part: 2, parts: 3, sessions: [S("b")] }));
  assert.equal(l.list().length, 0);
  const r = l.apply(msg({ ev: "snapshot", seq: 2, part: 3, parts: 3, sessions: [S("c")] }));
  assert.equal(r.changed, true);
  assert.deepEqual(l.list().map((s) => s.id), ["a", "b", "c"]);
});

test("a snapshot part out of order asks for a fresh snapshot and shows nothing half-read", () => {
  const l = new SessionList();
  l.apply(msg({ ev: "snapshot", seq: 2, part: 1, parts: 3, sessions: [S("a")] }));
  const r = l.apply(msg({ ev: "snapshot", seq: 2, part: 3, parts: 3, sessions: [S("c")] }));
  assert.equal(r.resync, true);
  assert.equal(l.list().length, 0);
});

test("a delta on the next seq applies: upserts, then removed", () => {
  const l = new SessionList();
  l.apply(msg({ ev: "snapshot", seq: 1, part: 1, parts: 1, sessions: [S("a"), S("b")] }));
  const r = l.apply(msg({ ev: "delta", seq: 2, upserts: [S("a", "needs_approval"), S("c")], removed: ["b"] }));
  assert.equal(r.resync, false);
  assert.equal(l.seq, 2);
  assert.deepEqual(l.list().map((s) => [s.id, s.status]), [
    ["a", "needs_approval"],
    ["c", "working"],
  ]);
});

test("a gap in the seq asks for a snapshot and applies nothing", () => {
  const l = new SessionList();
  l.apply(msg({ ev: "snapshot", seq: 1, part: 1, parts: 1, sessions: [S("a")] }));
  const r = l.apply(msg({ ev: "delta", seq: 3, upserts: [S("z")], removed: [] }));
  assert.equal(r.resync, true);
  assert.equal(r.changed, false);
  assert.equal(l.seq, 1);
  assert.deepEqual(l.list().map((s) => s.id), ["a"]);
});

test("a repeated seq is also a gap (nothing applies twice)", () => {
  const l = new SessionList();
  l.apply(msg({ ev: "snapshot", seq: 1, part: 1, parts: 1, sessions: [] }));
  l.apply(msg({ ev: "delta", seq: 2, upserts: [S("a")], removed: [] }));
  const r = l.apply(msg({ ev: "delta", seq: 2, upserts: [S("a", "idle")], removed: [] }));
  assert.equal(r.resync, true);
  assert.equal(l.list()[0].status, "working");
});

test("a delta before any snapshot asks for one", () => {
  const l = new SessionList();
  assert.equal(l.apply(msg({ ev: "delta", seq: 1, upserts: [S("a")], removed: [] })).resync, true);
});

test("split deltas with consecutive seqs each apply on their own", () => {
  const l = new SessionList();
  l.apply(msg({ ev: "snapshot", seq: 10, part: 1, parts: 1, sessions: [] }));
  for (let i = 1; i <= 3; i++) {
    const r = l.apply(msg({ ev: "delta", seq: 10 + i, upserts: [S(`x${i}`)], removed: [] }));
    assert.equal(r.resync, false, `part ${i}`);
  }
  assert.equal(l.seq, 13);
  assert.equal(l.list().length, 3);
});

test("the inbox shares the seq: a missed inbox message is a gap", () => {
  const l = new SessionList();
  l.apply(msg({ ev: "snapshot", seq: 1, part: 1, parts: 1, sessions: [] }));
  assert.equal(l.apply(msg({ ev: "inbox", seq: 3, unread: 2 })).resync, true);
  const ok = l.apply(msg({ ev: "inbox", seq: 2, unread: 2 }));
  assert.equal(ok.resync, false);
  assert.equal(ok.changed, true);
  assert.equal(l.unread, 2);
});

test("an inbox event with the same count still advances the seq but reports no change", () => {
  const l = new SessionList();
  l.apply(msg({ ev: "snapshot", seq: 1, part: 1, parts: 1, sessions: [] }));
  const same = l.apply(msg({ ev: "inbox", seq: 2, unread: 0 }));
  assert.equal(same.changed, false);
  const r = l.apply(msg({ ev: "delta", seq: 3, upserts: [], removed: [] }));
  assert.equal(r.resync, false);
});

test("a new snapshot replaces the old list and its seq, whatever the seq was", () => {
  const l = new SessionList();
  l.apply(msg({ ev: "snapshot", seq: 50, part: 1, parts: 1, sessions: [S("old")] }));
  l.apply(msg({ ev: "snapshot", seq: 1, part: 1, parts: 1, sessions: [S("new")] }));
  assert.deepEqual(l.list().map((s) => s.id), ["new"]);
  assert.equal(l.seq, 1);
});
