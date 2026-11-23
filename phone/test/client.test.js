// The phone's session channel against a fake PC: the IK handshake, the hello, requests and their
// answers in order, events feeding the list, a gap asking for a snapshot, a revoke, and a message the
// phone cannot decrypt ending the channel.

import assert from "node:assert/strict";
import { test } from "node:test";
import { generateKeypair, keypairFromSeed } from "../js/crypto.js";
import { ClientError, HostClient } from "../js/client.js";
import { RelayError } from "../js/relay.js";
import { fakeWebSocketClass, sessionHost } from "./fakes.js";
import { vector } from "./helpers.js";
import { unhex } from "../js/bytes.js";

const HOST_ID = vector("ids").prologue.host_id;
const PAGE = "https://phone.example.test";

const HELLO = {
  protocol: 1,
  level: "operator",
  host_name: "WORKPC",
  hide_names: false,
  notify: true,
  push_ready: false,
  remote_control: true,
  max_message_bytes: 49152,
};

/**
 * Waits until `pred` holds (the PC's events arrive after its answers).
 * @param {() => boolean} pred
 */
async function until(pred) {
  for (let i = 0; i < 200 && !pred(); i++) await new Promise((r) => setTimeout(r, 5));
}

const SESSION = (/** @type {string} */ id, status = "working") => ({ id, name: `name ${id}`, folder: "C:/code/api", status });

/**
 * A device and a fake PC for it.
 * @param {(req: any, host: any) => Promise<void> | void} serve
 */
async function setup(serve) {
  const hostKeypair = await keypairFromSeed(unhex(vector("noise").session.responder_static_private_hex));
  const token = new Uint8Array(32).fill(3);
  const log = { seen: /** @type {any[]} */ ([]), phoneStatic: null };
  const WS = fakeWebSocketClass(sessionHost({ hostKeypair, hostId: HOST_ID, expectToken: token, serve, log }));
  const identity = await generateKeypair();
  const device = { host_id: HOST_ID, relay: "wss://relay.example.test", host_pub: hostKeypair.publicKey, routing_token: token, device_id: "d1" };
  return { device, identity, WS, log };
}

/**
 * The standard PC: hello, snapshot and the other ops answered from a small table.
 * @param {Record<string, (req: any, host: any) => any>} [extra]
 */
function standardPc(extra = {}) {
  return async (/** @type {any} */ req, /** @type {any} */ host) => {
    if (extra[req.op]) return extra[req.op](req, host);
    if (req.op === "hello") return host.reply({ id: req.id, ok: true, data: HELLO });
    if (req.op === "snapshot") {
      await host.reply({ id: req.id, ok: true, data: {} });
      return host.event({ ev: "snapshot", seq: 5, part: 1, parts: 1, sessions: [SESSION("s1"), SESSION("s2", "needs_approval")] });
    }
    if (req.op === "session.wake") return host.reply({ id: req.id, ok: false, error: { code: "forbidden", message: "operator level needed" } });
    return host.reply({ id: req.id, ok: true, data: {} });
  };
}

test("connect: the handshake, the hello, and the snapshot that follows", async () => {
  const { device, identity, WS, log } = await setup(standardPc());
  const client = new HostClient({ device, identity, pageOrigin: PAGE, WebSocket: WS });
  const hello = await client.connect();
  assert.deepEqual(hello, HELLO);
  assert.deepEqual(log.phoneStatic, identity.publicKey, "the PC learns the phone's static key from the handshake");
  await client.request("snapshot");
  await until(() => client.list.list().length === 2);
  assert.deepEqual(client.list.list().map((s) => s.id), ["s1", "s2"]);
  client.close();
});

test("a wrong routing token is refused by the relay before any handshake", async () => {
  const hostKeypair = await keypairFromSeed(unhex(vector("noise").session.responder_static_private_hex));
  const WS = fakeWebSocketClass(sessionHost({ hostKeypair, hostId: HOST_ID, expectToken: new Uint8Array(32).fill(8), serve: standardPc() }));
  const device = { host_id: HOST_ID, relay: "wss://relay.example.test", host_pub: hostKeypair.publicKey, routing_token: new Uint8Array(32).fill(3), device_id: "d1" };
  const client = new HostClient({ device, identity: await generateKeypair(), pageOrigin: PAGE, WebSocket: WS });
  await assert.rejects(client.connect(), (e) => e instanceof RelayError && e.code === "not_allowed");
});

test("a request the level forbids comes back as its error, with the plain message", async () => {
  const { device, identity, WS } = await setup(standardPc());
  const client = new HostClient({ device, identity, pageOrigin: PAGE, WebSocket: WS });
  await client.connect();
  await assert.rejects(client.request("session.wake", { session_id: "s1" }), (e) => e instanceof ClientError && e.code === "forbidden");
  client.close();
});

test("requests are answered by id, even when the PC answers them out of order", async () => {
  /** @type {any[]} */
  const held = [];
  const { device, identity, WS } = await setup(async (req, host) => {
    if (req.op === "hello") return host.reply({ id: req.id, ok: true, data: HELLO });
    // Hold the first request until the second has been answered, then answer the first.
    held.push({ req, host });
    if (held.length === 2) {
      await host.reply({ id: held[1].req.id, ok: true, data: { n: 2 } });
      await host.reply({ id: held[0].req.id, ok: true, data: { n: 1 } });
    }
  });
  const client = new HostClient({ device, identity, pageOrigin: PAGE, WebSocket: WS });
  await client.connect();
  const [a, b] = await Promise.all([client.request("session.get", { session_id: "s1" }), client.request("session.get", { session_id: "s2" })]);
  assert.deepEqual(a, { n: 1 });
  assert.deepEqual(b, { n: 2 });
  client.close();
});

test("several requests sent at once arrive at the PC in order, and every one decrypts", async () => {
  const { device, identity, WS, log } = await setup(standardPc());
  const client = new HostClient({ device, identity, pageOrigin: PAGE, WebSocket: WS });
  await client.connect();
  const sent = [];
  for (let i = 0; i < 12; i++) sent.push(client.request("ping"));
  await Promise.all(sent);
  const ids = log.seen.map((r) => r.id);
  assert.deepEqual(ids, [...ids].sort((x, y) => x - y), "in the order they were made");
  assert.equal(ids.length, 13, "hello plus twelve pings");
  client.close();
});

test("a gap in the seq asks the PC for a snapshot, and the list follows the snapshot", async () => {
  /** @type {string[]} */
  const ops = [];
  const { device, identity, WS } = await setup(async (req, host) => {
    ops.push(req.op);
    if (req.op === "hello") {
      await host.reply({ id: req.id, ok: true, data: HELLO });
      return host.event({ ev: "snapshot", seq: 1, part: 1, parts: 1, sessions: [SESSION("s1")] });
    }
    if (req.op === "snapshot") {
      await host.reply({ id: req.id, ok: true, data: {} });
      return host.event({ ev: "snapshot", seq: 9, part: 1, parts: 1, sessions: [SESSION("s1"), SESSION("s3")] });
    }
    // A delta with seq 4: the phone last applied 1, so 2 and 3 are missing.
    if (req.op === "ping") {
      await host.reply({ id: req.id, ok: true, data: {} });
      return host.event({ ev: "delta", seq: 4, upserts: [SESSION("s9")], removed: [] });
    }
    return host.reply({ id: req.id, ok: true, data: {} });
  });
  const client = new HostClient({ device, identity, pageOrigin: PAGE, WebSocket: WS });
  await client.connect();
  await client.request("ping");
  await new Promise((r) => setTimeout(r, 50));
  assert.ok(ops.includes("snapshot"), "the phone asked for a snapshot");
  assert.deepEqual(client.list.list().map((s) => s.id), ["s1", "s3"], "the gap delta was not applied");
  client.close();
});

test("a revoke ends the channel with the revoked reason", async () => {
  const { device, identity, WS } = await setup(async (req, host) => {
    if (req.op === "hello") {
      await host.reply({ id: req.id, ok: true, data: HELLO });
      return host.event({ ev: "bye", reason: "revoked" });
    }
    return host.reply({ id: req.id, ok: true, data: {} });
  });
  const client = new HostClient({ device, identity, pageOrigin: PAGE, WebSocket: WS });
  /** @type {Error | null} */
  let closed = null;
  client.onEvent((e) => {
    if (e.kind === "closed") closed = e.error;
  });
  await client.connect();
  await new Promise((r) => setTimeout(r, 50));
  const got = /** @type {Error | null} */ (closed);
  assert.ok(got instanceof RelayError);
  assert.equal(got.code, "revoked");
});

test("a message that does not decrypt ends the channel: the Noise state cannot be trusted after it", async () => {
  const { device, identity, WS } = await setup(async (req, host) => {
    if (req.op === "hello") {
      await host.reply({ id: req.id, ok: true, data: HELLO });
      return host.raw(new Uint8Array(20).fill(1));
    }
    return host.reply({ id: req.id, ok: true, data: {} });
  });
  const client = new HostClient({ device, identity, pageOrigin: PAGE, WebSocket: WS });
  /** @type {Error | null} */
  let closed = null;
  client.onEvent((e) => {
    if (e.kind === "closed") closed = e.error;
  });
  await client.connect();
  await new Promise((r) => setTimeout(r, 50));
  assert.ok(closed, "the channel closed");
  await assert.rejects(client.request("ping"), (e) => e instanceof ClientError);
});

test("after the channel closes, a request fails as unavailable", async () => {
  const { device, identity, WS } = await setup(standardPc());
  const client = new HostClient({ device, identity, pageOrigin: PAGE, WebSocket: WS });
  await client.connect();
  client.close();
  await assert.rejects(client.request("ping"), (e) => e instanceof ClientError && e.code === "unavailable");
});
