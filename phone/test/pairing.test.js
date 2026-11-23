// Pairing end to end through the fake relay and a fake PC (the responder, same Noise code): the
// check code on both sides, allow, deny, a wrong PSK, and a second phone while one is pending. Also
// the relay address rule, which decides where the phone may connect at all.

import assert from "node:assert/strict";
import { test } from "node:test";
import { b64urlEncode, unhex } from "../js/bytes.js";
import { generateKeypair, keypairFromSeed } from "../js/crypto.js";
import { checkCode, decodeOffer, PairingError, startPairing } from "../js/pairing.js";
import { messageFor } from "../js/protocol.js";
import { checkRelayUrl, RelayError } from "../js/relay.js";
import { fakeWebSocketClass, pairingHost } from "./fakes.js";
import { vector } from "./helpers.js";

const pairingVec = vector("pairing");
const HOST_ID = pairingVec.offer.h;
const PAGE = "https://phone.example.test";
const DEV_PAGE = "http://127.0.0.1:8080";

/**
 * A fresh offer for one test: the host's real static key from the vector, a new slot and PSK.
 * @param {object} [o]
 */
async function offerAndHost(/** @type {{ psk?: Uint8Array }} */ o = {}) {
  const hostKeypair = await keypairFromSeed(unhex(vector("noise").pairing.responder_static_private_hex));
  const psk = o.psk ?? unhex(vector("noise").pairing.psk_hex);
  const pairingId = b64urlEncode(new Uint8Array(16).fill(5));
  /** @type {import("../js/pairing.js").Offer} */
  const offer = { v: 1, r: "wss://relay.example.test", h: HOST_ID, k: b64urlEncode(hostKeypair.publicKey), i: pairingId, p: b64urlEncode(psk) };
  return { offer, hostKeypair, psk, pairingId };
}

test("the vector's offer and the one this test builds agree in shape", () => {
  assert.deepEqual(Object.keys(pairingVec.offer).sort(), ["h", "i", "k", "p", "r", "v"]);
});

test("a pairing with the phone's check code and the PC's match, then allow", async () => {
  const { offer, hostKeypair, psk, pairingId } = await offerAndHost();
  /** @type {any[]} */
  const seen = [];
  const WS = fakeWebSocketClass(
    pairingHost({
      hostKeypair,
      hostId: HOST_ID,
      psk,
      pairingIdB64: pairingId,
      decide: "allow",
      level: "viewer",
      onPending: (info) => seen.push(info),
    }),
  );
  const identity = await generateKeypair();
  const session = await startPairing(offer, identity, { pageOrigin: PAGE, WebSocket: WS });
  assert.match(session.check, /^\d{4}$/);
  const got = await session.confirm("Pixel 9");
  assert.equal(seen.length, 1);
  assert.equal(seen[0].name, "Pixel 9");
  assert.equal(seen[0].phoneCheck, session.check);
  assert.equal(seen[0].hostCheck, session.check, "both screens show the same digits");
  assert.equal(got.level, "viewer");
  assert.equal(got.deviceId, "d_test1");
  assert.equal(got.hostId, HOST_ID);
  assert.equal(got.relay, "wss://relay.example.test");
  assert.equal(got.routingToken.length, 32);
  assert.deepEqual(got.hostPub, hostKeypair.publicKey);
});

test("the check code is the vector's for the vector's handshake", async () => {
  const v = vector("noise").pairing;
  assert.equal(await checkCode(unhex(v.handshake_hash_hex)), v.check_code);
});

test("a deny is the person's answer and says so", async () => {
  const { offer, hostKeypair, psk, pairingId } = await offerAndHost();
  const WS = fakeWebSocketClass(pairingHost({ hostKeypair, hostId: HOST_ID, psk, pairingIdB64: pairingId, decide: "deny" }));
  const session = await startPairing(offer, await generateKeypair(), { pageOrigin: PAGE, WebSocket: WS });
  await assert.rejects(session.confirm("Pixel 9"), (e) => e instanceof PairingError && e.code === "denied");
});

test("a wrong PSK is never confirmed: the phone cannot read the PC's message 2", async () => {
  const { offer, hostKeypair, pairingId } = await offerAndHost();
  const wrongPsk = new Uint8Array(32).fill(7);
  /** @type {any[]} */
  const seen = [];
  const WS = fakeWebSocketClass(
    pairingHost({ hostKeypair, hostId: HOST_ID, psk: wrongPsk, pairingIdB64: pairingId, onPending: (i) => seen.push(i) }),
  );
  await assert.rejects(startPairing(offer, await generateKeypair(), { pageOrigin: PAGE, WebSocket: WS }), (e) => e instanceof PairingError);
  assert.equal(seen.length, 0, "the PC shows nothing for this phone");
});

test("a second phone while one is pending is refused by the relay with not_allowed", async () => {
  const { offer, hostKeypair, psk, pairingId } = await offerAndHost();
  const WS = fakeWebSocketClass(pairingHost({ hostKeypair, hostId: HOST_ID, psk, pairingIdB64: pairingId }));
  // The relay has one pairing channel per slot. The second phone's hello is refused before Noise.
  const refusing = fakeWebSocketClass((socket) => {
    socket.refuse("not_allowed");
    return null;
  });
  const first = await startPairing(offer, await generateKeypair(), { pageOrigin: PAGE, WebSocket: WS });
  await assert.rejects(startPairing(offer, await generateKeypair(), { pageOrigin: PAGE, WebSocket: refusing }), (e) => e instanceof PairingError && e.code === "not_allowed");
  assert.match(messageFor("not_allowed", { pairing: true }), /Pairing is in use by another phone/);
  first.cancel();
});

test("a confirm with the wrong check code is cut off by the PC, and the phone gets no answer", async () => {
  const { offer, hostKeypair, psk, pairingId } = await offerAndHost();
  const WS = fakeWebSocketClass(pairingHost({ hostKeypair, hostId: HOST_ID, psk, pairingIdB64: pairingId }));
  const session = await startPairing(offer, await generateKeypair(), { pageOrigin: PAGE, WebSocket: WS });
  // Tamper with the confirmation by sending a wrong check through the same channel.
  session.check = "0000";
  await assert.rejects(session.confirm("Pixel 9"), (e) => e instanceof PairingError);
});

test("the name is checked before anything is sent", async () => {
  const { offer, hostKeypair, psk, pairingId } = await offerAndHost();
  const WS = fakeWebSocketClass(pairingHost({ hostKeypair, hostId: HOST_ID, psk, pairingIdB64: pairingId }));
  const session = await startPairing(offer, await generateKeypair(), { pageOrigin: PAGE, WebSocket: WS });
  await assert.rejects(session.confirm("   "), (e) => e instanceof PairingError && e.code === "bad_request");
  await assert.rejects(session.confirm("a\nb"), (e) => e instanceof PairingError && e.code === "bad_request");
  await assert.rejects(session.confirm("x".repeat(65)), (e) => e instanceof PairingError && e.code === "bad_request");
  session.cancel();
});

test("an offer whose relay is not wss is refused before any connection", async () => {
  const { offer } = await offerAndHost();
  let opened = false;
  const WS = class {
    constructor() {
      opened = true;
    }
  };
  await assert.rejects(startPairing({ ...offer, r: "ws://relay.example.test" }, await generateKeypair(), { pageOrigin: PAGE, WebSocket: WS }), (e) => e instanceof PairingError);
  assert.equal(opened, false);
});

test("decodeOffer refuses a non-wss relay in a production page's offer", () => {
  const bad = Buffer.from(JSON.stringify({ v: 1, r: "http://relay.example.test", h: HOST_ID, k: pairingVec.offer.k, i: pairingVec.offer.i, p: pairingVec.offer.p })).toString("base64url");
  assert.equal(decodeOffer(bad), null);
});

test("decodeOffer refuses padding, extra spellings and garbage without throwing", () => {
  const payload = pairingVec.encoded;
  assert.equal(decodeOffer(`${payload}=`), null);
  assert.equal(decodeOffer(payload.slice(0, -2)), null);
  assert.equal(decodeOffer("#p="), null);
  assert.equal(decodeOffer("%%%"), null);
  assert.equal(decodeOffer(""), null);
  assert.equal(decodeOffer(/** @type {any} */ (null)), null);
});

test("checkRelayUrl: wss always, ws only to the loopback from the dev origin, nothing else", () => {
  assert.equal(checkRelayUrl("wss://relay.example.test", PAGE), "wss://relay.example.test");
  assert.equal(checkRelayUrl("wss://relay.example.test/", PAGE), "wss://relay.example.test");
  assert.equal(checkRelayUrl("ws://127.0.0.1:8787", DEV_PAGE), "ws://127.0.0.1:8787");
  const refused = [
    ["ws://127.0.0.1:8787", PAGE],
    ["ws://127.0.0.1:8787", "https://127.0.0.1:8080"],
    ["ws://relay.example.test", DEV_PAGE],
    ["ws://127.0.0.1.example.test:8787", DEV_PAGE],
    ["ws://127.0.0.1@evil.example:8787", DEV_PAGE],
    ["wss://user:pw@relay.example.test", PAGE],
    ["wss://relay.example.test/path", PAGE],
    ["wss://relay.example.test?x=1", PAGE],
    ["https://relay.example.test", PAGE],
    ["wss://relay.example.test\n", PAGE],
    ["", PAGE],
    [42, PAGE],
  ];
  for (const [url, origin] of refused) {
    assert.throws(() => checkRelayUrl(url, /** @type {string} */ (origin)), RelayError, String(url));
  }
});

test("the pairing channel is opened on the relay's client path only", async () => {
  const { offer, hostKeypair, psk, pairingId } = await offerAndHost();
  /** @type {string[]} */
  const urls = [];
  const inner = fakeWebSocketClass(pairingHost({ hostKeypair, hostId: HOST_ID, psk, pairingIdB64: pairingId }));
  const WS = class extends inner {
    /** @param {string} url */
    constructor(url) {
      super(url);
      urls.push(url);
    }
  };
  const session = await startPairing(offer, await generateKeypair(), { pageOrigin: PAGE, WebSocket: WS });
  assert.deepEqual(urls, ["wss://relay.example.test/v1/client"]);
  session.cancel();
});
