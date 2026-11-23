// @ts-check
// Noise IK and IKpsk2 over WebCrypto (design section 10.3). Only what the phone needs: the initiator
// role for the phone, and the responder role, which the tests use as the host side. The pattern
// tables are the Noise spec's; the vectors in test/fixtures/noise.json check every byte.
//
// Protocol names (csm-remote-proto `noise.rs`): PARAMS_PAIRING (IKpsk2, the phone pairs with a PC it
// has an offer for) and PARAMS_SESSION (IK, a paired phone reconnects).

import { concat } from "./bytes.js";
import { aead, dh, generateKeypair, hkdf, sha256 } from "./crypto.js";

export const PARAMS_PAIRING = "Noise_IKpsk2_25519_AESGCM_SHA256";
export const PARAMS_SESSION = "Noise_IK_25519_AESGCM_SHA256";
export const REKEY_INTERVAL = 10000;
export const KEY_LEN = 32;
export const TAG_LEN = 16;

const EMPTY = new Uint8Array(0);
const MAX_NONCE = (1n << 64n) - 1n;

/**
 * Message patterns. A message is a list of tokens; `psk` is the PSK token of IKpsk2 (message 2).
 * @type {Record<string, string[][]>}
 */
const PATTERNS = {
  IK: [["e", "es", "s", "ss"], ["e", "ee", "se"]],
  IKpsk2: [["e", "es", "s", "ss"], ["e", "ee", "se", "psk"]],
};

/**
 * @typedef {import("./crypto.js").Keypair} Keypair
 */

/**
 * @param {bigint} n
 * @returns {Uint8Array}
 */
function nonceBytes(n) {
  const b = new Uint8Array(12);
  new DataView(b.buffer).setBigUint64(4, n, false);
  return b;
}

export class CipherState {
  /**
   * @param {Uint8Array | null} key
   */
  constructor(key) {
    /** @type {Uint8Array | null} */
    this.key = key;
    this.n = 0n;
  }

  /**
   * @param {Uint8Array} ad
   * @param {Uint8Array} plaintext
   * @returns {Promise<Uint8Array>}
   */
  async encrypt(ad, plaintext) {
    if (!this.key) return plaintext;
    if (this.n >= MAX_NONCE) throw new Error("nonce exhausted");
    // The nonce is taken before the first await, so two calls in flight cannot share one.
    const key = this.key;
    const n = this.n;
    this.n += 1n;
    return aead(key, nonceBytes(n), ad, plaintext, "encrypt");
  }

  /**
   * @param {Uint8Array} ad
   * @param {Uint8Array} ciphertext
   * @returns {Promise<Uint8Array>}
   */
  async decrypt(ad, ciphertext) {
    if (!this.key) return ciphertext;
    if (this.n >= MAX_NONCE) throw new Error("nonce exhausted");
    const key = this.key;
    const n = this.n;
    this.n += 1n;
    return aead(key, nonceBytes(n), ad, ciphertext, "decrypt");
  }

  /** Noise REKEY: the new key is the first 32 bytes of ENCRYPT(k, maxnonce, "", zeros(32)). */
  async rekey() {
    if (!this.key) return;
    const out = await aead(this.key, nonceBytes(MAX_NONCE), EMPTY, new Uint8Array(KEY_LEN), "encrypt");
    this.key = out.slice(0, KEY_LEN);
  }
}

/**
 * The symmetric state of the handshake (Noise spec 5.2).
 */
class SymmetricState {
  /**
   * @param {string} protocolName
   */
  constructor(protocolName) {
    const name = new TextEncoder().encode(protocolName);
    /** @type {Uint8Array} */
    this.h = name.length <= 32 ? concat(name, new Uint8Array(32 - name.length)) : EMPTY;
    this.ready = name.length <= 32 ? Promise.resolve() : sha256(name).then((d) => (this.h = d));
    /** @type {Uint8Array} */
    this.ck = EMPTY;
    this.cipher = new CipherState(null);
  }

  /** @returns {Promise<void>} */
  async init() {
    await this.ready;
    this.ck = this.h.slice();
  }

  /** @param {Uint8Array} data */
  async mixHash(data) {
    this.h = await sha256(concat(this.h, data));
  }

  /** @param {Uint8Array} ikm */
  async mixKey(ikm) {
    const [ck, k] = await hkdf(this.ck, ikm, 2);
    this.ck = ck;
    this.cipher = new CipherState(k);
  }

  /** @param {Uint8Array} ikm */
  async mixKeyAndHash(ikm) {
    const [ck, th, tk] = await hkdf(this.ck, ikm, 3);
    this.ck = ck;
    await this.mixHash(th);
    this.cipher = new CipherState(tk);
  }

  /** @param {Uint8Array} plaintext */
  async encryptAndHash(plaintext) {
    const ct = await this.cipher.encrypt(this.h, plaintext);
    await this.mixHash(ct);
    return ct;
  }

  /** @param {Uint8Array} ciphertext */
  async decryptAndHash(ciphertext) {
    const pt = await this.cipher.decrypt(this.h, ciphertext);
    await this.mixHash(ciphertext);
    return pt;
  }

  /** @returns {Promise<[CipherState, CipherState]>} */
  async split() {
    const [k1, k2] = await hkdf(this.ck, EMPTY, 2);
    return [new CipherState(k1), new CipherState(k2)];
  }
}

/**
 * One side of a handshake. Call `write` and `read` in turn; after the last message, `finish` gives
 * the transport ciphers and the handshake hash.
 */
export class Handshake {
  /**
   * @param {object} o
   * @param {"initiator" | "responder"} o.role
   * @param {string} o.params `PARAMS_PAIRING` or `PARAMS_SESSION`
   * @param {Uint8Array} o.prologue
   * @param {Keypair} o.s this side's static key
   * @param {Uint8Array | null} [o.rs] the remote static public key (required for the initiator; the responder's own for the pre-message)
   * @param {Uint8Array | null} [o.psk] the 32-byte PSK (IKpsk2)
   * @param {Keypair | null} [o.e] a fixed ephemeral (tests only)
   */
  constructor(o) {
    this.role = o.role;
    const pattern = o.params === PARAMS_PAIRING ? PATTERNS.IKpsk2 : PATTERNS.IK;
    this.hasPsk = o.params === PARAMS_PAIRING;
    this.pattern = pattern;
    this.s = o.s;
    this.rs = o.rs ?? null;
    this.psk = o.psk ?? null;
    this.e = o.e ?? null;
    this.re = null;
    this.turn = 0;
    this.ss = new SymmetricState(o.params);
    this.prologue = o.prologue;
    this.started = this.ss.init().then(async () => {
      await this.ss.mixHash(o.prologue);
      // Pre-message "<- s": the responder's static key is known to the initiator.
      if (o.role === "initiator") {
        if (!this.rs) throw new Error("initiator needs the responder's static key");
        await this.ss.mixHash(this.rs);
      } else {
        await this.ss.mixHash(this.s.publicKey);
      }
    });
    if (this.hasPsk && (!this.psk || this.psk.length !== 32)) throw new Error("IKpsk2 needs a 32-byte psk");
  }

  /** @returns {boolean} */
  get done() {
    return this.turn >= this.pattern.length;
  }

  /** @returns {boolean} whether this side writes the next message */
  get myTurn() {
    return (this.turn % 2 === 0) === (this.role === "initiator");
  }

  /** The DH of two tokens, with the first letter's key belonging to the initiator. */
  /**
   * @param {"es" | "ee" | "se" | "ss"} token
   * @returns {Promise<Uint8Array>}
   */
  async dhFor(token) {
    const init = this.role === "initiator";
    if (token === "ee") {
      if (!this.e || !this.re) throw new Error("ee before both ephemerals");
      return dh(this.e.privateKey, this.re);
    }
    if (token === "ss") {
      if (!this.rs) throw new Error("ss without the remote static");
      return dh(this.s.privateKey, this.rs);
    }
    if (token === "es") {
      // initiator's e with responder's s; responder's s with initiator's e
      if (init) {
        if (!this.e || !this.rs) throw new Error("es without keys");
        return dh(this.e.privateKey, this.rs);
      }
      if (!this.re) throw new Error("es without the initiator's e");
      return dh(this.s.privateKey, this.re);
    }
    // "se": initiator's s with responder's e
    if (init) {
      if (!this.re) throw new Error("se without the responder's e");
      return dh(this.s.privateKey, this.re);
    }
    if (!this.e) throw new Error("se without the responder's e");
    return dh(this.e.privateKey, this.rs ?? new Uint8Array(32));
  }

  /**
   * Writes the next handshake message with `payload`.
   * @param {Uint8Array} [payload]
   * @returns {Promise<Uint8Array>}
   */
  async write(payload = EMPTY) {
    await this.started;
    if (this.done || !this.myTurn) throw new Error("not this side's turn");
    /** @type {Uint8Array[]} */
    const out = [];
    for (const token of this.pattern[this.turn]) {
      if (token === "e") {
        this.e = this.e ?? (await generateKeypair());
        out.push(this.e.publicKey);
        await this.ss.mixHash(this.e.publicKey);
        if (this.hasPsk) await this.ss.mixKey(this.e.publicKey);
      } else if (token === "s") {
        out.push(await this.ss.encryptAndHash(this.s.publicKey));
      } else if (token === "psk") {
        if (!this.psk) throw new Error("psk missing");
        await this.ss.mixKeyAndHash(this.psk);
      } else {
        await this.ss.mixKey(await this.dhFor(/** @type {"es" | "ee" | "se" | "ss"} */ (token)));
      }
    }
    out.push(await this.ss.encryptAndHash(payload));
    this.turn += 1;
    return concat(...out);
  }

  /**
   * Reads the next handshake message. Returns its payload. Throws on any malformed or unauthentic input.
   * @param {Uint8Array} message
   * @returns {Promise<Uint8Array>}
   */
  async read(message) {
    await this.started;
    if (this.done || this.myTurn) throw new Error("not the other side's turn");
    let at = 0;
    /**
     * @param {number} n
     * @returns {Uint8Array}
     */
    const take = (n) => {
      if (at + n > message.length) throw new Error("handshake message too short");
      const piece = message.subarray(at, at + n);
      at += n;
      return piece;
    };
    for (const token of this.pattern[this.turn]) {
      if (token === "e") {
        const re = take(KEY_LEN);
        this.re = re.slice();
        await this.ss.mixHash(this.re);
        if (this.hasPsk) await this.ss.mixKey(this.re);
      } else if (token === "s") {
        const rs = await this.ss.decryptAndHash(take(KEY_LEN + TAG_LEN));
        this.rs = rs;
      } else if (token === "psk") {
        if (!this.psk) throw new Error("psk missing");
        await this.ss.mixKeyAndHash(this.psk);
      } else {
        await this.ss.mixKey(await this.dhFor(/** @type {"es" | "ee" | "se" | "ss"} */ (token)));
      }
    }
    const payload = await this.ss.decryptAndHash(message.subarray(at));
    this.turn += 1;
    return payload;
  }

  /**
   * After the last message: the transport ciphers (the initiator sends with the first, the
   * responder with the second) and the handshake hash, from which the check code is derived.
   * @returns {Promise<{ send: Transport, recv: Transport, hash: Uint8Array, remoteStatic: Uint8Array | null }>}
   */
  async finish() {
    if (!this.done) throw new Error("handshake not finished");
    const [c1, c2] = await this.ss.split();
    const init = this.role === "initiator";
    const sendCipher = init ? c1 : c2;
    const recvCipher = init ? c2 : c1;
    return {
      send: new Transport(sendCipher),
      recv: new Transport(recvCipher),
      hash: this.ss.h.slice(),
      remoteStatic: this.rs,
    };
  }
}

/**
 * One direction of transport. Counts messages and rekeys after every REKEY_INTERVAL, as the sender
 * and the receiver both do (csm-remote-proto `noise.rs`).
 */
export class Transport {
  /**
   * @param {CipherState} cipher
   * @param {number} [rekeyEvery]
   */
  constructor(cipher, rekeyEvery = REKEY_INTERVAL) {
    this.cipher = cipher;
    this.count = 0;
    this.rekeyEvery = rekeyEvery;
    /** Calls run one after another: a rekey must land between message N and message N+1. @type {Promise<unknown>} */
    this.queue = Promise.resolve();
  }

  /**
   * @template T
   * @param {() => Promise<T>} job
   * @returns {Promise<T>}
   */
  serial(job) {
    const run = this.queue.then(job);
    this.queue = run.catch(() => undefined);
    return run;
  }

  /**
   * @param {Uint8Array} plaintext
   * @returns {Promise<Uint8Array>}
   */
  seal(plaintext) {
    return this.serial(async () => {
      const out = await this.cipher.encrypt(EMPTY, plaintext);
      this.count += 1;
      if (this.count % this.rekeyEvery === 0) await this.cipher.rekey();
      return out;
    });
  }

  /**
   * @param {Uint8Array} ciphertext
   * @returns {Promise<Uint8Array>}
   */
  open(ciphertext) {
    return this.serial(async () => {
      const out = await this.cipher.decrypt(EMPTY, ciphertext);
      this.count += 1;
      if (this.count % this.rekeyEvery === 0) await this.cipher.rekey();
      return out;
    });
  }
}
