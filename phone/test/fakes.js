// In-process fakes for the tests: a WebSocket-shaped relay socket, and a PC (the "host") that answers
// on the other end of it, using the same Noise code as the app, as the responder. Nothing here
// touches the network. The phone under test gets this class as its WebSocket constructor.

import { b64urlEncode, utf8 } from "../js/bytes.js";
import { Handshake, PARAMS_PAIRING, PARAMS_SESSION } from "../js/noise.js";
import { checkCode, prologueFor } from "../js/pairing.js";
import { REFUSAL_NAMES } from "../js/protocol.js";

/**
 * @typedef {{ onBytes: (bytes: Uint8Array) => Promise<void> | void }} HostHandler
 */

/**
 * A WebSocket class whose far end is `onHello(socket, hello)`. The hello is the phone's JSON text
 * (`ClientToRelay::Hello`); the function returns the handler for the binary messages after it, or
 * null after it has refused the socket.
 * @param {(socket: FakeSocket, hello: any) => HostHandler | null} onHello
 * @returns {new (url: string) => FakeSocket}
 */
export function fakeWebSocketClass(onHello) {
  return class extends FakeSocket {
    /** @param {string} url */
    constructor(url) {
      super(url, onHello);
    }
  };
}

export class FakeSocket {
  /**
   * @param {string} url
   * @param {(socket: FakeSocket, hello: any) => HostHandler | null} onHello
   */
  constructor(url, onHello) {
    this.url = url;
    this.onHello = onHello;
    this.readyState = 0;
    this.binaryType = "blob";
    /** @type {((ev: any) => void) | null} */
    this.onopen = null;
    /** @type {((ev: any) => void) | null} */
    this.onmessage = null;
    /** @type {((ev: any) => void) | null} */
    this.onclose = null;
    /** @type {((ev: any) => void) | null} */
    this.onerror = null;
    /** @type {HostHandler | null} */
    this.handler = null;
    /** @type {Uint8Array[]} every binary message the phone sent, in order */
    this.received = [];
    /** @type {string[]} every text message the phone sent */
    this.texts = [];
    this.closedWith = /** @type {number | null} */ (null);
    setTimeout(() => {
      this.readyState = 1;
      this.onopen?.({});
    }, 0);
  }

  /**
   * @param {string | Uint8Array} data
   */
  send(data) {
    if (this.readyState !== 1) throw new Error("socket is not open");
    if (typeof data === "string") {
      this.texts.push(data);
      if (this.handler) throw new Error("the phone sent text after the hello");
      const hello = JSON.parse(data);
      this.handler = this.onHello(this, hello);
      if (!this.handler) return;
      this.textToPhone({ t: "ready", v: 1 });
      return;
    }
    const copy = new Uint8Array(data);
    this.received.push(copy);
    if (!this.handler) throw new Error("binary before the hello");
    const handler = this.handler;
    setTimeout(() => void handler.onBytes(copy), 0);
  }

  /**
   * Host to phone: a text message (ready, error).
   * @param {unknown} obj
   */
  textToPhone(obj) {
    setTimeout(() => this.onmessage?.({ data: JSON.stringify(obj) }), 0);
  }

  /**
   * Host to phone: one Noise message, delivered as an ArrayBuffer.
   * @param {Uint8Array} bytes
   */
  bytesToPhone(bytes) {
    const buf = bytes.slice().buffer;
    setTimeout(() => this.onmessage?.({ data: buf }), 0);
  }

  /**
   * Host refuses: an error text, then the close with the refusal's code (4000 + n).
   * @param {string} code
   */
  refuse(code) {
    const n = REFUSAL_NAMES.indexOf(/** @type {any} */ (code)) + 1;
    this.textToPhone({ t: "error", code });
    setTimeout(() => this.close(4000 + n, true), 1);
  }

  /**
   * @param {number} [code]
   * @param {boolean} [fromHost]
   */
  close(code = 1000, fromHost = false) {
    if (this.readyState === 3) return;
    this.readyState = 3;
    this.closedWith = code;
    this.onclose?.({ code, reason: fromHost ? "host" : "" });
  }
}

/**
 * A PC answering a pairing channel. It keeps the handshake's check code and the phone's answers,
 * and decides as `decide` says: "allow" or "deny". `onPending` sees the phone's name and check code.
 * @param {object} o
 * @param {import("../js/crypto.js").Keypair} o.hostKeypair
 * @param {string} o.hostId
 * @param {Uint8Array} o.psk
 * @param {string} o.pairingIdB64
 * @param {"allow" | "deny"} [o.decide]
 * @param {"viewer" | "operator"} [o.level]
 * @param {(info: { name: string, phoneCheck: string, hostCheck: string }) => void} [o.onPending]
 * @param {Uint8Array} [o.routingToken]
 * @returns {(socket: FakeSocket, hello: any) => HostHandler | null}
 */
export function pairingHost(o) {
  return (socket, hello) => {
    if (hello.t !== "hello" || hello.host_id !== o.hostId || hello.pairing_id !== o.pairingIdB64) {
      socket.refuse("not_allowed");
      return null;
    }
    const hs = new Handshake({ role: "responder", params: PARAMS_PAIRING, prologue: prologueFor(o.hostId), s: o.hostKeypair, psk: o.psk });
    /** @type {{ send: import("../js/noise.js").Transport, recv: import("../js/noise.js").Transport, hash: Uint8Array, remoteStatic: Uint8Array | null } | null} */
    let fin = null;
    /** @type {"msg1" | "confirm" | "hello" | "done"} */
    let step = "msg1";
    let hostCheck = "";
    let phoneCheck = "";
    return {
      onBytes: async (bytes) => {
        if (step === "msg1") {
          await hs.read(bytes);
          socket.bytesToPhone(await hs.write(utf8("host")));
          fin = await hs.finish();
          hostCheck = await checkCode(fin.hash);
          step = "confirm";
          return;
        }
        if (!fin) throw new Error("no transport");
        const plain = JSON.parse(new TextDecoder().decode(await fin.recv.open(bytes)));
        if (step === "confirm") {
          if (plain.t !== "confirm" || plain.check !== hostCheck) {
            socket.close(4000, true);
            step = "done";
            return;
          }
          phoneCheck = plain.check;
          step = "hello";
          return;
        }
        if (step === "hello") {
          o.onPending?.({ name: plain.name, phoneCheck, hostCheck });
          const reply =
            o.decide === "deny"
              ? { t: "deny", reason: "denied" }
              : { t: "accept", routing_token: b64urlEncode(o.routingToken ?? new Uint8Array(32).fill(9)), level: o.level ?? "operator", device_id: "d_test1" };
          socket.bytesToPhone(await fin.send.seal(utf8(JSON.stringify(reply))));
          step = "done";
        }
      },
    };
  };
}

/**
 * A PC answering a session channel (IK). `expectToken` is the routing token the phone must present.
 * `serve(request)` answers each decrypted request with a `{ ok, data }` or `{ ok: false, code }`
 * object, and `push(event)` sends an event. Calling `push` before the handshake is a test error.
 * @param {object} o
 * @param {import("../js/crypto.js").Keypair} o.hostKeypair
 * @param {string} o.hostId
 * @param {Uint8Array} o.expectToken
 * @param {(req: any, host: { reply: (obj: unknown) => Promise<void>, event: (obj: unknown) => Promise<void>, raw: (bytes: Uint8Array) => void }) => Promise<void> | void} o.serve
 * @param {{ phoneStatic?: Uint8Array | null, seen?: any[] }} [o.log]
 * @returns {(socket: FakeSocket, hello: any) => HostHandler | null}
 */
export function sessionHost(o) {
  return (socket, hello) => {
    if (hello.t !== "hello" || hello.host_id !== o.hostId || hello.token !== b64urlEncode(o.expectToken)) {
      socket.refuse("not_allowed");
      return null;
    }
    const hs = new Handshake({ role: "responder", params: PARAMS_SESSION, prologue: prologueFor(o.hostId), s: o.hostKeypair });
    /** @type {{ send: import("../js/noise.js").Transport, recv: import("../js/noise.js").Transport, remoteStatic: Uint8Array | null } | null} */
    let fin = null;
    let first = true;
    return {
      onBytes: async (bytes) => {
        if (first) {
          first = false;
          await hs.read(bytes);
          socket.bytesToPhone(await hs.write(new Uint8Array(0)));
          fin = await hs.finish();
          if (o.log) o.log.phoneStatic = fin.remoteStatic;
          return;
        }
        const ready = fin;
        if (!ready) throw new Error("no transport");
        const req = JSON.parse(new TextDecoder().decode(await ready.recv.open(bytes)));
        if (o.log?.seen) o.log.seen.push(req);
        const host = {
          /** @param {unknown} obj */
          reply: async (obj) => socket.bytesToPhone(await ready.send.seal(utf8(JSON.stringify(obj)))),
          /** @param {unknown} obj */
          event: async (obj) => socket.bytesToPhone(await ready.send.seal(utf8(JSON.stringify(obj)))),
          /** Bytes as they are (a message the phone cannot decrypt). @param {Uint8Array} bytes */
          raw: (bytes) => socket.bytesToPhone(bytes),
        };
        await o.serve(req, host);
      },
    };
  };
}

