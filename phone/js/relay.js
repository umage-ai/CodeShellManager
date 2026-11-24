// @ts-check
// The WebSocket to the relay, for one phone channel (design section 6 and relay/README.md).
//
// The phone opens `<relay>/v1/client`, sends one JSON text hello (`ClientToRelay::Hello`), waits for
// `{"t":"ready"}`, then exchanges binary messages. Each binary message is one Noise message, with no
// frame header: the relay adds that. A refusal arrives as `{"t":"error","code":...}` and as the close
// code 4000 + n. Nothing is sent to a relay before the page has checked its address (`checkRelayUrl`).

import { refusalFromCloseCode } from "./protocol.js";

export const RELAY_PROTOCOL = 1;

/**
 * The hello of a session channel: the routing token, base64url (`ClientToRelay::Hello`).
 * @param {string} hostId
 * @param {string} tokenB64
 * @returns {{ t: "hello", v: 1, host_id: string, token: string }}
 */
export function helloForToken(hostId, tokenB64) {
  return { t: "hello", v: 1, host_id: hostId, token: tokenB64 };
}

/**
 * The hello of a pairing channel: the offer's host id and pairing slot id.
 * @param {string} hostId
 * @param {string} pairingIdB64
 * @returns {{ t: "hello", v: 1, host_id: string, pairing_id: string }}
 */
export function helloForPairing(hostId, pairingIdB64) {
  return { t: "hello", v: 1, host_id: hostId, pairing_id: pairingIdB64 };
}
const READY_TIMEOUT_MS = 10_000;
const MAX_QUEUED = 64;

export class RelayError extends Error {
  /**
   * @param {string} code a refusal code, or "unavailable" for a network failure
   * @param {string} [detail]
   */
  constructor(code, detail = "") {
    super(code);
    this.name = "RelayError";
    this.code = code;
    this.detail = detail;
  }
}

/**
 * The address rule of design 10.1: `wss://` always. `ws://127.0.0.1` only when this page is the
 * local dev origin (`http://127.0.0.1:<port>`). Returns the base without a trailing slash.
 * @param {unknown} url
 * @param {string} pageOrigin
 * @returns {string}
 */
export function checkRelayUrl(url, pageOrigin) {
  if (typeof url !== "string" || url.length === 0 || url.length > 256 || /[\s\u0000-\u001f\u007f]/.test(url)) {
    throw new RelayError("bad_request", "The relay address is not valid.");
  }
  /** @type {URL} */
  let u;
  try {
    u = new URL(url);
  } catch {
    throw new RelayError("bad_request", "The relay address is not valid.");
  }
  if (u.username !== "" || u.password !== "" || u.search !== "" || u.hash !== "" || (u.pathname !== "/" && u.pathname !== "")) {
    throw new RelayError("bad_request", "The relay address has parts the phone does not use.");
  }
  if (u.protocol === "wss:" && u.hostname !== "") return `wss://${u.host}`;
  const devOrigin = /^http:\/\/127\.0\.0\.1:\d{1,5}$/.test(pageOrigin);
  if (u.protocol === "ws:" && u.hostname === "127.0.0.1" && devOrigin) return `ws://${u.host}`;
  throw new RelayError("bad_request", "The relay must use wss://.");
}

/**
 * The WebSocket constructor (injected so the tests can use a fake).
 * @typedef {new (url: string) => any} WebSocketLike
 */

export class RelayLink {
  /**
   * @param {string} base from `checkRelayUrl`
   * @param {{ WebSocket?: WebSocketLike }} [opts]
   */
  constructor(base, opts = {}) {
    const Ctor = opts.WebSocket ?? /** @type {WebSocketLike} */ (/** @type {any} */ (globalThis).WebSocket);
    this.ws = new Ctor(`${base}/v1/client`);
    this.ws.binaryType = "arraybuffer";
    /** @type {Uint8Array[]} */
    this.queue = [];
    /** @type {Array<{ resolve: (b: Uint8Array) => void, reject: (e: Error) => void }>} */
    this.waiters = [];
    /** @type {RelayError | null} */
    this.failed = null;
    /** @type {((v: void) => void) | null} */
    this.readyResolve = null;
    /** @type {((e: Error) => void) | null} */
    this.readyReject = null;
    this.ready = false;
    this.ws.onmessage = (/** @type {{ data: unknown }} */ ev) => this.onMessage(ev.data);
    this.ws.onclose = (/** @type {{ code?: number }} */ ev) => this.onClose(ev.code ?? 0);
    this.ws.onerror = () => {
      // The close event follows; nothing more to do here.
    };
  }

  /**
   * Sends the JSON hello and resolves once the relay says ready.
   * @param {Record<string, unknown>} hello
   * @returns {Promise<void>}
   */
  hello(hello) {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => this.fail(new RelayError("unavailable", "no answer from the relay")), READY_TIMEOUT_MS);
      this.readyResolve = () => {
        clearTimeout(timer);
        resolve();
      };
      this.readyReject = (e) => {
        clearTimeout(timer);
        reject(e);
      };
      const onOpen = () => {
        try {
          this.ws.send(JSON.stringify(hello));
        } catch {
          this.fail(new RelayError("unavailable"));
        }
      };
      if (this.ws.readyState === 1) onOpen();
      else this.ws.onopen = onOpen;
    });
  }

  /**
   * @param {unknown} data
   */
  onMessage(data) {
    if (typeof data === "string") {
      /** @type {unknown} */
      let v;
      try {
        v = JSON.parse(data);
      } catch {
        this.fail(new RelayError("bad_request"));
        return;
      }
      const t = typeof v === "object" && v !== null ? /** @type {Record<string, unknown>} */ (v) : {};
      if (t.t === "ready" && !this.ready) {
        this.ready = true;
        this.readyResolve?.();
        return;
      }
      if (t.t === "error" && typeof t.code === "string") {
        this.fail(new RelayError(t.code));
        return;
      }
      this.fail(new RelayError("bad_request"));
      return;
    }
    if (data instanceof ArrayBuffer) {
      if (!this.ready) {
        this.fail(new RelayError("bad_request"));
        return;
      }
      const bytes = new Uint8Array(data);
      const w = this.waiters.shift();
      if (w) w.resolve(bytes);
      else if (this.queue.length < MAX_QUEUED) this.queue.push(bytes);
      else this.fail(new RelayError("rate_limited"));
    }
  }

  /**
   * @param {number} code the WebSocket close code
   */
  onClose(code) {
    const refusal = refusalFromCloseCode(code);
    this.fail(new RelayError(refusal ?? "unavailable"));
  }

  /**
   * @param {RelayError} e
   */
  fail(e) {
    if (this.failed) return;
    this.failed = e;
    if (!this.ready) this.readyReject?.(e);
    for (const w of this.waiters.splice(0)) w.reject(e);
    try {
      this.ws.close();
    } catch {
      // already closed
    }
  }

  /**
   * @param {Uint8Array} bytes one Noise message
   */
  send(bytes) {
    if (this.failed) throw this.failed;
    this.ws.send(bytes);
  }

  /**
   * The next Noise message from the relay.
   * @returns {Promise<Uint8Array>}
   */
  next() {
    const q = this.queue.shift();
    if (q) return Promise.resolve(q);
    if (this.failed) return Promise.reject(this.failed);
    return new Promise((resolve, reject) => this.waiters.push({ resolve, reject }));
  }

  close() {
    this.failed = this.failed ?? new RelayError("unavailable", "closed by the phone");
    try {
      this.ws.close();
    } catch {
      // already closed
    }
  }
}
