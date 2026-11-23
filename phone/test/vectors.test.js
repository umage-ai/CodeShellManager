// csm-remote-proto's shared vectors, read by the phone's own code. The fixtures are copies of
// crates/csm-remote-proto/vectors/*.json; the Rust test `tests/vectors.rs` fails when a copy differs.

import assert from "node:assert/strict";
import { test } from "node:test";
import { b64urlDecode, hex, unhex, utf8 } from "../js/bytes.js";
import { decodeOffer, checkCode, confirmMessage, helloMessage, prologueFor } from "../js/pairing.js";
import {
  encodeRequest,
  ERROR_CODES,
  MAX_APP_MESSAGE_BYTES,
  OP_NAMES,
  parseFolder,
  parseHelloData,
  parseInboxItem,
  parsePairReply,
  parseProfile,
  parseCommand,
  parseServerMsg,
  refusalFromCloseCode,
  REFUSAL_NAMES,
  requiredLevel,
  STATUSES,
  groupOf,
} from "../js/protocol.js";
import { helloForPairing, helloForToken } from "../js/relay.js";
import { vector } from "./helpers.js";

const ids = vector("ids");
const codes = vector("codes");
const messages = vector("messages");
const pairing = vector("pairing");
const frames = vector("frames");

test("the fixtures are present and carry the shapes the phone reads", () => {
  assert.equal(messages.max_message_bytes, MAX_APP_MESSAGE_BYTES);
  assert.ok(Array.isArray(messages.events) && messages.events.length >= 5);
  assert.ok(Array.isArray(frames.frames) && frames.frames.length > 0);
});

test("the op names are the protocol's, in the vector's order", () => {
  assert.deepEqual(OP_NAMES, messages.ops);
});

test("each valid request's level is the level the phone assumes", () => {
  for (const r of messages.valid_requests) {
    assert.equal(requiredLevel(r.op), r.level, `${r.op}`);
  }
});

test("a valid request encodes to the JSON the vector gives (an empty args is left out)", () => {
  for (const r of messages.valid_requests) {
    const given = JSON.parse(r.json);
    if (given.args && Object.keys(given.args).length === 0) delete given.args;
    const parsed = JSON.parse(r.json);
    const text = encodeRequest(parsed.id, parsed.op, parsed.args ?? {});
    assert.notEqual(text, null);
    assert.deepEqual(JSON.parse(/** @type {string} */ (text)), given, r.op);
  }
});

test("refusal codes: names and close numbers agree with codes.json", () => {
  assert.deepEqual([...REFUSAL_NAMES], codes.codes.map((/** @type {any} */ c) => c.code));
  for (const c of codes.codes) {
    assert.equal(refusalFromCloseCode(c.ws_close_code), c.code);
  }
  assert.equal(refusalFromCloseCode(4999), null);
  assert.equal(refusalFromCloseCode(3999), null);
});

test("error codes match msg.rs", () => {
  assert.deepEqual([...ERROR_CODES], messages.error_codes);
});

test("every server message in the vectors parses to a response or an event", () => {
  for (const e of messages.events) {
    const parsed = parseServerMsg(utf8(JSON.stringify(e)));
    assert.ok(parsed, `event ${e.ev} must parse`);
    assert.equal(parsed.kind, "event");
  }
  for (const r of messages.responses) {
    const parsed = parseServerMsg(utf8(JSON.stringify(r)));
    assert.ok(parsed, `response ${r.id} must parse`);
    assert.equal(parsed.kind, "response");
    assert.equal(parsed.id, r.id);
  }
});

test("a delta and a snapshot keep their sequence numbers", () => {
  const snap = messages.events.find((/** @type {any} */ e) => e.ev === "snapshot");
  const delta = messages.events.find((/** @type {any} */ e) => e.ev === "delta");
  const a = parseServerMsg(utf8(JSON.stringify(snap)));
  const b = parseServerMsg(utf8(JSON.stringify(delta)));
  assert.equal(a?.kind === "event" && a.ev === "snapshot" ? a.seq : -1, snap.seq);
  assert.equal(b?.kind === "event" && b.ev === "delta" ? b.seq : -1, delta.seq);
});

test("the statuses map to the groups the phone lists them under", () => {
  assert.deepEqual(new Set(STATUSES), new Set(messages.statuses.map((/** @type {any} */ s) => s.status)));
  for (const s of messages.statuses) {
    assert.equal(groupOf(s.status) === "needs", s.needs_you, s.status);
  }
});

test("hello data parses to the vector's fields", () => {
  const h = parseHelloData(messages.hello_data);
  assert.deepEqual(h, messages.hello_data);
});

test("a folder, a profile, a command and an inbox item parse as the add-on sends them", () => {
  assert.deepEqual(parseFolder(messages.data.folder), messages.data.folder);
  assert.deepEqual(parseProfile(messages.data.profile), messages.data.profile);
  assert.deepEqual(parseCommand(messages.data.command), messages.data.command);
  assert.deepEqual(parseInboxItem(messages.data.inbox_item), {
    id: messages.data.inbox_item.id,
    title: messages.data.inbox_item.title,
    session_id: messages.data.inbox_item.session_id,
  });
});

test("a request over the app cap is not sent", () => {
  assert.equal(encodeRequest(1, "session.wake", { session_id: "x".repeat(MAX_APP_MESSAGE_BYTES) }), null);
});

test("the host's refusals of a bad request parse as an answer with its id", () => {
  for (const bad of messages.invalid_requests) {
    if (bad.id === null) continue;
    const parsed = parseServerMsg(utf8(JSON.stringify({ id: bad.id, ok: false, error: { code: bad.code, message: "" } })));
    assert.equal(parsed?.kind, "response");
  }
});

test("the pairing offer decodes from the vector's payload, link and bare forms", () => {
  const offer = pairing.offer;
  assert.deepEqual(decodeOffer(pairing.encoded), offer);
  assert.deepEqual(decodeOffer(pairing.url), offer);
  assert.deepEqual(decodeOffer(`p=${pairing.encoded}`), offer);
  assert.deepEqual(decodeOffer(`#p=${pairing.encoded}`), offer);
});

test("every invalid offer in the vector is refused", () => {
  for (const bad of pairing.invalid) {
    assert.equal(decodeOffer(bad.input), null, bad.error);
  }
});

test("the offer's JSON is the vector's, field for field", () => {
  const text = new TextDecoder().decode(b64urlDecode(pairing.encoded) ?? new Uint8Array());
  assert.equal(text, pairing.offer_json);
});

test("the check code of the vector's handshake hash", async () => {
  for (const c of ids.check_codes) {
    assert.equal(await checkCode(unhex(c.handshake_hash_hex)), c.check_code);
  }
});

test("the prologue is the vector's bytes", () => {
  assert.equal(hex(prologueFor(ids.prologue.host_id)), ids.prologue.prologue_hex);
});

test("the pairing messages the phone sends are the vector's", () => {
  assert.deepEqual(confirmMessage(pairing.messages.confirm.check), pairing.messages.confirm);
  assert.deepEqual(helloMessage(pairing.messages.hello.name), pairing.messages.hello);
});

test("the pairing replies the host sends parse", () => {
  const accept = parsePairReply(utf8(JSON.stringify(pairing.messages.accept)));
  assert.deepEqual(accept, pairing.messages.accept);
  assert.deepEqual(parsePairReply(utf8(JSON.stringify(pairing.messages.deny))), pairing.messages.deny);
});

test("the channel hellos are the vector's JSON", () => {
  const control = frames.control;
  assert.deepEqual(helloForToken(control.client_hello_token.host_id, control.client_hello_token.token), control.client_hello_token);
  assert.deepEqual(helloForPairing(control.client_hello_pairing.host_id, control.client_hello_pairing.pairing_id), control.client_hello_pairing);
});

test("the routing token and its hash in the vector decode to 32 bytes", () => {
  for (const t of ids.tokens) {
    assert.equal(hex(b64urlDecode(t.routing_token_b64) ?? new Uint8Array()), t.routing_token_hex);
  }
});
