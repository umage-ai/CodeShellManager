// @ts-check
// IndexedDB: the phone's identity key and its paired PCs (design section 10.2). Nothing goes to
// localStorage. The identity's private half is a non-extractable CryptoKey, which IndexedDB stores
// as an object (structured clone keeps it non-extractable). Every call can fail (private windows,
// blocked storage, a browser without IndexedDB), and a failure is reported as one plain error.

/** @typedef {import("./crypto.js").Keypair} Keypair */

export const DB_NAME = "csm-phone";
const DB_VERSION = 1;

export class StoreError extends Error {
  constructor() {
    super("This browser cannot keep a pairing. Use a current browser with IndexedDB turned on.");
    this.name = "StoreError";
  }
}

/**
 * A paired PC, as the phone keeps it. Keys are bytes; nothing here is a secret the PC cannot
 * regenerate except the routing token, which is treated as one all the same.
 * @typedef {object} Device
 * @property {string} host_id
 * @property {string} relay the relay base URL (`wss://...`)
 * @property {Uint8Array} host_pub the PC's static public key, pinned from the QR code
 * @property {Uint8Array} routing_token
 * @property {string} device_id the id the PC gave this phone
 * @property {string} name the name this phone was paired with
 * @property {"viewer" | "operator"} level the level as last seen
 * @property {number} paired_at Unix seconds
 */

/** @type {Promise<IDBDatabase> | null} */
let dbPromise = null;

/**
 * @param {IDBFactory} [factory]
 * @returns {Promise<IDBDatabase>}
 */
export function openDb(factory = globalThis.indexedDB) {
  if (!dbPromise) {
    dbPromise = new Promise((resolve, reject) => {
      let req;
      try {
        req = factory.open(DB_NAME, DB_VERSION);
      } catch {
        reject(new StoreError());
        return;
      }
      req.onupgradeneeded = () => {
        const db = req.result;
        if (!db.objectStoreNames.contains("kv")) db.createObjectStore("kv");
        if (!db.objectStoreNames.contains("devices")) db.createObjectStore("devices", { keyPath: "host_id" });
      };
      req.onsuccess = () => resolve(req.result);
      req.onerror = () => reject(new StoreError());
      req.onblocked = () => reject(new StoreError());
    }).catch((e) => {
      dbPromise = null;
      throw e;
    });
  }
  return dbPromise;
}

/**
 * Runs one request on a store and resolves with its result.
 * @template T
 * @param {string} name
 * @param {IDBTransactionMode} mode
 * @param {(s: IDBObjectStore) => IDBRequest<T>} op
 * @returns {Promise<T>}
 */
async function run(name, mode, op) {
  let db;
  try {
    db = await openDb();
  } catch {
    throw new StoreError();
  }
  return new Promise((resolve, reject) => {
    /** @type {IDBRequest<T> | undefined} */
    let req;
    try {
      const tx = db.transaction(name, mode);
      const r = op(tx.objectStore(name));
      req = r;
      r.onsuccess = () => resolve(r.result);
      r.onerror = () => reject(new StoreError());
    } catch {
      reject(new StoreError());
    }
  });
}

/**
 * The phone's identity key pair, or null on first run.
 * @returns {Promise<Keypair | null>}
 */
export async function getIdentity() {
  const v = await run("kv", "readonly", (s) => s.get("identity"));
  if (!v || typeof v !== "object") return null;
  const o = /** @type {{ privateKey?: unknown, publicKey?: unknown }} */ (v);
  if (!(o.publicKey instanceof Uint8Array) || o.publicKey.length !== 32 || !o.privateKey) return null;
  return { privateKey: /** @type {CryptoKey} */ (o.privateKey), publicKey: o.publicKey };
}

/**
 * @param {Keypair} identity
 */
export async function saveIdentity(identity) {
  await run("kv", "readwrite", (s) => s.put({ privateKey: identity.privateKey, publicKey: identity.publicKey }, "identity"));
}

/**
 * @returns {Promise<Device[]>}
 */
export async function listDevices() {
  const rows = await run("devices", "readonly", (s) => s.getAll());
  return Array.isArray(rows) ? rows.filter(isDevice) : [];
}

/**
 * @param {unknown} v
 * @returns {v is Device}
 */
function isDevice(v) {
  if (typeof v !== "object" || v === null) return false;
  const d = /** @type {Record<string, unknown>} */ (v);
  return (
    typeof d.host_id === "string" &&
    typeof d.relay === "string" &&
    d.host_pub instanceof Uint8Array &&
    d.routing_token instanceof Uint8Array &&
    typeof d.device_id === "string" &&
    typeof d.name === "string" &&
    (d.level === "viewer" || d.level === "operator") &&
    typeof d.paired_at === "number"
  );
}

/**
 * @param {Device} device
 */
export async function saveDevice(device) {
  await run("devices", "readwrite", (s) => s.put(device));
}

/**
 * Forgets a PC on this phone. The PC keeps listing the phone until it is revoked there.
 * @param {string} hostId
 */
export async function deleteDevice(hostId) {
  await run("devices", "readwrite", (s) => s.delete(hostId));
}
