// The service worker and the notification text, run in a vm sandbox with a stub worker global: a push
// shows the text from the payload (or the generic text for anything that does not match), a shell
// file is served from the cache, and nothing else (no relay response, no session data) is cached.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import vm from "node:vm";
import { test } from "node:test";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const pushText = readFileSync(join(root, "js", "push-text.js"), "utf8");
const swSource = readFileSync(join(root, "sw.js"), "utf8");

/**
 * Loads push-text.js as a classic script in a fresh context and returns its CsmPush.
 * In a worker `self` is the global object, so the sandbox global is its own `self`.
 */
function loadPushText() {
  const ctx = {};
  ctx.self = ctx;
  vm.createContext(ctx);
  vm.runInContext(pushText, ctx);
  return /** @type {any} */ (ctx).CsmPush;
}

/**
 * Runs sw.js with a stub worker global. Returns its listeners and what it showed and cached.
 */
function loadWorker() {
  /** @type {Record<string, Function[]>} */
  const listeners = {};
  /** @type {Array<{ title: string, options: any }>} */
  const shown = [];
  /** @type {string[]} */
  const cacheAdds = [];
  /** @type {Map<string, string>} */
  const cached = new Map();
  const ctx = /** @type {any} */ ({});
  ctx.self = ctx;
  Object.assign(ctx, {
    location: { origin: "https://phone.example.test" },
    addEventListener: (/** @type {string} */ type, /** @type {Function} */ fn) => {
      (listeners[type] ??= []).push(fn);
    },
    skipWaiting: () => Promise.resolve(),
    clients: { claim: () => Promise.resolve(), matchAll: () => Promise.resolve([]), openWindow: () => Promise.resolve() },
    registration: {
      showNotification: (/** @type {string} */ title, /** @type {any} */ options) => {
        shown.push({ title, options });
        return Promise.resolve();
      },
    },
    caches: {
      open: () =>
        Promise.resolve({
          addAll: (/** @type {string[]} */ urls) => {
            cacheAdds.push(...urls);
            for (const u of urls) cached.set(u, `shell:${u}`);
            return Promise.resolve();
          },
        }),
      keys: () => Promise.resolve([]),
      delete: () => Promise.resolve(true),
      match: (/** @type {string} */ key) => Promise.resolve(cached.has(key) ? { body: cached.get(key) } : undefined),
    },
    fetch: () => Promise.resolve({ ok: true }),
    importScripts: (/** @type {string} */ path) => {
      if (path !== "/js/push-text.js") throw new Error(`unexpected import ${path}`);
      vm.runInContext(pushText, ctx);
    },
    URL,
    Set,
    Promise,
    console,
  });
  vm.createContext(ctx);
  vm.runInContext(swSource, ctx);
  return { listeners, shown, cacheAdds, cached };
}

/**
 * Fires the listeners of one event type, and waits for what they handed to waitUntil.
 * @param {Function[] | undefined} fns
 * @param {any} event
 */
async function fire(fns, event) {
  /** @type {Promise<unknown>[]} */
  const waits = [];
  for (const fn of fns ?? []) fn({ ...event, waitUntil: (/** @type {Promise<unknown>} */ p) => waits.push(p) });
  await Promise.all(waits);
}

test("the push text: a named session, several, and a count without names", () => {
  const P = loadPushText();
  assert.equal(P.notification({ t: "needs_you", n: 1, s: "api", sid: "s1" }).body, "api needs you");
  assert.equal(P.notification({ t: "needs_you", n: 3, s: "api", sid: "s1" }).body, "api and 2 more need you");
  assert.equal(P.notification({ t: "needs_you", n: 2 }).body, "2 sessions need you");
  assert.equal(P.notification({ t: "needs_you", n: 1 }).body, "1 session needs you");
});

test("a payload that does not match is shown as the generic text, never with a name", () => {
  const P = loadPushText();
  const bad = [
    null,
    "x",
    { t: "test", n: 1 },
    { t: "needs_you", n: 0 },
    { t: "needs_you", n: 1.5 },
    { t: "needs_you", n: 1, s: "a\nb" },
    { t: "needs_you", n: 1, s: "x".repeat(129) },
  ];
  for (const payload of bad) {
    // Objects from the vm context have another prototype, so compare plain copies.
    const got = JSON.parse(JSON.stringify(P.notification(payload)));
    assert.deepEqual(got, { title: P.TITLE, body: P.GENERIC, tag: P.TAG }, JSON.stringify(payload));
  }
});

test("a push event shows the notification with the tag, from the payload alone", async () => {
  const w = loadWorker();
  await fire(w.listeners.push, { data: { json: () => ({ t: "needs_you", n: 2, s: "api", sid: "s1" }) } });
  assert.equal(w.shown.length, 1);
  assert.equal(w.shown[0].options.body, "api and 1 more need you");
  assert.equal(w.shown[0].options.tag, "needs-you");
  assert.equal(w.shown[0].title, "CodeShellManager Phone");
});

test("a push with unreadable data still shows a notification (generic text)", async () => {
  const w = loadWorker();
  await fire(w.listeners.push, {
    data: {
      json: () => {
        throw new SyntaxError("bad");
      },
    },
  });
  assert.equal(w.shown.length, 1);
  assert.equal(w.shown[0].options.body, "A session needs you");
});

test("only the shell is cached at install: the relay and session data never are", async () => {
  const w = loadWorker();
  await fire(w.listeners.install, {});
  assert.ok(w.cacheAdds.includes("/index.html"));
  assert.ok(w.cacheAdds.includes("/js/app.js"));
  for (const u of w.cacheAdds) {
    assert.ok(/^\/(index\.html|css\/|js\/|manifest|icon)/.test(u), `unexpected cached path ${u}`);
  }
});

test("a fetch for a shell file is answered by the worker; a relay or API request is not", () => {
  const w = loadWorker();
  /** @type {string[]} */
  const answered = [];
  /**
   * @param {string} url
   */
  const fetchEvent = (url) => ({
    request: { method: "GET", url, mode: "cors" },
    respondWith: () => answered.push(url),
  });
  for (const fn of w.listeners.fetch ?? []) {
    fn(fetchEvent("https://phone.example.test/js/app.js"));
    fn(fetchEvent("https://relay.example.test/v1/vapid"));
    fn(fetchEvent("https://phone.example.test/api/sessions"));
  }
  assert.deepEqual(answered, ["https://phone.example.test/js/app.js"]);
});

test("a POST and a cross-origin request are left to the browser", () => {
  const w = loadWorker();
  let answered = 0;
  for (const fn of w.listeners.fetch ?? []) {
    fn({ request: { method: "POST", url: "https://phone.example.test/js/app.js", mode: "cors" }, respondWith: () => answered++ });
    fn({ request: { method: "GET", url: "https://other.example.test/js/app.js", mode: "cors" }, respondWith: () => answered++ });
  }
  assert.equal(answered, 0);
});
