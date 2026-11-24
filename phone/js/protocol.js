// @ts-check
// The phone's side of csm-remote-proto `msg.rs` and `codes.rs`: the fixed op set, the server's
// responses and events (checked field by field, never trusted), the refusal codes, the plain
// messages shown for each, and the "needs you" grouping. JSON shapes follow msg.rs exactly.

/** Every op name the phone may send (msg.rs `Op::NAMES`). */
export const OP_NAMES = [
  "hello", "ping", "snapshot", "session.list", "session.get", "inbox.list", "task.list", "task.get",
  "folder.list", "profile.list", "command.list", "session.wake", "session.sleep", "session.restart",
  "session.close", "session.new", "session.command", "session.status_clear", "push.subscribe",
  "push.unsubscribe", "settings.set",
];

/** The ops that need an operator (msg.rs `Op::required_level`); the rest need a viewer. */
const OPERATOR_OPS = [
  "session.wake", "session.sleep", "session.restart", "session.close", "session.new", "session.command", "session.status_clear",
];

/**
 * The least level that may run an op. The UI hides what this forbids; the PC refuses it anyway.
 * @param {string} op
 * @returns {Level}
 */
export function requiredLevel(op) {
  return OPERATOR_OPS.includes(op) ? "operator" : "viewer";
}

/** Largest app message in either direction (msg.rs `MAX_APP_MESSAGE_BYTES`). */
export const MAX_APP_MESSAGE_BYTES = 49152;

/** The protocol version the phone speaks. */
export const PROTOCOL = 1;

/** @typedef {"viewer" | "operator"} Level */

/**
 * The statuses a phone shows (msg.rs `PhoneStatus`).
 * @typedef {"working" | "needs_approval" | "needs_question" | "idle" | "done" | "asleep" | "exited"} PhoneStatus
 */

/**
 * @typedef {object} SessionInfo
 * @property {string} id
 * @property {string} name
 * @property {string} folder
 * @property {PhoneStatus} status
 * @property {string} [branch]
 * @property {string} [message]
 * @property {string} [group]
 * @property {string} [profile]
 * @property {number} [since]
 */

/** Refusal codes by number (codes.rs; the WebSocket close code is 4000 + number). */
export const REFUSAL_NAMES = /** @type {const} */ ([
  "bad_request",
  "unsupported_version",
  "not_allowed",
  "not_enrolled",
  "host_offline",
  "rate_limited",
  "relay_full",
  "frame_too_large",
  "idle_timeout",
  "revoked",
  "host_replaced",
  "push_refused",
  "push_failed",
]);

/**
 * @param {number} closeCode
 * @returns {string | null}
 */
export function refusalFromCloseCode(closeCode) {
  const n = closeCode - 4000;
  return n >= 1 && n <= REFUSAL_NAMES.length ? REFUSAL_NAMES[n - 1] : null;
}

/** The app-level error codes (msg.rs `ErrorCode`). */
export const ERROR_CODES = /** @type {const} */ ([
  "bad_request",
  "unknown_op",
  "too_large",
  "forbidden",
  "unknown_session",
  "unknown_task",
  "folder_not_allowed",
  "unknown_profile",
  "command_not_allowed",
  "busy",
  "failed",
  "unavailable",
  "rate_limited",
]);

/**
 * The plain message for each code, shown as is (design 4 and 10.5). Never CSM's raw text.
 * @type {Record<string, string>}
 */
export const MESSAGES = {
  bad_request: "The PC did not understand that request.",
  unknown_op: "This version of the phone cannot do that.",
  too_large: "That request is too large.",
  forbidden: "Your phone is paired as a viewer. Ask the person at the PC to change it to operator.",
  unknown_session: "That session is gone.",
  unknown_task: "That task is gone.",
  folder_not_allowed: "That folder is not on the PC's list for phones.",
  unknown_profile: "That profile is not set up on the PC.",
  command_not_allowed: "That command is not allowed from a phone.",
  busy: "This session is busy.",
  failed: "The PC could not do that.",
  unavailable: "The PC is not answering right now.",
  rate_limited: "Too many requests. Wait a moment and try again.",
  not_allowed: "The PC refused the connection. The phone may have been revoked, or the link has expired.",
  host_offline: "The PC is offline. It will show up when it is back on.",
  revoked: "The PC has removed this phone. Pair it again if you want to use it.",
  not_enrolled: "This relay does not accept that PC.",
  relay_full: "The relay is full right now. Try again later.",
  frame_too_large: "The connection sent something too large.",
  idle_timeout: "The connection was idle for too long.",
  host_replaced: "The PC connected again from somewhere else.",
  unsupported_version: "This phone app is too old or too new for the relay. Reload the app.",
  level_changed: "The PC changed this phone's level. The phone reconnects.",
  shutdown: "The PC is closing the connection. It will come back when the PC is on.",
  pairing_busy:
    "Pairing is in use by another phone, or this code has expired or been used. If another phone is pairing, check the PC and press Deny there.",
};

/**
 * The text for a code. `pairing` picks the pairing wording for `not_allowed`.
 * @param {string} code
 * @param {{ pairing?: boolean }} [opts]
 * @returns {string}
 */
export function messageFor(code, opts = {}) {
  if (opts.pairing && code === "not_allowed") return MESSAGES.pairing_busy;
  return MESSAGES[code] ?? "Something went wrong. Try again.";
}

/**
 * @param {unknown} v
 * @returns {v is Record<string, unknown>}
 */
function isObject(v) {
  return typeof v === "object" && v !== null && !Array.isArray(v);
}

/**
 * @param {unknown} v
 * @param {number} max
 * @returns {v is string}
 */
export function cleanText(v, max) {
  if (typeof v !== "string") return false;
  const n = [...v].length;
  return n >= 1 && n <= max && !/[\u0000-\u001f\u007f]/.test(v);
}

/**
 * Turns a request into its JSON text. Refuses (returns null) anything over the app cap.
 * @param {number} id
 * @param {string} op
 * @param {Record<string, unknown>} [args]
 * @returns {string | null}
 */
export function encodeRequest(id, op, args) {
  const text = JSON.stringify(args && Object.keys(args).length > 0 ? { id, op, args } : { id, op });
  return new TextEncoder().encode(text).length > MAX_APP_MESSAGE_BYTES ? null : text;
}

/**
 * Checks a session object as msg.rs `SessionInfo` (the optional fields may be absent).
 * @param {unknown} v
 * @returns {SessionInfo | null}
 */
export function parseSession(v) {
  if (!isObject(v)) return null;
  const { id, name, folder, status } = v;
  if (!cleanText(id, 64) || typeof name !== "string" || typeof folder !== "string") return null;
  if (typeof status !== "string" || !STATUSES.includes(/** @type {PhoneStatus} */ (status))) return null;
  /** @type {SessionInfo} */
  const out = { id, name, folder, status: /** @type {PhoneStatus} */ (status) };
  if (v.branch !== undefined) {
    if (typeof v.branch !== "string") return null;
    out.branch = v.branch;
  }
  if (v.message !== undefined) {
    if (typeof v.message !== "string") return null;
    out.message = v.message;
  }
  if (v.group !== undefined) {
    if (typeof v.group !== "string") return null;
    out.group = v.group;
  }
  if (v.profile !== undefined) {
    if (typeof v.profile !== "string") return null;
    out.profile = v.profile;
  }
  if (v.since !== undefined) {
    if (typeof v.since !== "number" || !Number.isSafeInteger(v.since) || v.since < 0) return null;
    out.since = v.since;
  }
  return out;
}

/** @type {PhoneStatus[]} */
export const STATUSES = ["working", "needs_approval", "needs_question", "idle", "done", "asleep", "exited"];

/**
 * The group a status is listed under (design 10.5, in this order).
 * @param {PhoneStatus} status
 * @returns {"needs" | "working" | "idle" | "asleep" | "exited"}
 */
export function groupOf(status) {
  if (status === "needs_approval" || status === "needs_question") return "needs";
  if (status === "working") return "working";
  if (status === "asleep") return "asleep";
  if (status === "exited") return "exited";
  return "idle";
}

/**
 * @param {PhoneStatus} status
 * @returns {string}
 */
export function statusWord(status) {
  switch (status) {
    case "needs_approval":
      return "Needs approval";
    case "needs_question":
      return "Has a question";
    case "working":
      return "Working";
    case "done":
      return "Done";
    case "asleep":
      return "Asleep";
    case "exited":
      return "Exited";
    default:
      return "Idle";
  }
}

/**
 * A message from the host: a response to a request, or an event.
 * @typedef {{ kind: "response", id: number, ok: true, data: any } | { kind: "response", id: number, ok: false, code: string, message: string } | { kind: "event", ev: "snapshot", seq: number, part: number, parts: number, sessions: SessionInfo[], unread?: number } | { kind: "event", ev: "delta", seq: number, upserts: SessionInfo[], removed: string[] } | { kind: "event", ev: "inbox", seq: number, unread: number } | { kind: "event", ev: "bye", reason: "revoked" | "level_changed" | "shutdown" } | { kind: "event", ev: "remote_control", session: string, ok: boolean, code?: string, message?: string }} ServerMsg
 */

/**
 * Parses one decrypted message from the host. Returns null for anything that does not match msg.rs
 * exactly; the caller then ignores it (and resynchronises if a sequence was involved).
 * @param {Uint8Array} bytes
 * @returns {ServerMsg | null}
 */
export function parseServerMsg(bytes) {
  if (bytes.length > MAX_APP_MESSAGE_BYTES) return null;
  let v;
  try {
    v = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
  } catch {
    return null;
  }
  if (!isObject(v)) return null;
  if ("ev" in v) return parseEvent(v);
  if ("ok" in v) return parseResponse(v);
  return null;
}

/**
 * @param {Record<string, unknown>} v
 * @returns {ServerMsg | null}
 */
function parseResponse(v) {
  if (typeof v.id !== "number" || !Number.isSafeInteger(v.id) || v.id < 0) return null;
  if (v.ok === true) return { kind: "response", id: v.id, ok: true, data: v.data ?? null };
  if (v.ok === false && isObject(v.error) && typeof v.error.code === "string") {
    const message = typeof v.error.message === "string" ? v.error.message : "";
    return { kind: "response", id: v.id, ok: false, code: v.error.code, message };
  }
  return null;
}

/**
 * @param {Record<string, unknown>} v
 * @returns {ServerMsg | null}
 */
function parseEvent(v) {
  const seq = v.seq;
  switch (v.ev) {
    case "snapshot": {
      if (!validSeq(seq) || !validPart(v.part, v.parts) || !Array.isArray(v.sessions)) return null;
      const sessions = parseSessions(v.sessions);
      if (!sessions) return null;
      const unread = v.unread === undefined ? undefined : validCount(v.unread);
      if (v.unread !== undefined && unread === undefined) return null;
      return { kind: "event", ev: "snapshot", seq, part: /** @type {number} */ (v.part), parts: /** @type {number} */ (v.parts), sessions, unread };
    }
    case "delta": {
      if (!validSeq(seq) || !Array.isArray(v.upserts) || !Array.isArray(v.removed)) return null;
      const upserts = parseSessions(v.upserts);
      if (!upserts) return null;
      if (!v.removed.every((r) => cleanText(r, 64))) return null;
      return { kind: "event", ev: "delta", seq, upserts, removed: /** @type {string[]} */ (v.removed) };
    }
    case "inbox": {
      const unread = validCount(v.unread);
      if (!validSeq(seq) || unread === undefined) return null;
      return { kind: "event", ev: "inbox", seq, unread };
    }
    case "bye":
      if (v.reason === "revoked" || v.reason === "level_changed" || v.reason === "shutdown") return { kind: "event", ev: "bye", reason: v.reason };
      return null;
    case "remote_control": {
      if (!cleanText(v.session, 64) || typeof v.ok !== "boolean") return null;
      if (v.ok) return { kind: "event", ev: "remote_control", session: v.session, ok: true };
      if (typeof v.code !== "string") return null;
      const message = typeof v.message === "string" ? v.message : "";
      return { kind: "event", ev: "remote_control", session: v.session, ok: false, code: v.code, message };
    }
    default:
      return null;
  }
}

/**
 * @param {unknown} v
 * @returns {v is number}
 */
function validSeq(v) {
  return typeof v === "number" && Number.isSafeInteger(v) && v >= 0;
}

/**
 * @param {unknown} part
 * @param {unknown} parts
 * @returns {boolean}
 */
function validPart(part, parts) {
  return typeof part === "number" && typeof parts === "number" && Number.isSafeInteger(part) && Number.isSafeInteger(parts) && part >= 1 && parts >= part;
}

/**
 * @param {unknown} v
 * @returns {number | undefined}
 */
function validCount(v) {
  return typeof v === "number" && Number.isSafeInteger(v) && v >= 0 && v <= 1_000_000 ? v : undefined;
}

/**
 * @param {unknown[]} list
 * @returns {SessionInfo[] | null}
 */
function parseSessions(list) {
  const out = [];
  for (const item of list) {
    const s = parseSession(item);
    if (!s) return null;
    out.push(s);
  }
  return out;
}

/**
 * The pairing reply, as the phone reads it from the encrypted channel (pairing.rs `PairMsg`).
 * @typedef {{ t: "accept", routing_token: string, level: Level, device_id: string } | { t: "deny", reason: "denied" | "expired" | "bad_request" }} PairReply
 */

/**
 * @param {Uint8Array} bytes
 * @returns {PairReply | null}
 */
export function parsePairReply(bytes) {
  let v;
  try {
    v = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
  } catch {
    return null;
  }
  if (!isObject(v)) return null;
  if (v.t === "accept" && typeof v.routing_token === "string" && (v.level === "viewer" || v.level === "operator") && cleanText(v.device_id, 64)) {
    return { t: "accept", routing_token: v.routing_token, level: v.level, device_id: v.device_id };
  }
  if (v.t === "deny" && (v.reason === "denied" || v.reason === "expired" || v.reason === "bad_request")) {
    return { t: "deny", reason: v.reason };
  }
  return null;
}

/**
 * @typedef {object} HelloData
 * @property {number} protocol
 * @property {Level} level
 * @property {string} host_name
 * @property {boolean} hide_names
 * @property {boolean} notify
 * @property {boolean} push_ready
 * @property {boolean} remote_control
 * @property {number} max_message_bytes
 */

/**
 * The reply to `hello` (msg.rs `HelloData`). Every field is required.
 * @param {unknown} v
 * @returns {HelloData | null}
 */
export function parseHelloData(v) {
  if (!isObject(v)) return null;
  const { protocol, level, host_name, hide_names, notify, push_ready, remote_control, max_message_bytes } = v;
  if (typeof protocol !== "number" || (level !== "viewer" && level !== "operator") || typeof host_name !== "string") return null;
  if (typeof hide_names !== "boolean" || typeof notify !== "boolean" || typeof push_ready !== "boolean" || typeof remote_control !== "boolean") return null;
  if (typeof max_message_bytes !== "number" || max_message_bytes <= 0 || max_message_bytes > MAX_APP_MESSAGE_BYTES) return null;
  return { protocol, level, host_name, hide_names, notify, push_ready, remote_control, max_message_bytes };
}

/**
 * The reply to `session.new` (the session's id and the remote-control answer).
 * @param {unknown} v
 * @returns {{ id: string, remote_control: string } | null}
 */
export function parseNewSession(v) {
  if (!isObject(v) || !cleanText(v.id, 64)) return null;
  return { id: v.id, remote_control: typeof v.remote_control === "string" ? v.remote_control : "off" };
}

/**
 * The list replies (`session.list`, `folder.list`, `profile.list`, `command.list`, `inbox.list`).
 * Each takes the array under its key; an entry that does not match is dropped, not guessed at.
 * @template T
 * @param {unknown} data
 * @param {string} key
 * @param {(v: unknown) => T | null} parse
 * @returns {T[] | null}
 */
export function parseList(data, key, parse) {
  if (!isObject(data) || !Array.isArray(data[key])) return null;
  const out = [];
  for (const item of data[key]) {
    const v = parse(item);
    if (v !== null) out.push(v);
  }
  return out;
}

/**
 * @param {unknown} v
 * @returns {{ path: string, name: string, pinned: boolean } | null}
 */
export function parseFolder(v) {
  if (!isObject(v) || typeof v.path !== "string" || typeof v.name !== "string" || typeof v.pinned !== "boolean") return null;
  return { path: v.path, name: v.name, pinned: v.pinned };
}

/**
 * @param {unknown} v
 * @returns {{ name: string, label: string } | null}
 */
export function parseProfile(v) {
  if (!isObject(v) || typeof v.name !== "string" || typeof v.label !== "string") return null;
  return { name: v.name, label: v.label };
}

/**
 * @param {unknown} v
 * @returns {{ command: string, label: string } | null}
 */
export function parseCommand(v) {
  if (!isObject(v) || typeof v.command !== "string" || typeof v.label !== "string") return null;
  return { command: v.command, label: v.label };
}

/**
 * @param {unknown} v
 * @returns {{ id: string, title: string, session_id: string | null } | null}
 */
export function parseInboxItem(v) {
  if (!isObject(v) || !cleanText(v.id, 64) || typeof v.title !== "string") return null;
  const session = typeof v.session_id === "string" ? v.session_id : null;
  return { id: v.id, title: v.title, session_id: session };
}
