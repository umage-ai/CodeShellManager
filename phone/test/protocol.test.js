// The phone's message handling beyond the vectors: what is refused, what is clean, and the plain
// messages. A hostile PC or relay can send anything, so every shape is checked and a bad one is
// dropped, never trusted.

import assert from "node:assert/strict";
import { test } from "node:test";
import { utf8 } from "../js/bytes.js";
import {
  cleanText,
  encodeRequest,
  groupOf,
  MESSAGES,
  messageFor,
  parseList,
  parsePairReply,
  parseServerMsg,
  parseSession,
  refusalFromCloseCode,
} from "../js/protocol.js";

/** @param {unknown} v */
const raw = (v) => utf8(typeof v === "string" ? v : JSON.stringify(v));

test("a message that is not JSON, not an object, or not a known shape is dropped", () => {
  for (const bad of ["not json", "[]", "null", "42", JSON.stringify({ ev: "nope" }), JSON.stringify({ hello: 1 })]) {
    assert.equal(parseServerMsg(utf8(bad)), null, bad);
  }
});

test("invalid UTF-8 is refused, not read as replacement characters", () => {
  assert.equal(parseServerMsg(new Uint8Array([0x7b, 0xff, 0x7d])), null);
});

test("a message over the 48 KiB app cap is refused before it is parsed", () => {
  const big = raw({ id: 1, ok: true, data: { x: "a".repeat(49152) } });
  assert.equal(parseServerMsg(big), null);
});

test("a response needs a numeric id and either data or an error with a code", () => {
  assert.equal(parseServerMsg(raw({ id: "1", ok: true, data: {} })), null);
  assert.equal(parseServerMsg(raw({ id: 1, ok: false })), null);
  assert.equal(parseServerMsg(raw({ id: 1, ok: false, error: {} })), null);
  assert.deepEqual(parseServerMsg(raw({ id: 1, ok: true })), { kind: "response", id: 1, ok: true, data: null });
});

test("a session with a status the phone does not know is refused as a whole message", () => {
  const msg = raw({ ev: "snapshot", seq: 1, part: 1, parts: 1, sessions: [{ id: "a", name: "a", folder: "/", status: "reading_the_terminal" }] });
  assert.equal(parseServerMsg(msg), null);
});

test("a session with a bad field type is refused, and an empty id is refused", () => {
  assert.equal(parseSession({ id: "", name: "a", folder: "/", status: "idle" }), null);
  assert.equal(parseSession({ id: "a", name: 1, folder: "/", status: "idle" }), null);
  assert.equal(parseSession({ id: "a", name: "a", folder: "/", status: "idle", since: -1 }), null);
  assert.equal(parseSession({ id: "a", name: "a", folder: "/", status: "idle", since: 1.5 }), null);
  assert.equal(parseSession({ id: "a\u0000", name: "a", folder: "/", status: "idle" }), null);
});

test("the optional fields are kept when they are the right type and absent otherwise", () => {
  const s = parseSession({ id: "a", name: "a", folder: "/", status: "idle", branch: "main", message: "hi", since: 1760000000 });
  assert.deepEqual(s, { id: "a", name: "a", folder: "/", status: "idle", branch: "main", message: "hi", since: 1760000000 });
  assert.deepEqual(parseSession({ id: "a", name: "a", folder: "/", status: "idle" }), { id: "a", name: "a", folder: "/", status: "idle" });
});

test("a delta with an unclean removed id is refused", () => {
  assert.equal(parseServerMsg(raw({ ev: "delta", seq: 2, upserts: [], removed: ["a\nb"] })), null);
});

test("a delta with a bad session inside is refused, not partly applied", () => {
  const bad = { ev: "delta", seq: 2, upserts: [{ id: "ok", name: "a", folder: "/", status: "idle" }, { id: "x" }], removed: [] };
  assert.equal(parseServerMsg(raw(bad)), null);
});

test("a counter outside the safe range is refused", () => {
  assert.equal(parseServerMsg(raw({ ev: "delta", seq: 2 ** 60, upserts: [], removed: [] })), null);
  assert.equal(parseServerMsg(raw({ ev: "inbox", seq: 1, unread: -1 })), null);
  assert.equal(parseServerMsg(raw({ ev: "snapshot", seq: 1, part: 0, parts: 1, sessions: [] })), null);
  assert.equal(parseServerMsg(raw({ ev: "snapshot", seq: 1, part: 2, parts: 1, sessions: [] })), null);
});

test("a remote_control failure carries its code and message; a success carries neither", () => {
  assert.deepEqual(parseServerMsg(raw({ ev: "remote_control", session: "s1", ok: true })), { kind: "event", ev: "remote_control", session: "s1", ok: true });
  assert.deepEqual(parseServerMsg(raw({ ev: "remote_control", session: "s1", ok: false, code: "busy", message: "busy" })), {
    kind: "event",
    ev: "remote_control",
    session: "s1",
    ok: false,
    code: "busy",
    message: "busy",
  });
  assert.equal(parseServerMsg(raw({ ev: "remote_control", session: "s1", ok: false })), null, "a failure needs its code");
});

test("bye needs one of the three reasons", () => {
  assert.deepEqual(parseServerMsg(raw({ ev: "bye", reason: "level_changed" })), { kind: "event", ev: "bye", reason: "level_changed" });
  assert.equal(parseServerMsg(raw({ ev: "bye", reason: "whatever" })), null);
});

test("encodeRequest leaves out an empty args and refuses an oversized request", () => {
  assert.equal(encodeRequest(1, "ping"), '{"id":1,"op":"ping"}');
  assert.equal(encodeRequest(2, "ping", {}), '{"id":2,"op":"ping"}');
  assert.equal(encodeRequest(3, "session.wake", { session_id: "a".repeat(60000) }), null);
});

test("the plain message for every code, and the pairing wording for not_allowed", () => {
  assert.equal(messageFor("busy"), MESSAGES.busy);
  assert.equal(messageFor("no_such_code"), "Something went wrong. Try again.");
  assert.match(messageFor("not_allowed", { pairing: true }), /Pairing is in use by another phone/);
  assert.match(messageFor("not_allowed"), /refused the connection/);
});

test("refusal close codes map to names; anything else is not a refusal", () => {
  assert.equal(refusalFromCloseCode(4010), "revoked");
  assert.equal(refusalFromCloseCode(4000), null);
  assert.equal(refusalFromCloseCode(1000), null);
  assert.equal(refusalFromCloseCode(4014), null);
});

test("groups: needs first, then working, then idle or done, asleep, exited", () => {
  assert.equal(groupOf("needs_approval"), "needs");
  assert.equal(groupOf("needs_question"), "needs");
  assert.equal(groupOf("working"), "working");
  assert.equal(groupOf("done"), "idle");
  assert.equal(groupOf("idle"), "idle");
  assert.equal(groupOf("asleep"), "asleep");
  assert.equal(groupOf("exited"), "exited");
});

test("parseList drops the entries that do not match, and refuses a missing array", () => {
  /** @param {unknown} v */
  const folder = (v) => (typeof v === "object" && v !== null && "path" in v ? { path: "p" } : null);
  assert.deepEqual(parseList({ folders: [{ path: "x" }, 3, null] }, "folders", folder), [{ path: "p" }]);
  assert.equal(parseList({}, "folders", folder), null);
  assert.equal(parseList(null, "folders", folder), null);
});

test("a pairing reply is accept or deny with its fields, and nothing else", () => {
  assert.equal(parsePairReply(raw({ t: "accept", routing_token: "x", level: "owner", device_id: "d" })), null);
  assert.equal(parsePairReply(raw({ t: "deny", reason: "maybe" })), null);
  assert.equal(parsePairReply(raw({ t: "deny", reason: "expired" }))?.t, "deny");
});

test("cleanText refuses controls and empty or long strings", () => {
  assert.equal(cleanText("abc", 3), true);
  assert.equal(cleanText("abcd", 3), false);
  assert.equal(cleanText("", 3), false);
  assert.equal(cleanText("a\tb", 3), false);
  assert.equal(cleanText(5, 3), false);
});
