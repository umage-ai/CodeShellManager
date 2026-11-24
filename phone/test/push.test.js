// The Web Push key: fetched from the relay's /v1/vapid over https, and refused unless it is the
// 65-byte uncompressed P-256 point the browser needs. The relay's address is only ever turned into an
// https address for that one request.

import assert from "node:assert/strict";
import { test } from "node:test";
import { b64urlEncode } from "../js/bytes.js";
import { fetchVapidKey, httpBaseOf } from "../js/push.js";

/**
 * A fetch that answers with `body`, and records what it was asked.
 * @param {unknown} body
 * @param {{ ok?: boolean }} [o]
 */
function fakeFetch(body, o = {}) {
  /** @type {{ url: string, init: { redirect?: string } | undefined }} */
  const seen = { url: "", init: undefined };
  const f = async (/** @type {string} */ url, /** @type {{ redirect?: string } | undefined} */ init) => {
    seen.url = url;
    seen.init = init;
    return { ok: o.ok ?? true, json: async () => body };
  };
  return { fetch: /** @type {typeof fetch} */ (/** @type {unknown} */ (f)), seen };
}

test("the relay's push key is requested over https from the same host", async () => {
  const key = new Uint8Array(65);
  key[0] = 4;
  const f = fakeFetch({ public_key: b64urlEncode(key) });
  const got = await fetchVapidKey("wss://relay.example.test", f.fetch);
  assert.deepEqual(got, key);
  assert.equal(f.seen.url, "https://relay.example.test/v1/vapid");
  assert.equal(f.seen.init?.redirect, "error", "a redirect is not followed");
});

test("a key that is not a 65-byte uncompressed point is refused", async () => {
  for (const bad of [new Uint8Array(32), (() => { const k = new Uint8Array(65); k[0] = 3; return k; })()]) {
    await assert.rejects(fetchVapidKey("wss://relay.example.test", fakeFetch({ public_key: b64urlEncode(bad) }).fetch));
  }
  await assert.rejects(fetchVapidKey("wss://relay.example.test", fakeFetch({ public_key: "not base64 !" }).fetch));
  await assert.rejects(fetchVapidKey("wss://relay.example.test", fakeFetch({}).fetch));
});

test("a relay with push off (404) gives no key", async () => {
  await assert.rejects(fetchVapidKey("wss://relay.example.test", fakeFetch({}, { ok: false }).fetch));
});

test("only a wss relay has a push key to fetch; a loopback relay does not", () => {
  assert.equal(httpBaseOf("wss://relay.example.test"), "https://relay.example.test");
  assert.throws(() => httpBaseOf("ws://127.0.0.1:8787"));
});
