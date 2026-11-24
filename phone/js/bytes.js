// @ts-check
// Byte helpers: base64url (unpadded, canonical only, as csm-remote-proto's `b64_decode_fixed`),
// hex (for the vectors), concatenation and constant-time comparison.

const B64URL = /^[A-Za-z0-9_-]*$/;

/**
 * @param {Uint8Array} bytes
 * @returns {string}
 */
export function b64urlEncode(bytes) {
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

/**
 * Decodes unpadded base64url. Returns null for anything that is not the canonical spelling.
 * @param {string} text
 * @returns {Uint8Array | null}
 */
export function b64urlDecode(text) {
  if (typeof text !== "string" || !B64URL.test(text) || text.length % 4 === 1) return null;
  const std = text.replace(/-/g, "+").replace(/_/g, "/");
  const padded = std + "=".repeat((4 - (std.length % 4)) % 4);
  let bin;
  try {
    bin = atob(padded);
  } catch {
    return null;
  }
  const out = new Uint8Array(bin.length);
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
  // One value, one spelling: re-encoding must give the input back.
  return b64urlEncode(out) === text ? out : null;
}

/**
 * Decodes unpadded base64url into exactly `n` bytes, or null.
 * @param {unknown} text
 * @param {number} n
 * @returns {Uint8Array | null}
 */
export function b64urlFixed(text, n) {
  if (typeof text !== "string") return null;
  const bytes = b64urlDecode(text);
  return bytes && bytes.length === n ? bytes : null;
}

/**
 * @param {Uint8Array} bytes
 * @returns {string}
 */
export function hex(bytes) {
  let s = "";
  for (const b of bytes) s += b.toString(16).padStart(2, "0");
  return s;
}

/**
 * @param {string} s
 * @returns {Uint8Array}
 */
export function unhex(s) {
  if (s.length % 2 !== 0 || !/^[0-9a-f]*$/i.test(s)) throw new Error("bad hex");
  const out = new Uint8Array(s.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(s.substr(i * 2, 2), 16);
  return out;
}

/**
 * @param {...Uint8Array} parts
 * @returns {Uint8Array}
 */
export function concat(...parts) {
  let n = 0;
  for (const p of parts) n += p.length;
  const out = new Uint8Array(n);
  let at = 0;
  for (const p of parts) {
    out.set(p, at);
    at += p.length;
  }
  return out;
}

/**
 * @param {Uint8Array} a
 * @param {Uint8Array} b
 * @returns {boolean}
 */
export function ctEqual(a, b) {
  if (a.length !== b.length) return false;
  let acc = 0;
  for (let i = 0; i < a.length; i++) acc |= a[i] ^ b[i];
  return acc === 0;
}

/**
 * @param {string} s
 * @returns {Uint8Array}
 */
export function utf8(s) {
  return new TextEncoder().encode(s);
}
