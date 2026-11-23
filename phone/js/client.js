// @ts-check
// One session channel to a paired PC (design sections 6, 7 and 10.2). Connects through the relay with
// the device's routing token, runs Noise IK (the phone is the initiator; the PC's static key is pinned
// from the QR code at pairing), then carries JSON requests and events inside the Noise transport.
//
// Requests go out in the order they are made, one at a time on the wire, because Noise has no
// reordering: each message's nonce is the next in its direction. Answers are matched by id. Events
// feed the session list (delta.js) and the screens.

import { b64urlEncode, utf8 } from "./bytes.js";
import { prologueFor } from "./pairing.js";
import { Handshake, PARAMS_SESSION } from "./noise.js";
import { encodeRequest, parseHelloData, parseServerMsg, PROTOCOL, messageFor } from "./protocol.js";
import { checkRelayUrl, helloForToken, RelayError, RelayLink } from "./relay.js";
import { SessionList } from "./delta.js";

export const REQUEST_TIMEOUT_MS = 30_000;
export const CLIENT_NAME = "csm-phone-web 0.1";

export class ClientError extends Error {
  /**
   * @param {string} code
   * @param {string} [detail]
   */
  constructor(code, detail = "") {
    super(messageFor(code));
    this.name = "ClientError";
    this.code = code;
    this.detail = detail;
  }
}

/**
 * @typedef {import("./protocol.js").ServerMsg} ServerMsg
 * @typedef {{ kind: "list" } | { kind: "event", msg: ServerMsg } | { kind: "closed", error: Error }} ClientEvent
 * @typedef {import("./protocol.js").HelloData} HelloData
 * @typedef {{ host_id: string, host_pub: Uint8Array, routing_token: Uint8Array, relay: string }} DeviceLink
 * @typedef {{ device: DeviceLink, identity: import("./crypto.js").Keypair, pageOrigin: string, WebSocket?: any }} ClientOptions
 */

export class HostClient {
  /**
   * @param {ClientOptions} opts
   */
  constructor(opts) {
    this.opts = opts;
    this.list = new SessionList();
    /** @type {Set<(e: ClientEvent) => void>} */
    this.listeners = new Set();
    /** @type {Map<number, { resolve: (d: unknown) => void, reject: (e: Error) => void }>} */
    this.pending = new Map();
    this.nextId = 1;
    /** @type {RelayLink | null} */
    this.link = null;
    /** @type {import("./noise.js").Transport | null} */
    this.send = null;
    /** @type {import("./noise.js").Transport | null} */
    this.recv = null;
    /** @type {HelloData | null} */
    this.hello = null;
    /** @type {Promise<void>} */
    this.outbound = Promise.resolve();
    this.closedError = /** @type {Error | null} */ (null);
  }

  /**
   * @param {(e: ClientEvent) => void} fn
   * @returns {() => void} unsubscribe
   */
  onEvent(fn) {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  /** @param {ClientEvent} e */
  emit(e) {
    for (const fn of this.listeners) fn(e);
  }

  /**
   * Opens the channel and says hello. Rejects with a RelayError (refusal or network) or a ClientError.
   * @returns {Promise<HelloData>}
   */
  async connect() {
    const { device, identity, pageOrigin, WebSocket } = this.opts;
    const base = checkRelayUrl(device.relay, pageOrigin);
    const link = new RelayLink(base, { WebSocket });
    try {
      await link.hello(helloForToken(device.host_id, b64urlEncode(device.routing_token)));
      const hs = new Handshake({ role: "initiator", params: PARAMS_SESSION, prologue: prologueFor(device.host_id), s: identity, rs: device.host_pub });
      link.send(await hs.write(new Uint8Array(0)));
      await hs.read(await link.next());
      const fin = await hs.finish();
      this.link = link;
      this.send = fin.send;
      this.recv = fin.recv;
      void this.readLoop(link, fin.recv);
      const data = await this.request("hello", { client: CLIENT_NAME, protocol: PROTOCOL });
      const hello = parseHelloData(data);
      if (!hello) throw new ClientError("bad_request", "hello reply");
      this.hello = hello;
      return hello;
    } catch (e) {
      link.close();
      this.link = null;
      if (e instanceof RelayError) throw e;
      if (e instanceof ClientError) throw e;
      throw new ClientError("bad_request", String(e));
    }
  }

  /**
   * Reads the channel until it closes. A message that does not decrypt ends the channel: the Noise
   * state can no longer be trusted.
   * @param {RelayLink} link
   * @param {import("./noise.js").Transport} recv
   */
  async readLoop(link, recv) {
    try {
      for (;;) {
        const bytes = await link.next();
        const plain = await recv.open(bytes);
        const msg = parseServerMsg(plain);
        if (msg) this.dispatch(msg);
      }
    } catch (e) {
      this.closed(e instanceof RelayError ? e : new RelayError("bad_request", String(e)));
    }
  }

  /**
   * @param {ServerMsg} msg
   */
  dispatch(msg) {
    if (msg.kind === "response") {
      const w = this.pending.get(msg.id);
      if (!w) return;
      this.pending.delete(msg.id);
      if (msg.ok) w.resolve(msg.data);
      else w.reject(new ClientError(msg.code, msg.message));
      return;
    }
    if (msg.ev === "snapshot" || msg.ev === "delta" || msg.ev === "inbox") {
      const r = this.list.apply(msg);
      if (r.resync) void this.request("snapshot").catch(() => undefined);
      if (r.changed) this.emit({ kind: "list" });
      return;
    }
    this.emit({ kind: "event", msg });
    if (msg.ev === "bye") this.close(new RelayError(msg.reason));
  }

  /**
   * Sends one request and resolves with its data.
   * @param {string} op
   * @param {Record<string, unknown>} [args]
   * @returns {Promise<unknown>}
   */
  request(op, args = {}) {
    const send = this.send;
    const link = this.link;
    if (!send || !link || this.closedError) return Promise.reject(new ClientError("unavailable"));
    const id = this.nextId++;
    const text = encodeRequest(id, op, args);
    if (text === null) return Promise.reject(new ClientError("too_large"));
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new ClientError("unavailable"));
      }, REQUEST_TIMEOUT_MS);
      this.pending.set(id, {
        resolve: (d) => {
          clearTimeout(timer);
          resolve(d);
        },
        reject: (e) => {
          clearTimeout(timer);
          reject(e);
        },
      });
      // One at a time on the wire: seal and send in the order requests were made.
      this.outbound = this.outbound.then(async () => {
        try {
          const sealed = await send.seal(utf8(text));
          link.send(sealed);
        } catch (e) {
          this.pending.delete(id);
          clearTimeout(timer);
          reject(e instanceof Error ? e : new ClientError("unavailable"));
        }
      });
    });
  }

  /**
   * Closes the channel on purpose or after an error from the host.
   * @param {Error} [error]
   */
  close(error) {
    const e = error ?? new RelayError("unavailable", "closed by the phone");
    this.link?.close();
    this.closed(e);
  }

  /**
   * @param {Error} error
   */
  closed(error) {
    if (this.closedError) return;
    this.closedError = error;
    for (const w of this.pending.values()) w.reject(new ClientError("unavailable"));
    this.pending.clear();
    this.link = null;
    this.emit({ kind: "closed", error });
  }
}
