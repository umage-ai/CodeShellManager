// @ts-check
// The phone web app: the controller. It owns the identity key, the paired PCs, the connection to the
// active PC (with the backoff of relay/README.md), and the routes. The screens (ui/*.js) draw from
// `ctx` and call back into it; they never touch storage or the network themselves.
//
// Routes (hash): #/sessions (default), #/session/<id>, #/new, #/settings, #/pair.
// An offer link opens #/pair with the offer read from the fragment, which is then removed from the
// address bar so the PSK is not left in history.

import { el, replace } from "./ui/dom.js";
import { renderPair } from "./ui/pair.js";
import { renderSessions } from "./ui/sessions.js";
import { renderSession, renderNewSession } from "./ui/session.js";
import { renderSettings } from "./ui/settings.js";
import { generateKeypair } from "./crypto.js";
import { ClientError, HostClient } from "./client.js";
import { decodeOffer, PairingError, startPairing } from "./pairing.js";
import { messageFor } from "./protocol.js";
import { RelayError } from "./relay.js";
import { SessionList } from "./delta.js";
import { deleteDevice, getIdentity, listDevices, saveDevice, saveIdentity, StoreError } from "./store.js";
import { fetchVapidKey, subscribe, unsubscribe } from "./push.js";

export const APP_VERSION = "0.1.0";
const BACKOFF_SECS = [1, 2, 5, 10, 30];
/** Refusals that a retry cannot fix: stop reconnecting. */
const FATAL = new Set(["revoked", "not_allowed", "not_enrolled", "unsupported_version", "bad_request"]);

/**
 * @typedef {import("./store.js").Device} Device
 * @typedef {import("./crypto.js").Keypair} Keypair
 * @typedef {import("./protocol.js").HelloData} HelloData
 * @typedef {import("./pairing.js").PairingSession} PairingSession
 * @typedef {{ tone: "ok" | "warn" | "error" | "idle", text: string }} Conn
 * @typedef {{
 *   identity: Keypair | null,
 *   devices: Device[],
 *   activeId: string | null,
 *   client: HostClient | null,
 *   hello: HelloData | null,
 *   conn: Conn,
 *   notice: string,
 *   listAt: number,
 *   retry: number,
 *   retryTimer: ReturnType<typeof setTimeout> | null,
 *   pairing: { stage: "connecting" | "check" | "waiting" | "error", check?: string, error?: string, session?: PairingSession, name: string } | null,
 *   pushOn: boolean,
 *   pendingOffer: string,
 *   lastList: SessionList | null,
 * }} State
 */

/** @type {State} */
const state = {
  identity: null,
  devices: [],
  activeId: null,
  client: null,
  hello: null,
  conn: { tone: "idle", text: "Starting" },
  notice: "",
  listAt: 0,
  retry: 0,
  retryTimer: null,
  pairing: null,
  pushOn: false,
  pendingOffer: "",
  lastList: null,
};

/** Set by the screens' events through `ctx`; read by the router. */
/** @type {ReturnType<typeof makeCtx>} */
let ctx;

/**
 * @returns {string}
 */
function currentRoute() {
  const h = location.hash.replace(/^#/, "");
  return h.length > 0 ? h : "/sessions";
}

/**
 * @param {string} route
 */
function go(route) {
  if (location.hash !== `#${route}`) location.hash = route;
  else render();
}

/**
 * @returns {Device | null}
 */
function activeDevice() {
  return state.devices.find((d) => d.host_id === state.activeId) ?? state.devices[0] ?? null;
}

/**
 * @param {string} text
 * @param {Conn["tone"]} tone
 */
function setConn(text, tone) {
  state.conn = { text, tone };
  render();
}

/**
 * Creates the phone's identity on first run.
 * @returns {Promise<Keypair>}
 */
async function createIdentity() {
  const kp = await generateKeypair();
  await saveIdentity(kp);
  return kp;
}

/**
 * Connects to the active PC. A failure schedules a retry unless the refusal cannot be fixed by one.
 * @param {Device} device
 */
async function connectDevice(device) {
  closeClient();
  if (!state.identity) return;
  setConn("Connecting…", "warn");
  const client = new HostClient({ device, identity: state.identity, pageOrigin: location.origin });
  state.client = client;
  state.lastList = client.list;
  client.onEvent((e) => {
    if (e.kind === "list") {
      state.listAt = Date.now();
      render();
    } else if (e.kind === "event" && e.msg.kind === "event") {
      if (e.msg.ev === "remote_control") state.notice = e.msg.ok ? "Remote Control is on." : messageFor(e.msg.code ?? "failed");
      if (e.msg.ev === "bye" && e.msg.reason === "level_changed") state.notice = messageFor("level_changed");
      render();
    } else if (e.kind === "closed") {
      onClosed(device, client, e.error);
    }
  });
  try {
    const hello = await client.connect();
    if (state.client !== client) return;
    state.hello = hello;
    state.retry = 0;
    state.listAt = state.listAt || Date.now();
    setConn("Connected", "ok");
    if (hello.level !== device.level) {
      const updated = { ...device, level: hello.level };
      await saveDevice(updated).catch(() => undefined);
      state.devices = state.devices.map((d) => (d.host_id === device.host_id ? updated : d));
    }
    state.pushOn = hello.push_ready;
  } catch (e) {
    if (state.client === client) onClosed(device, client, e instanceof Error ? e : new Error("failed"));
  }
}

/**
 * @param {Device} device
 * @param {HostClient} client
 * @param {Error} error
 */
async function onClosed(device, client, error) {
  if (state.client !== client) return;
  state.client = null;
  state.hello = null;
  const raw = /** @type {{ code?: unknown }} */ (error).code;
  const reason = typeof raw === "string" ? raw : "unavailable";
  if (reason === "revoked") {
    await deleteDevice(device.host_id).catch(() => undefined);
    state.devices = state.devices.filter((d) => d.host_id !== device.host_id);
    state.activeId = null;
    state.notice = messageFor("revoked");
    setConn("Not paired", "error");
    go("/pair");
    return;
  }
  if (FATAL.has(reason)) {
    setConn(messageFor(reason), "error");
    return;
  }
  const delay = BACKOFF_SECS[Math.min(state.retry, BACKOFF_SECS.length - 1)];
  state.retry += 1;
  setConn(state.listAt ? `Offline: showing the last list from ${clock(state.listAt)}` : messageFor(reason), "error");
  if (state.retryTimer) clearTimeout(state.retryTimer);
  state.retryTimer = setTimeout(() => {
    state.retryTimer = null;
    const d = activeDevice();
    if (d && !state.client) void connectDevice(d);
  }, delay * 1000);
}

function closeClient() {
  if (state.retryTimer) {
    clearTimeout(state.retryTimer);
    state.retryTimer = null;
  }
  const c = state.client;
  state.client = null;
  state.hello = null;
  c?.close();
}

/**
 * @param {number} ms
 * @returns {string}
 */
function clock(ms) {
  const d = new Date(ms);
  return `${String(d.getHours()).padStart(2, "0")}:${String(d.getMinutes()).padStart(2, "0")}`;
}

/**
 * Starts pairing with the offer the person pasted or scanned.
 * @param {string} text
 * @param {string} name
 */
async function pairStart(text, name) {
  const offer = decodeOffer(text);
  if (!offer) {
    state.pairing = { stage: "error", error: messageFor("bad_request"), name };
    render();
    return;
  }
  state.pairing = { stage: "connecting", name };
  render();
  try {
    if (!state.identity) state.identity = await createIdentity();
    const session = await startPairing(offer, state.identity, { pageOrigin: location.origin });
    state.pairing = { stage: "check", check: session.check, session, name };
  } catch (e) {
    const code = e instanceof PairingError || e instanceof RelayError ? e.code : "bad_request";
    state.pairing = { stage: "error", error: messageFor(code, { pairing: true }), name };
  }
  render();
}

/**
 * The person read the digits on the PC and they match: confirm and wait for the PC.
 */
async function pairConfirm() {
  const p = state.pairing;
  if (!p || p.stage !== "check" || !p.session) return;
  const session = p.session;
  state.pairing = { stage: "waiting", check: p.check, name: p.name, session };
  render();
  try {
    const got = await session.confirm(p.name);
    const device = {
      host_id: got.hostId,
      relay: got.relay,
      host_pub: got.hostPub,
      routing_token: got.routingToken,
      device_id: got.deviceId,
      name: p.name,
      level: got.level,
      paired_at: Math.floor(Date.now() / 1000),
    };
    await saveDevice(device);
    state.devices = [...state.devices.filter((d) => d.host_id !== device.host_id), device];
    state.activeId = device.host_id;
    state.pairing = null;
    state.notice = "Paired. The PC lists this phone.";
    go("/sessions");
    void connectDevice(device);
  } catch (e) {
    const code = e instanceof PairingError ? e.code : "bad_request";
    state.pairing = { stage: "error", error: messageFor(code, { pairing: true }), name: p.name };
    render();
  }
}

/**
 * @param {Device} device
 */
async function forget(device) {
  closeClient();
  await deleteDevice(device.host_id);
  state.devices = state.devices.filter((d) => d.host_id !== device.host_id);
  state.activeId = null;
  state.notice = "This phone is forgotten here. The PC still lists it until it is revoked there.";
  if (state.devices.length > 0) {
    state.activeId = state.devices[0].host_id;
    void connectDevice(state.devices[0]);
    go("/sessions");
  } else {
    setConn("Not paired", "idle");
    go("/pair");
  }
}

/**
 * The calls the screens make. Each one is a request the PC answers, or a local change.
 * @returns {any}
 */
function makeCtx() {
  return {
    state,
    /** @param {string} route */
    go,
    /** Redraws the current screen (the screens keep their own state). */
    refresh() {
      render();
    },
    /** @param {string} text */
    notice(text) {
      state.notice = text;
      render();
    },
    pairStart,
    pairConfirm,
    /** The session list: live, or the last one received while offline. @returns {SessionList | null} */
    list() {
      return state.lastList;
    },
    pairCancel() {
      state.pairing?.session?.cancel();
      state.pairing = null;
      render();
    },
    /** @param {string} hostId */
    selectDevice(hostId) {
      state.activeId = hostId;
      const d = activeDevice();
      closeClient();
      if (d) void connectDevice(d);
      render();
    },
    /**
     * @param {string} op
     * @param {Record<string, unknown>} [args]
     * @returns {Promise<unknown>}
     */
    request(op, args) {
      const c = state.client;
      if (!c) return Promise.reject(new ClientError("unavailable"));
      return c.request(op, args);
    },
    /** @param {Device} device */
    forget,
    /** @param {Device} device */
    reconnect(device) {
      void connectDevice(device);
    },
    /**
     * @param {boolean} on
     */
    async setPush(on) {
      const device = activeDevice();
      if (!device) return;
      const reg = await navigator.serviceWorker?.ready;
      if (!reg) throw new Error("this browser cannot show notifications");
      if (on) {
        const key = await fetchVapidKey(device.relay);
        const sub = await subscribe(key, reg);
        await ctx.request("push.subscribe", sub);
      } else {
        await ctx.request("push.unsubscribe").catch(() => undefined);
        await unsubscribe(reg);
      }
      state.pushOn = on;
      render();
    },
    /**
     * @param {{ hide_names: boolean, notify: boolean }} prefs
     */
    async setPrefs(prefs) {
      await ctx.request("settings.set", { ...prefs });
      if (state.hello) state.hello = { ...state.hello, hide_names: prefs.hide_names, notify: prefs.notify };
      render();
    },
  };
}

/**
 * Draws the current route.
 */
function render() {
  const root = document.getElementById("view");
  if (!root) return;
  const route = currentRoute();
  const device = activeDevice();
  const paired = state.devices.length > 0;
  const tabs = document.getElementById("tabs");
  if (tabs) tabs.hidden = !paired;
  const conn = document.getElementById("conn");
  if (conn) {
    conn.textContent = paired ? state.conn.text : "";
    conn.dataset.tone = state.conn.tone;
  }
  const title = document.getElementById("pc-name");
  if (title) title.textContent = device ? (state.hello?.host_name ?? device.name) : "";
  /** @type {HTMLElement[]} */
  const content = [];
  if (state.notice) {
    content.push(
      el("p", {
        className: "notice",
        text: state.notice,
        attrs: { role: "status" },
        on: { click: () => ((state.notice = ""), render()) },
      }),
    );
  }
  if (!paired && route !== "/pair") {
    location.hash = "/pair";
    return;
  }
  const session = route.startsWith("/session/") ? route.slice("/session/".length) : null;
  if (route === "/pair" || !paired) renderPair(content, ctx);
  else if (session !== null) renderSession(content, ctx, decodeURIComponent(session));
  else if (route === "/new") renderNewSession(content, ctx);
  else if (route === "/settings") renderSettings(content, ctx);
  else renderSessions(content, ctx);
  replace(root, content);
  highlightTabs(route);
}

/**
 * @param {string} route
 */
function highlightTabs(route) {
  for (const a of document.querySelectorAll("#tabs a")) {
    const href = a.getAttribute("href") ?? "";
    const on = href === `#${route}` || (route.startsWith("/session/") && href === "#/sessions");
    if (on) a.setAttribute("aria-current", "page");
    else a.removeAttribute("aria-current");
  }
}

async function boot() {
  ctx = makeCtx();
  try {
    state.identity = (await getIdentity()) ?? (await createIdentity());
    state.devices = await listDevices();
  } catch (e) {
    const msg = e instanceof StoreError ? e.message : "This browser cannot keep a pairing.";
    setConn(msg, "error");
    render();
    return;
  }
  // An offer in the address (the link from the PC) opens the pairing screen, once. The fragment
  // is removed at once: it carries the one-time PSK.
  if (location.hash.startsWith("#p=")) {
    state.pendingOffer = location.href;
    history.replaceState(null, "", `${location.pathname}#/pair`);
  }
  state.activeId = state.devices[0]?.host_id ?? null;
  if (state.devices.length > 0) void connectDevice(state.devices[0]);
  else setConn("Not paired", "idle");
  window.addEventListener("hashchange", () => render());
  if ("serviceWorker" in navigator) {
    navigator.serviceWorker.register("/sw.js").catch(() => undefined);
  }
  render();
}

void boot();
