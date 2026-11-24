// @ts-check
// WebCrypto wrappers for the Noise layer: SHA-256, HMAC, the Noise HKDF, AES-256-GCM and X25519.
// Nothing here keeps state. Private keys are CryptoKeys created non-extractable wherever the
// browser allows it, so the page can use them for DH but never read their bytes.

import { concat } from "./bytes.js";

/** @type {SubtleCrypto} */
const subtle = globalThis.crypto.subtle;

/** The X25519 base point (u = 9), little endian. X25519(k, 9) is the public key of k (RFC 7748). */
const BASEPOINT = Uint8Array.from([9, ...new Array(31).fill(0)]);

/** PKCS#8 wrapper of a raw X25519 private key (RFC 8410). */
const PKCS8_X25519_PREFIX = Uint8Array.from([0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x6e, 0x04, 0x22, 0x04, 0x20]);

/**
 * @typedef {{ privateKey: CryptoKey, publicKey: Uint8Array }} Keypair
 */

/**
 * @param {Uint8Array} data
 * @returns {Promise<Uint8Array>}
 */
export async function sha256(data) {
  return new Uint8Array(await subtle.digest("SHA-256", /** @type {BufferSource} */ (data)));
}

/**
 * @param {Uint8Array} key
 * @param {Uint8Array} data
 * @returns {Promise<Uint8Array>}
 */
export async function hmacSha256(key, data) {
  const k = await subtle.importKey("raw", /** @type {BufferSource} */ (key), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  return new Uint8Array(await subtle.sign("HMAC", k, /** @type {BufferSource} */ (data)));
}

/**
 * The Noise HKDF (Noise spec 4.3): `n` outputs of 32 bytes each, from chaining key `ck` and input
 * key material `ikm`. `n` is 2 or 3.
 * @param {Uint8Array} ck
 * @param {Uint8Array} ikm
 * @param {number} n
 * @returns {Promise<Uint8Array[]>}
 */
export async function hkdf(ck, ikm, n) {
  const temp = await hmacSha256(ck, ikm);
  /** @type {Uint8Array[]} */
  const out = [];
  /** @type {Uint8Array} */
  let prev = new Uint8Array(0);
  for (let i = 1; i <= n; i++) {
    prev = await hmacSha256(temp, concat(prev, Uint8Array.of(i)));
    out.push(prev);
  }
  return out;
}

/**
 * AES-256-GCM with a 12-byte nonce and a 16-byte tag appended (Noise's AESGCM).
 * @param {Uint8Array} key
 * @param {Uint8Array} nonce
 * @param {Uint8Array} ad
 * @param {Uint8Array} plaintext
 * @param {"encrypt" | "decrypt"} mode
 * @returns {Promise<Uint8Array>}
 */
export async function aead(key, nonce, ad, plaintext, mode) {
  const k = await subtle.importKey("raw", /** @type {BufferSource} */ (key), { name: "AES-GCM" }, false, [mode]);
  const params = { name: "AES-GCM", iv: /** @type {BufferSource} */ (nonce), additionalData: /** @type {BufferSource} */ (ad), tagLength: 128 };
  const data = /** @type {BufferSource} */ (plaintext);
  return new Uint8Array(mode === "encrypt" ? await subtle.encrypt(params, k, data) : await subtle.decrypt(params, k, data));
}

/**
 * Generates an X25519 identity or handshake key. The private half is not extractable.
 * @returns {Promise<Keypair>}
 */
export async function generateKeypair() {
  const pair = /** @type {CryptoKeyPair} */ (await subtle.generateKey({ name: "X25519" }, false, ["deriveBits"]));
  const raw = await subtle.exportKey("raw", pair.publicKey);
  return { privateKey: pair.privateKey, publicKey: new Uint8Array(raw) };
}

/**
 * The public key of a private key: X25519(k, 9).
 * @param {CryptoKey} privateKey
 * @returns {Promise<Uint8Array>}
 */
export async function publicOf(privateKey) {
  const base = await subtle.importKey("raw", BASEPOINT, { name: "X25519" }, true, []);
  return new Uint8Array(await subtle.deriveBits({ name: "X25519", public: base }, privateKey, 256));
}

/**
 * A keypair from a known 32-byte private key. For the test vectors only: the app never has one.
 * @param {Uint8Array} seed
 * @returns {Promise<Keypair>}
 */
export async function keypairFromSeed(seed) {
  if (seed.length !== 32) throw new Error("seed must be 32 bytes");
  const privateKey = await subtle.importKey("pkcs8", /** @type {BufferSource} */ (concat(PKCS8_X25519_PREFIX, seed)), { name: "X25519" }, false, ["deriveBits"]);
  return { privateKey, publicKey: await publicOf(privateKey) };
}

/**
 * X25519 DH. Refuses the all-zero output (a low-order peer key), which the Noise spec requires and
 * which WebCrypto does not check on every browser.
 * @param {CryptoKey} privateKey
 * @param {Uint8Array} peerPublic
 * @returns {Promise<Uint8Array>}
 */
export async function dh(privateKey, peerPublic) {
  if (peerPublic.length !== 32) throw new Error("bad public key length");
  const peer = await subtle.importKey("raw", /** @type {BufferSource} */ (peerPublic), { name: "X25519" }, true, []);
  const out = new Uint8Array(await subtle.deriveBits({ name: "X25519", public: peer }, privateKey, 256));
  if (out.every((b) => b === 0)) throw new Error("low-order public key");
  return out;
}
