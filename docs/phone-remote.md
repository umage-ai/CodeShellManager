# Phone remote

The phone remote lets you see your CodeShellManager sessions on a phone and do a short, fixed list
of things to them: wake, sleep, restart, close, start a new session in a folder you allowed, send a
slash command you allowed, and get a "needs you" notification. It is a **process add-on** named
*Phone remote* (`samples/addons/phone-remote/addon.toml`, program `csm-phone`). It is **off until you
enable it**, and it connects to nothing until you also turn on the connection and set a relay.

The design, with the threat model and the decisions, is `docs/design/phone-remote.md`. This page is
for using it.

## How it fits together

- The PC opens **one outbound connection** to a relay. Nothing on the PC listens on a port.
- The **relay** (`relay/`, the `csm-relay` program) joins the PC and the phones. You can run your own,
  or use the one umage runs for its own PCs (see "Choosing a relay").
- The phone opens the **web app** from a static site (a link or QR code from the PC). The app is
  plain files; it is not served by the relay.
- Everything between the phone and the PC is **end to end encrypted** (Noise). The relay carries
  bytes it cannot read.

## Enable it

1. In CSM, open **Settings > Add-ons** and find **Phone remote**.
2. Turn it on. The enable dialog lists the CSM tools the add-on may call without asking each time
   (sessions: list, get, new, wake, sleep, restart, close, clear status, send a command, and reading
   the inbox and tasks). Approve that list once. A changed manifest turns the add-on off until you
   approve it again.
3. Open the add-on's **Settings** (the same screen, Phone remote). Set:
   - **Relay**: the `wss://` address of the relay (see below). Blank means nothing connects.
   - **Connect to the relay**: the second switch. Nothing dials out unless this is on as well as the
     add-on.
   - **Folders** and **profiles**: where a phone may start a new session. A phone can only pick a
     folder and profile you list here.
   - **Commands**: the slash commands a phone may send (exact spellings, each starting with `/`).
     A command must also be one CSM's own command list allows.
   - **Push**: whether "needs you" notifications are sent at all.
   - **Max phone sessions**: how many live sessions phones may have started at once (1 to 20,
     default 5).

Relay and connection changes take effect when the add-on restarts: turn Phone remote off and on.
Everything else applies at once.

The title bar shows one dot for the add-on's state, coloured by its worst tone. Its tooltip is the
status line, for example "Connected to relay, 1 phone" or "Relay unreachable, trying again". Clicking
the dot opens Settings > Add-ons.

## Choosing a relay

- **Run your own.** Copy `relay/relay.example.toml` to `relay/relay.toml`, admit your PC (its host id
  is shown in the add-on's settings, or use an enrolment token from `csm-relay enrol-token`), and run
  `docker compose -f relay/docker-compose.yml up -d --build` (Caddy does TLS). `relay/README.md` has the
  details, including the limits and the push settings.
- **For local testing only.** `relay/relay.dev.toml` admits every host on `127.0.0.1:8787` with push
  off. Use it on loopback and nowhere else:
  `cargo run -p csm-relay -- --config relay/relay.dev.toml`. Set the relay to `ws://127.0.0.1:8787`
  and the add-on's **app origin** to `http://127.0.0.1:8080`, and serve the phone app there with
  `node phone/tools/serve.mjs`.
- **The umage relay.** Only for umage's own PCs. Its address is not in this build yet; the settings
  screen does not offer it until the owner gives it.

The relay must be `wss://` (TLS with a certificate from a public certificate authority, as the
supplied Caddy setup provides). A `ws://` relay is accepted only on `127.0.0.1` for local testing.

## Pair a phone

1. Open the add-on's **Pairing** screen (from the command palette, or Settings > Add-ons) and press
   **Start pairing**. The screen shows a QR code and a link, and the time left (ten minutes).
2. On the phone, open the link or scan the code, and give the phone a name. Press **Send to the PC**.
   The phone sends its name and its confirmation. Nothing is compared yet, and the PC does not show a
   code until this press.
3. On the PC, the Pairing screen now shows the name the phone sent and a four-digit **check code**. The
   phone waits and shows the same four digits, with the words "Check that the PC shows these same four
   digits, then press Allow on the PC."
4. Compare the digits on the PC with the digits on the phone. Choose the level and press **Allow** on the
   PC only when they match. If they differ, press **Deny**. A different code means something else is in
   the middle of the pairing. Each button answers only the code printed beside it. If the waiting phone changed, the
   screen shows the new code and the old buttons no longer apply: compare again.

One phone pairs at a time. A second phone that tries while a slot is open is refused by the relay;
it can try again once the first is done or the time has run out.

## Levels

- **Viewer**: the session list (needs you first, then working, idle and done), each session's status
  and name, and the inbox. A viewer cannot change anything: the PC refuses every change from a viewer
  whatever the phone shows.
- **Operator**: everything a viewer has, plus wake, sleep, restart, close (with the busy confirmation
  when the session is busy), start a new session in an allowed folder and profile (optionally with
  Remote Control turned on for it), send an allowed slash command, and clear a session's status.

Change a phone's level on the **Phones** screen. The phone reconnects at its new level.

## Notifications and hiding names

- A phone with a notification subscription gets one push per "needs you" episode when a session
  starts to need you. It is sent whether or not the phone is connected. A phone gets at most one push
  every 30 seconds.
- **Hide session names** keeps names out of the notification text (the push still says how many
  sessions need you). It is a switch per phone on the **Phones** screen, and a switch for every
  phone at once.
- The phone turns notifications on itself (the browser asks permission). The PC only stores the
  subscription the phone gives it, and only if its push service is on the allowed list.

## Revoke a phone

On the **Phones** screen, select the phone and press **Revoke**, then **Yes, revoke**. The phone is
disconnected at once, and it cannot connect again until you pair it anew. Revoking one phone does
not affect the others. Turning the connection off in the add-on's settings stops every phone at once.

## What the relay can and cannot see

The relay can see:

- when a PC and a phone connect, how long they stay, and how much they send (sizes and rates);
- the PC's host id (a hash of its secret) and each phone's routing token, which it checks against the
  PC's allow list (it keeps hashes);
- pairing slots (their ids and expiry) and refusal codes;
- for a push, the push service endpoint the PC gave it, and the encrypted push body.

The relay cannot see, and cannot forge or change:

- session names, statuses, folders, commands, tasks, the inbox, or anything else the phone and PC
  say to each other. Those are inside the Noise channel, keyed with keys only the phone and the PC
  hold;
- the push text: the add-on encrypts it for the phone's subscription key (RFC 8291) before the relay
  sees it.

A hostile relay can drop, delay or replay traffic, and it can refuse service. It cannot read the
session list or send the PC an operation the phone did not make: the PC completes the encrypted
handshake only with the key the phone paired with, and it checks the level on every request.

## What you must provide for the hosted version

These are the owner's items (design section 14). None is needed to use the phone remote on your own
relay.

| Item | Why |
|---|---|
| Relay hosting with TLS and a DNS name (`wss://`) | PCs and phones connect to it |
| Relay admission for umage PCs (an allow-list file or an enrolment token) | the relay refuses hosts it does not admit |
| A static HTTPS domain for the phone app | phones open the app there; the QR code points at it |
| The umage relay URL | the "Use the umage relay" button (hidden until then) |
| VAPID keys | not needed: the relay creates its key pair on first run; set `push.vapid_subject` to a contact |
| Enrolment tokens for a second PC or a team | `csm-relay enrol-token` |
| The installer change that ships the add-on | a release step (`docs/release.md`); the add-on is in `samples/addons/` and disabled |

The phone app needs no build. Publish the files of `phone/` at the root of the static origin, leaving
out `test/`, `tools/`, `node_modules/`, the `package*.json` files, `tsconfig.json` and the README, and
send the headers in `phone/_headers` (the production content security policy).

## Testing

- `cd phone && npm ci` once, then `npm test` and `npm run typecheck`: the web app's tests (Noise
  vectors, pairing, the session channel, the rules). `cargo xtask ci` runs both when `phone/` exists.
- The Rust side: `cargo test --workspace` (the add-on's unit and protocol tests, the real relay tests
  in `apps/csm-phone-it/` and `relay/tests/`).
- **End to end:** `cargo xtask phone-e2e`. It starts the real relay in process on an ephemeral
  loopback port, starts the add-on's host against it with a fake CSM, and runs the phone app's own
  protocol code under Node 22 or later (`CSM_NODE` names another binary). It pairs a viewer, lists,
  checks that a viewer is refused an operator op, pairs an operator, wakes a session and checks that
  both phones see the change, subscribes the operator to push with a fake endpoint, revokes the
  operator and checks that it is cut off and cannot return, and checks that the viewer still works.
  Nothing leaves 127.0.0.1: no push service and no hosted relay is contacted.
- What the end-to-end test does not do: it does not drive a browser (a Playwright smoke test of the
  real page is not built yet), and it does not deliver a push. A push needs an https endpoint on an
  allowed push service, and the relay refuses loopback endpoints, so the push body is checked at unit
  level only (the RFC 8291 encryption against the RFC's own example, `apps/csm-phone/src/push.rs`).
