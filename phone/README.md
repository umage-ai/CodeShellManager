# CodeShellManager phone app (`phone/`)

The phone's side of the phone remote (`docs/design/phone-remote.md`, section 10). Plain ES modules
with JSDoc, no bundler, no framework, no runtime dependency. The app is static files: what is served
is the source. The only development dependencies are TypeScript (for `tsc --checkJs`) and the Node
type definitions, both pinned in `package.json` and `package-lock.json`.

Requirements: Node 22 or later (the tests use `node:test` and the WebCrypto X25519 of Node 22+).

## Build and serve locally

There is no build step.

```sh
cd phone
npm ci                      # once: the pinned dev dependencies (typescript, @types/node)
npm run serve               # node tools/serve.mjs, on http://127.0.0.1:8080/
```

`npm run serve` takes a port: `node tools/serve.mjs 9000`. It listens on `127.0.0.1` only and sends
the **development** content security policy: `connect-src 'self' ws://127.0.0.1:*`, so the page can
reach a relay on the loopback address. Only this server sends it. A production host sends the
production policy in `_headers`.

To pair against a local relay (design section 11): start the relay in dev mode, then set the PC's
add-on `app_origin` to `http://127.0.0.1:8080` so the offer's link points at this server. The app
then reads the offer from the link's fragment (`#p=...`), which is never sent to any server.

Pages: `http://127.0.0.1:8080/` and `http://127.0.0.1:8080/v/<version>/` are the same app shell (an
offer link has the second form). WebCrypto and service workers both work on `127.0.0.1`.

## Deploy (a static host)

Serve the folder's files at the root of a static origin over HTTPS, with these exceptions:

- `test/`, `tools/`, `node_modules/`, `package*.json`, `tsconfig.json` and this README are not part
  of the app. Do not publish them.
- Every `/v/<version>/` path must return `index.html` (a rewrite), because the offer link has that
  path.
- Send the headers in `_headers` (Netlify and Cloudflare Pages read that file; elsewhere set the same
  headers). The production policy is `connect-src 'self' wss: https:`: the relay comes from the offer
  (design decision 11), and `ws://` is refused by the app itself except on the loopback dev origin.
- `camera=(self)` in `Permissions-Policy` is needed for the QR scanner; the paste field works without it.

## Tests and checks

```sh
npm test                    # node --test: vectors, Noise, pairing, the session channel, rules, worker
npm run typecheck           # tsc --noEmit --checkJs over js/ and test/
```

- `test/fixtures/*.json` are copies of `crates/csm-remote-proto/vectors/*.json`. The Rust test
  `crates/csm-remote-proto/tests/vectors.rs` fails when a copy differs, and
  `CSM_UPDATE_VECTORS=1 cargo test -p csm-remote-proto` regenerates both. So the phone is checked
  against the bytes the Rust side produces.
- `test/fakes.js` is an in-process relay socket and a fake PC (the responder, with the same Noise code).
  Nothing in the tests touches the network except the loopback dev server test.
- The policy tests (`test/policy.test.js`) keep rules on the source: no HTML written from data, no
  keys in web storage, no colour literals in screen code, the production and dev headers.

## Layout

```
index.html              app shell (absolute URLs; <base href="/">)
manifest.webmanifest    PWA manifest
sw.js                   service worker: app shell cache, push, notification click
_headers                production headers (CSP per design 10.1)
css/app.css             tokens on :root (light and dark); screens use these only
js/
  app.js                controller: identity, paired PCs, connection and backoff, routes
  crypto.js             WebCrypto: SHA-256, HMAC, Noise HKDF, AES-GCM, X25519
  noise.js              Noise IK and IKpsk2 (initiator and responder), transport, rekey
  bytes.js              base64url (canonical only), hex, concat
  pairing.js            the offer (QR payload), check code, the pairing exchange
  relay.js              the relay address rule, the WebSocket for one channel
  client.js             one session channel: hello, requests in order, events
  delta.js              the session list: snapshot parts, deltas, seq rules
  protocol.js           msg.rs and codes.rs in JS: ops, responses, events, messages
  store.js              IndexedDB: the identity key (non-extractable) and paired PCs
  push.js               VAPID key fetch and the Push subscription
  push-text.js          the notification text (a classic script for the worker)
  ui/                   screens (plain DOM; text is always textContent)
test/                   node:test suites and fixtures
tools/serve.mjs         the dev server
```

## What the app keeps, and where

- The identity's private key is a non-extractable X25519 `CryptoKey` in IndexedDB (database
  `csm-phone`). A browser that refuses IndexedDB gets "this browser cannot keep a pairing".
- Each paired PC: its host id, relay, pinned host public key, routing token, the device id and the
  level as last seen. Nothing else is stored. Session lists are kept in memory only.
- No `localStorage`. The offer's fragment is removed from the address bar when the app reads it.
