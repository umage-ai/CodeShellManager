// @ts-check
// Web Push subscription (design section 9). The relay holds the VAPID key pair and publishes the
// public half at `GET /v1/vapid` (relay/README.md). The phone subscribes with it and gives the PC the
// endpoint and keys through `push.subscribe`, over the encrypted channel. The PC checks the endpoint
// host against its own list before the relay sees it.

import { b64urlDecode, b64urlEncode } from "./bytes.js";

/**
 * The HTTPS base of a relay's WebSocket base (`wss://host` to `https://host`).
 * @param {string} wsBase
 * @returns {string}
 */
export function httpBaseOf(wsBase) {
  if (!wsBase.startsWith("wss://")) throw new Error("push needs a wss:// relay");
  return `https://${wsBase.slice("wss://".length)}`;
}

/**
 * The relay's VAPID public key (65 bytes, uncompressed P-256). Refuses any other shape.
 * @param {string} wsBase
 * @param {typeof fetch} [fetchImpl]
 * @returns {Promise<Uint8Array>}
 */
export async function fetchVapidKey(wsBase, fetchImpl = globalThis.fetch) {
  const res = await fetchImpl(`${httpBaseOf(wsBase)}/v1/vapid`, { credentials: "omit", cache: "no-store", redirect: "error" });
  if (!res.ok) throw new Error("the relay has no push key");
  /** @type {unknown} */
  const body = await res.json();
  const key = typeof body === "object" && body !== null ? /** @type {{ public_key?: unknown }} */ (body).public_key : undefined;
  const bytes = typeof key === "string" ? b64urlDecode(key) : null;
  if (!bytes || bytes.length !== 65 || bytes[0] !== 4) throw new Error("the relay sent a bad push key");
  return bytes;
}

/**
 * Asks the browser for permission and subscribes this page's service worker.
 * @param {Uint8Array} vapidKey
 * @param {ServiceWorkerRegistration} registration
 * @returns {Promise<{ endpoint: string, p256dh: string, auth: string }>}
 */
export async function subscribe(vapidKey, registration) {
  const permission = await Notification.requestPermission();
  if (permission !== "granted") throw new Error("notifications are not allowed in this browser");
  const sub = await registration.pushManager.subscribe({ userVisibleOnly: true, applicationServerKey: /** @type {BufferSource} */ (vapidKey) });
  const p256dh = sub.getKey("p256dh");
  const auth = sub.getKey("auth");
  if (!p256dh || !auth) throw new Error("the browser gave no push keys");
  return { endpoint: sub.endpoint, p256dh: b64urlEncode(new Uint8Array(p256dh)), auth: b64urlEncode(new Uint8Array(auth)) };
}

/**
 * Drops this page's push subscription (the PC is told by `push.unsubscribe`).
 * @param {ServiceWorkerRegistration} registration
 */
export async function unsubscribe(registration) {
  const sub = await registration.pushManager.getSubscription();
  if (sub) await sub.unsubscribe();
}
