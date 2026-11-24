// Service worker (design section 10.4). It caches the app shell only: the HTML, CSS, scripts and
// icons, under one versioned cache name. It never caches a relay response, a session, a key or any
// request that is not a shell file or a page navigation. Push events show the text from the payload
// (js/push-text.js); a tap opens the session list.

importScripts("/js/push-text.js");

var CACHE = "csm-phone-shell-0.1.0";
var SHELL = [
  "/index.html",
  "/css/app.css",
  "/js/app.js",
  "/js/bytes.js",
  "/js/crypto.js",
  "/js/noise.js",
  "/js/protocol.js",
  "/js/relay.js",
  "/js/pairing.js",
  "/js/delta.js",
  "/js/client.js",
  "/js/store.js",
  "/js/push.js",
  "/js/push-text.js",
  "/js/ui/dom.js",
  "/js/ui/pair.js",
  "/js/ui/sessions.js",
  "/js/ui/session.js",
  "/js/ui/settings.js",
  "/manifest.webmanifest",
  "/icon.svg",
];
var SHELL_SET = new Set(SHELL);

self.addEventListener("install", function (event) {
  event.waitUntil(
    caches
      .open(CACHE)
      .then(function (cache) {
        return cache.addAll(SHELL);
      })
      .then(function () {
        return self.skipWaiting();
      }),
  );
});

self.addEventListener("activate", function (event) {
  event.waitUntil(
    caches
      .keys()
      .then(function (keys) {
        return Promise.all(
          keys
            .filter(function (k) {
              return k.indexOf("csm-phone-shell-") === 0 && k !== CACHE;
            })
            .map(function (k) {
              return caches.delete(k);
            }),
        );
      })
      .then(function () {
        return self.clients.claim();
      }),
  );
});

self.addEventListener("fetch", function (event) {
  var req = event.request;
  if (req.method !== "GET") return;
  var url = new URL(req.url);
  if (url.origin !== self.location.origin) return;
  if (req.mode === "navigate") {
    // The app page is one document for every /v/<version>/ path. Offline, the cached shell answers.
    event.respondWith(
      fetch(req).catch(function () {
        return caches.match("/index.html");
      }),
    );
    return;
  }
  if (!SHELL_SET.has(url.pathname)) return;
  event.respondWith(
    caches.match(url.pathname).then(function (hit) {
      return hit || fetch(req);
    }),
  );
});

self.addEventListener("push", function (event) {
  var payload = null;
  try {
    payload = event.data ? event.data.json() : null;
  } catch (e) {
    payload = null;
  }
  var n = CsmPush.notification(payload);
  event.waitUntil(self.registration.showNotification(n.title, { body: n.body, tag: n.tag, icon: "/icon.svg", data: { url: "/#/sessions" } }));
});

self.addEventListener("notificationclick", function (event) {
  event.notification.close();
  var target = (event.notification.data && event.notification.data.url) || "/#/sessions";
  event.waitUntil(
    self.clients.matchAll({ type: "window", includeUncontrolled: true }).then(function (list) {
      for (var i = 0; i < list.length; i++) {
        if ("focus" in list[i]) return list[i].focus();
      }
      return self.clients.openWindow(target);
    }),
  );
});
