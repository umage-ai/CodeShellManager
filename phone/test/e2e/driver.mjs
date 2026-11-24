// The phone side of the end-to-end test (design section 11, step 3). The Rust test
// (apps/csm-phone-it/tests/e2e.rs) starts a real csm-relay and the add-on's host on 127.0.0.1, then
// runs this file with Node and drives it: one JSON command per line on stdin, one JSON reply per
// line on stdout. Everything the phone does goes through the app's own modules (`js/pairing.js`,
// `js/client.js`, `js/crypto.js`). There is no service worker in Node, so the push subscription is
// made with the same `push.subscribe` op the page sends, with a fake endpoint.
//
// Nothing here talks to a hosted service. The relay is given in RELAY and must be a loopback ws://
// address; the page origin is the dev origin the app accepts for it.
//
// Diagnostics go to stderr. stdout carries only replies, so the Rust side can read them as lines.

import { createInterface } from "node:readline";
import { randomBytes, webcrypto } from "node:crypto";
import { b64urlEncode } from "../../js/bytes.js";
import { decodeOffer, startPairing } from "../../js/pairing.js";
import { generateKeypair } from "../../js/crypto.js";
import { HostClient } from "../../js/client.js";

const PAGE_ORIGIN = process.env.PAGE_ORIGIN || "http://127.0.0.1:8080";
const RELAY = process.env.RELAY || "";
const WAIT_MS = 15_000;

/** @type {Map<string, { device: any, identity: any, client: any | null }>} */
const phones = new Map();

/** @param {unknown} v */
function reply(v) {
  process.stdout.write(`${JSON.stringify(v)}\n`);
}

/** @param {string} msg */
function note(msg) {
  process.stderr.write(`[driver] ${msg}\n`);
}

/**
 * @param {any} e
 * @returns {{ ok: false, code: string, message: string }}
 */
function failure(e) {
  const code = typeof e?.code === "string" ? e.code : "bad_request";
  const message = typeof e?.message === "string" ? e.message : String(e);
  return { ok: false, code, message };
}

/** @param {string} name */
function phoneNamed(name) {
  const p = phones.get(name);
  if (!p) throw new Error(`no phone named ${name}`);
  return p;
}

/** @param {any} client */
function sessionsOf(client) {
  return client.list.list().map((/** @type {any} */ s) => ({ id: s.id, name: s.name, status: s.status }));
}

/**
 * Pairs a phone. The PC's decision comes from the Rust side while this waits for the answer.
 * @param {{ name: string, offer: string }} cmd
 */
async function pair(cmd) {
  const offer = decodeOffer(cmd.offer);
  if (!offer) return { ok: false, code: "bad_request", message: "the offer does not decode" };
  const identity = await generateKeypair();
  const session = await startPairing(offer, identity, { pageOrigin: PAGE_ORIGIN, WebSocket: globalThis.WebSocket });
  note(`check code ${session.check}`);
  const got = await session.confirm(cmd.name);
  phones.set(cmd.name, {
    identity,
    client: null,
    device: { host_id: got.hostId, host_pub: got.hostPub, routing_token: got.routingToken, relay: got.relay, device_id: got.deviceId, level: got.level },
  });
  return { ok: true, level: got.level, device_id: got.deviceId, check: session.check };
}

/**
 * Resolves when `ready()` holds, re-checking on every channel event.
 * @param {any} client
 * @param {() => boolean} ready
 * @param {number} ms
 * @returns {Promise<void>}
 */
function waitFor(client, ready, ms) {
  if (ready()) return Promise.resolve();
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      off();
      reject(new Error("timed out"));
    }, ms);
    const off = client.onEvent(() => {
      if (!ready()) return;
      clearTimeout(timer);
      off();
      resolve();
    });
  });
}

/** @param {{ name: string }} cmd */
async function connect(cmd) {
  const p = phoneNamed(cmd.name);
  if (p.client) p.client.close();
  const client = new HostClient({ device: p.device, identity: p.identity, pageOrigin: PAGE_ORIGIN, WebSocket: globalThis.WebSocket });
  p.client = client;
  const hello = await client.connect();
  // The snapshot follows the hello; wait for it so the reply shows the list.
  await waitFor(client, () => client.list.seq !== null, WAIT_MS);
  return { ok: true, host_name: hello.host_name, level: hello.level, sessions: sessionsOf(client) };
}

/** @param {{ name: string, op: string, args?: Record<string, unknown> }} cmd */
async function request(cmd) {
  const client = phoneNamed(cmd.name).client;
  if (!client) return { ok: false, code: "unavailable", message: "not connected" };
  try {
    const data = await client.request(cmd.op, cmd.args ?? {});
    return { ok: true, data };
  } catch (e) {
    return failure(e);
  }
}

/** @param {{ name: string, id: string, status: string, timeout_ms?: number }} cmd */
async function waitSession(cmd) {
  const client = phoneNamed(cmd.name).client;
  if (!client) return { ok: false, code: "unavailable", message: "not connected" };
  const seen = () => client.list.list().some((/** @type {any} */ s) => s.id === cmd.id && s.status === cmd.status);
  try {
    await waitFor(client, seen, cmd.timeout_ms ?? WAIT_MS);
    return { ok: true, sessions: sessionsOf(client) };
  } catch {
    return { ok: false, code: "timeout", message: "the session did not reach that status", sessions: sessionsOf(client) };
  }
}

/** @param {{ name: string, timeout_ms?: number }} cmd */
async function expectBye(cmd) {
  const client = phoneNamed(cmd.name).client;
  if (!client) return { ok: false, code: "unavailable", message: "not connected" };
  return new Promise((resolve) => {
    const timer = setTimeout(() => {
      off();
      resolve({ ok: false, code: "timeout", message: "no bye" });
    }, cmd.timeout_ms ?? WAIT_MS);
    const off = client.onEvent((/** @type {any} */ e) => {
      let reason = null;
      if (e.kind === "event" && e.msg.ev === "bye") reason = e.msg.reason;
      else if (e.kind === "closed") reason = "closed";
      if (reason === null) return;
      clearTimeout(timer);
      off();
      resolve({ ok: true, reason });
    });
  });
}

/**
 * Subscribes this phone to Web Push with a fake endpoint: no push service is contacted. The keys are
 * a real P-256 point and 16 random bytes, the shapes a browser gives.
 * @param {{ name: string, endpoint: string }} cmd
 */
async function pushSubscribe(cmd) {
  const key = await webcrypto.subtle.generateKey({ name: "ECDH", namedCurve: "P-256" }, true, ["deriveBits"]);
  const p256dh = b64urlEncode(new Uint8Array(await webcrypto.subtle.exportKey("raw", key.publicKey)));
  const auth = b64urlEncode(randomBytes(16));
  return request({ name: cmd.name, op: "push.subscribe", args: { endpoint: cmd.endpoint, p256dh, auth } });
}

/** @param {any} cmd */
async function run(cmd) {
  switch (cmd.do) {
    case "pair":
      return pair(cmd);
    case "connect":
      return connect(cmd);
    case "request":
      return request(cmd);
    case "wait_session":
      return waitSession(cmd);
    case "expect_bye":
      return expectBye(cmd);
    case "push_subscribe":
      return pushSubscribe(cmd);
    case "quit":
      return { ok: true };
    default:
      return { ok: false, code: "bad_request", message: `unknown command ${String(cmd.do)}` };
  }
}

if (!RELAY) {
  note("RELAY is not set");
  process.exit(2);
}

const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
// One command at a time: each reply is written before the next command is read.
let chain = Promise.resolve();
lines.on("line", (line) => {
  chain = chain.then(async () => {
    let cmd;
    try {
      cmd = JSON.parse(line);
    } catch {
      reply({ ok: false, code: "bad_request", message: "not JSON" });
      return;
    }
    try {
      reply(await run(cmd));
    } catch (e) {
      reply(failure(e));
    }
    if (cmd?.do === "quit") {
      for (const p of phones.values()) p.client?.close();
      process.exit(0);
    }
  });
});
lines.on("close", () => {
  void chain.then(() => process.exit(0));
});
