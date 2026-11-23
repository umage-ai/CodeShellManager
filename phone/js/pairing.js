// @ts-check
// Pairing (design section 8 and pairing.rs). The phone reads the offer from the link or the QR code,
// opens the one pairing channel with the offer's slot id, runs Noise IKpsk2 (the PSK comes from the
// offer), and shows the check code. The person reads the same four digits on the PC. Only then does
// the phone send `Confirm` and its name; the PC shows the request and the person allows or denies it.

import { b64urlDecode, b64urlFixed, concat, utf8 } from "./bytes.js";
import { sha256 } from "./crypto.js";
import { Handshake, PARAMS_PAIRING } from "./noise.js";
import { parsePairReply, PROTOCOL } from "./protocol.js";
import { checkRelayUrl, helloForPairing, RelayError, RelayLink } from "./relay.js";

export const OFFER_VERSION = 1;

/**
 * The phone's first encrypted message in the pairing channel: the key confirmation (pairing.rs).
 * @param {string} check the four digits the phone shows
 * @returns {{ t: "confirm", check: string }}
 */
export function confirmMessage(check) {
  return { t: "confirm", check };
}

/**
 * The second: the device name (pairing.rs `PairMsg::Hello`).
 * @param {string} name
 * @returns {{ t: "hello", v: 1, name: string }}
 */
export function helloMessage(name) {
  return { t: "hello", v: PROTOCOL, name };
}
export const MAX_NAME_CHARS = 64;
const PAIRING_ID_LEN = 16;
const KEY_LEN = 32;
const PSK_LEN = 32;
const HOST_ID = /^[a-z2-7]{26}$/;
const CHECK_DOMAIN = utf8("csm-remote check");

/**
 * @typedef {object} Offer
 * @property {1} v
 * @property {string} r relay base URL (`wss://...`)
 * @property {string} h host id
 * @property {string} k host static public key (base64url, 32 bytes)
 * @property {string} i pairing id (base64url, 16 bytes)
 * @property {string} p PSK (base64url, 32 bytes)
 */

export class PairingError extends Error {
  /**
   * @param {string} code
   */
  constructor(code) {
    super(code);
    this.name = "PairingError";
    this.code = code;
  }
}

/**
 * The four digits: u32_be(SHA-256("csm-remote check" || handshake_hash)[0..4]) mod 10000, zero padded.
 * @param {Uint8Array} handshakeHash
 * @returns {Promise<string>}
 */
export async function checkCode(handshakeHash) {
  const d = await sha256(concat(CHECK_DOMAIN, handshakeHash));
  const n = ((d[0] << 24) >>> 0) + (d[1] << 16) + (d[2] << 8) + d[3];
  return String(n % 10000).padStart(4, "0");
}

/**
 * Reads an offer from the pasted text: the bare payload, `p=<payload>`, `#p=<payload>` or the whole link.
 * Refuses anything that is not a well-formed offer. Never throws.
 * @param {string} input
 * @returns {Offer | null}
 */
export function decodeOffer(input) {
  if (typeof input !== "string") return null;
  const trimmed = input.trim();
  const hash = trimmed.lastIndexOf("#");
  const fragment = hash >= 0 ? trimmed.slice(hash + 1) : trimmed;
  const payload = fragment.startsWith("p=") ? fragment.slice(2) : fragment;
  const bytes = b64urlDecode(payload);
  if (!bytes) return null;
  /** @type {unknown} */
  let v;
  try {
    v = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
  } catch {
    return null;
  }
  if (typeof v !== "object" || v === null) return null;
  const o = /** @type {Record<string, unknown>} */ (v);
  if (o.v !== OFFER_VERSION) return null;
  if (typeof o.r !== "string" || o.r.length > 256 || /[\s\u0000-\u001f\u007f]/.test(o.r)) return null;
  if (!(o.r.startsWith("wss://") || o.r.startsWith("ws://127.0.0.1"))) return null;
  if (typeof o.h !== "string" || !HOST_ID.test(o.h)) return null;
  if (!b64urlFixed(o.k, KEY_LEN) || !b64urlFixed(o.i, PAIRING_ID_LEN) || !b64urlFixed(o.p, PSK_LEN)) return null;
  return { v: OFFER_VERSION, r: o.r, h: o.h, k: /** @type {string} */ (o.k), i: /** @type {string} */ (o.i), p: /** @type {string} */ (o.p) };
}

/**
 * A pairing in progress: the handshake is done and the check code is known. `confirm` sends the
 * confirmation and the name, then waits for the PC's answer.
 */
export class PairingSession {
  /**
   * @param {RelayLink} link
   * @param {{ send: import("./noise.js").Transport, recv: import("./noise.js").Transport }} transport
   * @param {string} check
   * @param {Uint8Array} hostPub
   * @param {string} hostId
   * @param {string} relay the relay base the offer named (`wss://...`)
   */
  constructor(link, transport, check, hostPub, hostId, relay) {
    this.relay = relay;
    this.link = link;
    this.transport = transport;
    this.check = check;
    this.hostPub = hostPub;
    this.hostId = hostId;
    this.confirmed = false;
  }

  /**
   * Sends `Confirm` (the check code) and `Hello` (the name), then waits for the PC to allow or deny.
   * @param {string} name
   * @returns {Promise<{ routingToken: Uint8Array, level: "viewer" | "operator", deviceId: string, hostPub: Uint8Array, hostId: string, relay: string }>}
   */
  async confirm(name) {
    if (this.confirmed) throw new PairingError("bad_request");
    const trimmed = name.trim();
    if (trimmed.length < 1 || [...trimmed].length > MAX_NAME_CHARS || /[\u0000-\u001f\u007f]/.test(trimmed)) throw new PairingError("bad_request");
    this.confirmed = true;
    try {
      this.link.send(await this.transport.send.seal(utf8(JSON.stringify(confirmMessage(this.check)))));
      this.link.send(await this.transport.send.seal(utf8(JSON.stringify(helloMessage(trimmed)))));
      const reply = parsePairReply(await this.transport.recv.open(await this.link.next()));
      if (!reply) throw new PairingError("bad_request");
      if (reply.t === "deny") throw new PairingError(reply.reason);
      const token = b64urlFixed(reply.routing_token, 32);
      if (!token) throw new PairingError("bad_request");
      return { routingToken: token, level: reply.level, deviceId: reply.device_id, hostPub: this.hostPub, hostId: this.hostId, relay: this.relay };
    } catch (e) {
      if (e instanceof PairingError) throw e;
      if (e instanceof RelayError) throw new PairingError(e.code);
      throw new PairingError("bad_request");
    } finally {
      this.link.close();
    }
  }

  /** Closes the channel without answering. */
  cancel() {
    this.link.close();
  }
}

/**
 * Opens the pairing channel and runs the handshake. Resolves with the check code to show; the
 * phone's identity key is `identity` (a Keypair from crypto.js).
 * @param {Offer} offer
 * @param {import("./crypto.js").Keypair} identity
 * @param {{ pageOrigin: string, WebSocket?: any }} opts
 * @returns {Promise<PairingSession>}
 */
export async function startPairing(offer, identity, opts) {
  let base;
  try {
    base = checkRelayUrl(offer.r, opts.pageOrigin);
  } catch (e) {
    throw new PairingError(e instanceof RelayError ? e.code : "bad_request");
  }
  const hostPub = b64urlFixed(offer.k, KEY_LEN);
  const psk = b64urlFixed(offer.p, PSK_LEN);
  const pairingId = b64urlFixed(offer.i, PAIRING_ID_LEN);
  if (!hostPub || !psk || !pairingId || !HOST_ID.test(offer.h)) throw new PairingError("bad_request");
  const link = new RelayLink(base, { WebSocket: opts.WebSocket });
  try {
    await link.hello(helloForPairing(offer.h, offer.i));
    const hs = new Handshake({
      role: "initiator",
      params: PARAMS_PAIRING,
      prologue: prologueFor(offer.h),
      s: identity,
      rs: hostPub,
      psk,
    });
    link.send(await hs.write(new Uint8Array(0)));
    const msg2 = await link.next();
    await hs.read(msg2);
    const fin = await hs.finish();
    const check = await checkCode(fin.hash);
    return new PairingSession(link, fin, check, hostPub, offer.h, base);
  } catch (e) {
    link.close();
    if (e instanceof RelayError) throw new PairingError(e.code);
    if (e instanceof PairingError) throw e;
    throw new PairingError("bad_request");
  }
}

/**
 * @param {string} hostId
 * @returns {Uint8Array}
 */
export function prologueFor(hostId) {
  return utf8(`csm-remote/1\u0000${hostId}`);
}

