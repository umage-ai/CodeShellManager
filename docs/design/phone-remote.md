# Phone remote: design note

Status: **design, no code yet** (branch `phone/addon`, base `b16b6c48f57aaa493d627f3e32d4f9cc2d4e1060`).
Read with `docs/addons.md` (the add-on protocol, screens), `relay/README.md` (the relay),
`crates/csm-remote-proto/` (the wire protocol and its vectors) and `docs/design/remote-csm.md`
section 4.6 (how this relates to the peer remote).

The owner's decisions (taken, not up for debate here): the phone remote is a **process add-on**,
public in this repository, **off by default**. It connects **outbound** to a self-hostable relay
(`csm-relay`, built in `relay/`). Everything is end to end encrypted with Noise; the relay is
untrusted. umage hosts a relay for umage's own use only. The phone web app is a static site on a
umage static domain, **not** served by the relay. Scope is view and operate, with the fixed op set
(no free text, no terminal reading). Push notifications show the session name, with a "hide names"
switch.

Where this note says "decision", the owner should confirm before step 1 starts. The list is in
section 13.

---

## 0. What the repository already has (checked, not assumed)

| Piece | Where | State |
|---|---|---|
| Phone protocol: ops, levels, responses, events, push payload | `crates/csm-remote-proto/src/msg.rs` | built, tested |
| Noise parameters and check code | `crates/csm-remote-proto/src/noise.rs` | built; no Noise code in the crate (the add-on uses `snow`, the phone its own) |
| Pairing offer (QR payload), pairing messages | `crates/csm-remote-proto/src/pairing.rs` | built, tested |
| Relay control messages and host frames | `crates/csm-remote-proto/src/relay.rs` | built, tested |
| Limits, refusal codes, ids, token hashes | `limits.rs`, `codes.rs`, `ids.rs` | built |
| Shared vectors (`noise.json`, `pairing.json`, `messages.json`, `frames.json`, ...) | `crates/csm-remote-proto/vectors/` | built; `tests/vectors.rs` regenerates them |
| Relay (`csm-relay`) with push forwarding and access control | `relay/` (own `Cargo.toml`, `tests/relay.rs`) | built |
| Process add-on shape (JSON-RPC over stdio, data folder, screens) | `apps/csm-review/` (`host.rs`, `view.rs`), `samples/addons/review/addon.toml` | built |
| Private file helper for the data folder (`icacls` on Windows, 0700/0600 on Unix) | `crates/csm-peer/src/fsperm.rs` | built; the add-on should reuse the approach, which may mean moving it to a shared crate (step 1 decision) |
| QR code generation and drawing | `crates/csm-ui/src/screens/remote.rs` (`qr_modules`, `qr`), uses `qrcodegen` | built for the peer pairing screen |
| Secrets in the OS keychain for add-ons (`addon_secret_*`, effect `stores_secrets`) | `docs/addons.md` "Secrets" | documented as built |
| Screen widgets | `docs/addons.md` "The widgets" | built; **no image, no QR widget** |
| Session/inbox/task tools the add-on needs | `docs/tools.md` | built (names below) |
| Source policy: process spawn only in listed modules; no clippy-denied panics in `DENY_ROOTS` | `xtask/src/policy.rs` | `apps/csm-phone/` must be added to `DENY_ROOTS` |

Relevant crypto already in `Cargo.lock` (no new downloads for the add-on): `snow` 0.10, `p256` 0.14,
`hkdf` 0.13, `aes-gcm` 0.10, `sha2` 0.10, `tungstenite` 0.29 (through `tokio-tungstenite`).

---

## 1. Components and names

| Part | Name and place | Language | Notes |
|---|---|---|---|
| Add-on (host side) | `apps/csm-phone/`, binary `csm-phone`, add-on id `phone-remote` | Rust | Copies the shape of `apps/csm-review`: a library (`csm_phone`) with the logic, a thin `main.rs`. Lint roots as in `DENY_ROOTS`. |
| Relay | `relay/` (existing) | Rust | No change for v1. Dev tests start it on 127.0.0.1. |
| Phone web app | `phone/` at the repository root | plain ES modules, JSDoc types | Static files only. Its own tests run with Node. Not a Cargo member. |
| Shared protocol | `crates/csm-remote-proto/` (existing) | Rust | Gets the Noise and pairing vectors the browser must pass. |

Why `phone/` and not `apps/`: `apps/*` are Cargo members that `xtask` checks as Rust. The web app is
JavaScript with its own tooling. Keeping it at the top level stops the Rust gates from reading it,
and it deploys on its own.

A new repository is **not** needed for v1. Everything lives here: the add-on, the relay, the web app
and the vectors. Deployment (the umage relay and the umage static site) is an operations task for the
owner; section 14 lists what that needs. The site may later be published from a separate repository
or a build of `phone/`; that is a deployment choice, not a code one.

---

## 2. Scope

**In v1:**
- Pair a phone with a PC by scanning a QR code (or pasting the link). Check code, Allow or Deny,
  level (viewer or operator).
- Device list with revoke, and a level change.
- Session list with status and "needs you" first; session detail.
- Operate by level: wake, sleep, restart, close (with the busy confirmation), new session from an
  allow-listed folder and profile, the allow-listed slash commands, clear status, and "Turn on Remote
  Control" for a session.
- Web Push for "needs you" with a "hide names" switch.
- A PWA: installable, the app shell works offline, no data stored outside the phone's IndexedDB.

**Out of scope for v1** (explicit, so they are not built by accident):
- Free text into a session, the terminal (reading or typing), screen contents, a session's output.
- Diffs, review, tasks editing, inbox actions beyond listing.
- Any op the fixed set does not have. A phone never names a tool id or passes parameters.
- The peer remote (full tools and terminal streams over TCP, `remote-csm.md`). Different profile, different
  device kind, no shared device store (section 4.6 of that note).
- Multiple relays per device. One relay per paired PC; a phone paired with two PCs has two entries.
- Hosted relay for anyone but umage. A self-hosted relay is documented in `relay/README.md`.
- Native phone apps, app stores, a Capacitor or similar wrapper.
- A relay-side account system. Admission is by host (the relay's own access control).
- Offline command queues. A command sent while the PC is unreachable fails with `host_offline`.
- Localisation beyond English.

---

## 3. The add-on's manifest

`samples/addons/phone-remote/addon.toml` (shipped disabled, like `review`; the installer change is a
decision, section 13). Exact draft:

```toml
# First-party add-on: "Phone remote". Off until you enable it.
# A program (csm-phone) that CSM starts when you enable this add-on. It opens one outbound
# connection to the relay you set (or the umage relay), pairs phones you allow, and turns their
# fixed requests into CSM tools. Nothing listens on this PC. End to end encrypted; the relay
# cannot read or forge what it carries. See docs/design/phone-remote.md.

[addon]
id = "phone-remote"
name = "Phone remote"
version = "0.1.0"
description = "Operate your sessions from a paired phone through a relay you choose. End to end encrypted. Off until you enable it; you choose which folders and commands a phone may use."
api = 1
kind = "process"
author = "CodeShellManager"

[process]
argv = ["csm-phone"]
effects = ["stores_data", "stores_secrets", "accesses_network", "sends_input", "launches_process"]
calls = [
  "session_list", "session_get", "session_recent", "inbox_list",
  "task_list", "task_get", "settings_get",
  "session_new", "session_wake", "session_sleep", "session_restart",
  "session_close", "session_status_clear",
  "session_send_command",
]
```

Effects, honestly:

| Effect | Why |
|---|---|
| `stores_data` | device store, host key file, config, push subscriptions, under `addon-data/phone-remote/` |
| `stores_secrets` | the relay enrolment token and the relay host secret, through `addon_secret_set` |
| `accesses_network` | the WebSocket to the relay and the push POSTs made by the relay on its behalf |
| `sends_input` | `session_send_command` (a slash command from the allow list, typed and Enter) |
| `launches_process` | `session_new`, `session_wake`, `session_restart` start agent programs |

`calls` lists what runs without a consent card per call. The enable dialog shows the whole list.
Those tools change sessions, so this list is a real grant: the person approves it once, at enable.
Decision 3 in section 13 asks whether the owner accepts that (the alternative is a consent card for
each phone action, which makes the phone unusable).

Add-on tools (toolset `addons`, through the `[process]` tool list in `initialize`):

| id | effects | what |
|---|---|---|
| `phone-remote.status` | read | the relay state, paired devices (name, level, last seen, kind), the device store state (ok, damaged) |
| `phone-remote.pair` | ui, state (opens the pairing screen; starts a pairing slot) | the person's pairing screen |
| `phone-remote.revoke` | state | `device_id`; the same as the Revoke button |
| `phone-remote.enable_access` | state, accesses_network | `on` (bool): the second switch under the add-on, so enabling the add-on alone does not connect |

These tools are for CSM and MCP. A phone never calls them.

---

## 4. Op to CSM tool mapping

The add-on checks the device's level with `Op::required_level()` (already done in `msg.rs`), then
validates the arguments (`Op::validate()`), then maps the op. Policy (is this folder allowed, is this
command configured, is this profile known) is the add-on's own, read from its config. Every mapping
is one fixed tool, and no argument from the phone reaches a CSM tool unchecked.

| Op | Level | CSM tool(s) | Notes |
|---|---|---|---|
| `hello` | viewer | none | Looks the device up in the store by routing token. Unknown: `bye` with `revoked`. Replies with `HelloData` (`level`, `hide_names`, `notify`, `push_ready`, `remote_control` from `settings_get ClaudeCommands`, `max_message_bytes` 48 KiB). |
| `ping` | viewer | none | |
| `snapshot` | viewer | `session_list` (+ `inbox_list` for the count) | Starts a new `seq` (section 7). |
| `session.list` | viewer | `session_list` | Same data as `snapshot`, one message. |
| `session.get` | viewer | `session_get` | Returns `SessionInfo` with `message` (the program's own status text). |
| `inbox.list` | viewer | `inbox_list` | `InboxItem` per entry. |
| `task.list` | viewer | `task_list` (open tasks, `include_closed` false) | Titles and state only. |
| `task.get` | viewer | `task_get` | Title, state, session. The body is **not** returned in v1 (section 13, decision 6). |
| `folder.list` | viewer | `session_recent`, `session_list` (folders), add-on config | Only folders on the add-on's allow list. `pinned` is true for allow-list entries marked pinned in config. |
| `profile.list` | viewer | none (add-on config) | Profiles are the add-on's own: a name, a label, a command (argv) on the PC. A phone sees name and label, never the argv. |
| `command.list` | viewer | `settings_get` (`ClaudeCommands`), add-on config | The intersection: the PC's configured slash commands and the add-on's allow list. Default allow list: `/remote-control`. |
| `session.wake` | operator | `session_wake` | |
| `session.sleep` | operator | `session_sleep` | |
| `session.restart` | operator | `session_restart` | Slow: it waits for the program to exit. The reply is sent when CSM answers; the channel stays open meanwhile. |
| `session.close` | operator | `session_close` (`force` as sent) | A busy session without `force` gets `busy` back; the phone asks again. |
| `session.new` | operator | `session_new` | `folder` must be on the allow list; `command` comes from the profile (never from the phone); `kind` local only. With `remote_control`, the add-on waits (at most 2 minutes) for the session to report idle, then calls `session_send_command` with `/remote-control`. |
| `session.command` | operator | `session_send_command` (`command`, `force` as sent) | `command` must be on the `command.list` intersection, else `command_not_allowed`. CSM itself refuses a session mid-turn. |
| `session.status_clear` | operator | `session_status_clear` | |
| `push.subscribe` | viewer | none (device store) | The endpoint must be https and on the relay's allow list (`push.allowed_hosts`); checked here too, so a refused one never reaches the relay. |
| `push.unsubscribe` | viewer | none | |
| `settings.set` | viewer | none (device store) | `hide_names`, `notify`. Per device. |

Error mapping, from CSM's error to the phone's `ErrorCode`:
`unknown session` to `unknown_session`; a busy refusal to `busy`; a refused command to
`command_not_allowed`; a folder off the list to `folder_not_allowed`; any other tool failure to
`failed` with CSM's message (plain text, shown as is); CSM not answering (the 10 s call timeout) to
`unavailable`. The phone never sees CSM's raw JSON.

**Concurrency.** Every op runs on a worker. The phone channel loop never waits for a CSM call. Per
channel, at most 8 requests in flight; beyond that the add-on answers `rate_limited`. A `session.restart`
can take a long time, so it does not hold the other requests of its channel.

**Calls from the add-on** use `csm/call` as in `addons.md` ("Process add-ons", "Messages"). Their
results go back to the request that asked.

---

## 5. The add-on's own storage

Folder: `<state folder>/addon-data/phone-remote/` (the `data_dir` of `initialize`; the folder is
created by CSM when the process starts, and the add-on creates its files below).

| File | Contents | Permissions | If damaged |
|---|---|---|---|
| `host-key` | the host's X25519 static private key (32 bytes, raw, no text) | owner-only (0600; Windows: one ACE for the current user, as `fsperm.rs`) | **error**: the add-on stops serving and says so. It is never replaced. |
| `devices.json` | `{"version":1,"devices":[...]}` (see below) | owner-only | **error**, the same. A backup `devices.json.bak` (the last good copy, written before each change) is shown in the error, and the person may restore it. Nothing is restored by itself. |
| `config.json` | the relay URL, the folder allow list, the profiles, the command allow list, `enabled` (the second switch), push on or off | owner-only | error, the same, but the add-on runs with defaults (no relay, nothing allowed) and says why. |
| `push.json` | not a file: push subscriptions live in the device store per device. | | |

`devices.json` entry (kind column, section 4.6 of `remote-csm.md`):

```json
{"device_id":"d_...","kind":"phone","name":"Pixel 9","level":"operator",
 "token_hash":"base64url(32)","phone_pub":"base64url(32)",
 "paired_at":1770000000,"last_seen":1770003600,
 "hide_names":false,"notify":true,
 "push":{"endpoint":"https://...","p256dh":"...","auth":"..."}}
```

- The device store is written by a temp file in the same folder, then renamed over the old one (no
  half-written file after a crash). The `.bak` is the previous file, copied before each rename.
- Only the add-on writes it. A `devices.json` that does not parse, has a wrong `version`, a duplicate
  `device_id` or `token_hash`, or a field out of range is **damaged**. Damaged means error: no phone
  connects, the screen says "the device list is damaged; the file was not changed", and two buttons
  appear: "Open the folder" and "Start a new device list (keeps the damaged file as `devices.json.damaged-<time>`)".
  The second one needs a confirm. Nothing does it automatically.
- `host-key` missing or damaged: the same, and it is **never** regenerated silently. A new key means
  every paired phone is unknown (the phones pin `host_pub` from the QR). Recovery is the same
  explicit button, and it removes the pairings.
- A "damaged" file with a name that is not `phone-remote`'s own is never touched.

**Secrets.** The relay enrolment token (when the relay uses one) and the relay host secret (32 bytes,
base64url; its hash is the host id) go through `addon_secret_set` / `addon_secret_get`, with keys
`relay.enrol_token` and `relay.host_secret`. They never go into `config.json` or `state.json`. The
host id the relay checks is a hash of the host secret, so losing it changes the host id: the same
"explicit recovery" rule applies.

The host key is a file, not a secret in the keychain, because the brief asks for the device store and
host key as files (decision 4 offers the keychain for both).

**Path rules.** Every path is built inside the data folder from fixed names. No path comes from a
phone, a relay message or a config value.

---

## 6. The relay connection

- One outbound WebSocket to `wss://<relay>/v1/host` (plain `ws://127.0.0.1` for dev only, which the
  offer validation already allows). The relay URL is in `config.json`.
- Hello: `HostToRelay::Hello { v, host_id, host_secret, access }`. `access` is the enrolment token,
  when set, read from the keychain.
- Then `Allow { tokens }` with the hashes of every device's routing token (`token_hash`), and
  `PairingOpen` only while the pairing screen is open (at most 600 s).
- Phone channels are binary `HostFrame`s (`version | kind | channel u32 BE | payload`). The add-on keeps a
  map from channel to device and enforces one Noise session per channel.
- Reconnect with backoff 1, 2, 5, 10, 30 s (as `relay/README.md`). After every connect the add-on
  sends `Allow`, and an open pairing slot again, since the relay forgets them when the host disconnects.
- Silence: the relay drops a host silent for 90 s. The add-on sends a WebSocket ping every 30 s.
- Threads, not an async runtime: one reader and one writer thread per socket, bounded channels
  (64 frames per phone channel). A full queue closes that channel with `rate_limited`, not the host.

Noise (the add-on side uses `snow`):
- Pairing: `Noise_IKpsk2_25519_AESGCM_SHA256`, the add-on is the responder, the prologue
  `csm-remote/1\0<host_id>`, PSK from the offer, msg 1 carries the phone's `PairMsg::Hello`.
- Devices: `Noise_IK_25519_AESGCM_SHA256`, msg 1 may carry the phone's hello JSON. The add-on accepts a
  static key only if its routing token and static key match a device-store entry.
- Rekey after `REKEY_INTERVAL` (10 000) messages per direction. A channel is closed after
  `MAX_CHANNEL_SECS` (3600) so the phone reconnects.
- Every app message is JSON of at most 48 KiB (`MAX_APP_MESSAGE_BYTES`); `msg::encode` refuses bigger.

---

## 7. Session deltas

The PC is the source of truth. Each paired phone channel has its own state in the add-on:
`last_sent: map<session id, SessionInfo>`, `seq`.

1. CSM sends `session_changed` and `inbox_changed` (the subscriptions in `initialize`). They are
   **coalesced**: at most one refresh per channel every 200 ms, so a burst of status changes is one
   `session_list`.
2. The refresh calls `session_list` once (shared by all channels), and each channel diffs its own
   `last_sent` against it: upserts for new or changed entries, `removed` for ids no longer present.
   A `SessionInfo` is changed when any of `name`, `folder`, `branch`, `status`, `group`, `profile`,
   `since` differs. `message` is never sent by a delta: it needs `session_get`, which only a phone that
   asked for a session's detail does. (So a detail screen shows the message; the list does not.)
3. The delta gets `seq = last_seq + 1` and is sent. `last_sent` is updated only after the send succeeds
   to the relay; if the channel is gone, the state is dropped and the next connect sends a snapshot.
4. A snapshot (`hello`, `snapshot`, or any reconnect) starts a new `seq` with
   `split_snapshot(seq, sessions, unread, 48 KiB)`. Each part is one message, all with the same `seq`.
5. A delta bigger than 48 KiB is **split into several deltas with consecutive seqs**. The protocol
   text says "changes since seq-1"; with several deltas each one is its own step. `msg.rs` must say so
   (a doc change, step 1).
6. The phone applies a delta only if `seq` is exactly the one after the last one it applied. A gap, or
   a snapshot with a new seq, is handled as the protocol says: a gap means the phone sends `snapshot`.
7. `Inbox { seq, unread }`: **open question** (decision 7). Either it shares the channel's seq counter
   (so a missed inbox message is a visible gap, which the phone then fixes with a snapshot), or it has
   its own counter. The design assumes the shared counter, which is what `msg.rs` implies.

Sizes: a session is roughly 200 bytes of JSON, so 48 KiB holds about 200 sessions per message.
Larger lists are split. The phone's list virtualises above 500 sessions.

---

## 8. Pairing: the screens and the QR code

Both screens are add-on screens (`docs/addons.md`, "Screens"). The add-on declares them in
`initialize`:

```json
"screens":[
  {"id":"phone-remote.pair","title":"Pair a phone","icon":"smartphone","scope":"global","summary":"Pair a phone with this PC"},
  {"id":"phone-remote.devices","title":"Phones","icon":"devices","scope":"global","summary":"Paired phones and their levels"}
]
```

Icons must exist in CSM's icon set (an unknown one draws none). `smartphone` and `devices` are
assumed; check the set in step 1.

**Pairing screen** (`phone-remote.pair`), while a slot is open:

```
Pair a phone                                         [Add-on: Phone remote]
Scan this code with the phone, or open the link on it.
[ QR code of the offer URL ]                 (new widget: section 8.2)
https://phone.umage.example/v/0.1.0/#p=...   (text, mono, selectable)
Expires in 9:42 · the code can be used once
--- after the phone connects ---
Device: Pixel 9      Check code: 8216
"Read the same four digits on the phone."
Level:  [ Viewer | Operator ]                (segmented, key "level")
[ Allow ]  [ Deny ]                           (buttons, event: true)
```

- The screen is a **tree** the add-on builds and CSM draws; the add-on never draws. Every control sends
  an event: `click` for Allow and Deny, `select` for the level. The add-on sends `Deny` (`denied`) on
  Deny, or on expiry (`expired`), and closes the slot with `PairingClose`.
- On Allow, the add-on creates the device (random `device_id`, 32-byte `routing_token`, stores
  `token_hash` and the phone's static key), appends it to `devices.json`, sends
  `Allow { tokens }` to the relay, sends `Accept { routing_token, level, device_id }` over the channel
  and closes the slot. The order matters: the device is saved before `Accept` is sent, so a crash never
  leaves a phone paired that the PC does not know.
- Limits from `limits.rs`: 5 attempts per slot, one pairing channel at a time, 10 minutes per slot.
  A sixth attempt, or a second pairing while one is open, is refused and shown on the screen.
- The check code is `check_code(handshake_hash)` on both sides. The screen says: "If the digits differ,
  press Deny." That is the whole defence against a swapped QR.

**Devices screen** (`phone-remote.devices`): a `list` of devices (`title` = name, `subtitle` = level,
last seen as "5 min ago", `badges` = "needs push" when notify is on but no subscription), and a detail
area with: level `segmented` (Viewer or Operator, a change re-keys nothing, it sends `Bye { level_changed }`
and the phone reconnects), `checkbox` "Notify me about sessions that need me", and a `Revoke` `danger`
button (confirm first, a CSM confirm draws it because the button's action is the add-on's own tool,
`phone-remote.revoke`). Revoke: the device leaves `devices.json`, the new allow list goes to the
relay, and the relay closes that device's channel with `revoked`. The phone gets `Bye { revoked }`.

**Indicator.** Two places in v1: the add-on's row in Settings > Add-ons shows the state text from
`plugin_status` (for example "Connected to relay, 2 phones"), and the screen's tab carries a `badge`
with the count of connected phones. A status-bar indicator needs a new CSM notification, see 8.3.

### 8.1 Whether the screen protocol can show a QR code

**No, not today.** The widgets are listed in `docs/addons.md` ("The widgets"): there is no image node.
`markdown` shows no images (alt text only). `code`, `diff` and `text` are text. A screen cannot carry a
picture, so the add-on cannot send a PNG of the QR code, and a data URI would not help either.

### 8.2 The smallest addition: a `qr` widget

A new node, drawn by CSM:

```json
{"type":"qr","key":"offer","text":"https://phone.umage.example/v/0.1.0/#p=...","label":"Scan with the phone","size":"md"}
```

- `text`: required, 1 to 512 bytes, no control characters. Longer is a bad prop: the node draws a
  notice and nothing else breaks. (The offer URL is about 330 to 400 bytes in practice, see the check in
  section 12.)
- `size`: `sm`, `md` (default), `lg`, as token-driven pixel sizes in `csm-ui`'s theme. Colours are
  the theme's `text` on `base` (a QR is dark on light in both themes; the quiet zone is drawn).
- `label`: optional caption under it.
- Error correction: `M`. The module matrix comes from `qrcodegen` (`qr_modules` in
  `csm-ui/src/screens/remote.rs`, which already draws the peer's pairing QR). Move `qr_modules` to a
  place both the peer screen and the engine's screen parser can use (it is pure: a string in, a grid
  out), for example `csm-core::qr`, behind the existing `qrcodegen` dependency. Only `csm-ui` draws it.
- The matrix is computed **when the tree is parsed** (on the worker that already validates the tree),
  stored in the node, and drawn in the frame. Frame code does no work beyond drawing modules, so the
  UI-thread rule holds.
- Node cap: a tree already has a 5,000 node cap. A QR node counts as one node, with its matrix not part
  of the 768 KiB tree size (the matrix is at most 177x177 bits, ~4 KiB).
- Accessibility: the QR node's `label` is also its screen-reader text, and the link under it is
  always shown as text, so a person can copy it.
- Unknown `type` degrades to "unsupported: qr" on an older CSM, as for every new widget.

Why not an image: a `blob` or image node would need a binary channel in the screen protocol (blobs are
"reserved, not built yet") and an image decoder. A QR widget is one prop and no decoder.

### 8.3 Other small additions to propose (not built here)

- A `csm/indicator` notification: `{text, tone}` per add-on, drawn in the status bar. Needed for the
  "indicator" the owner asked for. Until it exists, the indicator is the screen badge and the Settings
  row (section 8).
- `copy` on a `button`: `{"action": {"copy": "text"}}`, a button that copies its text (a link is
  otherwise hard to copy on a phone-less screen). Optional; the selectable text covers v1.

---

## 9. Push notifications

Chain: CSM status change, add-on decision, RFC 8291 encryption in the add-on, relay POST, push service,
phone service worker.

1. Trigger: a session's status changes **into** `needs_you_approval` or `needs_you_question`
   (from `session_list` deltas, step 7). Only one push per session per "needs you" episode, and at
   most one push per device every 30 s (the add-on's coalescing; the relay caps 60 per host and 20 per
   endpoint per hour as a second line).
2. Payload (`PushPayload`): `{"t":"needs_you","n":<sessions needing you>,"s":"<name>","sid":"<id>"}`.
   With **hide names** on for that device, `s` and `sid` are left out: the notification says
   "2 sessions need you" and no names. `n` is always sent.
3. Encryption: the add-on encrypts the JSON as an RFC 8291 `aes128gcm` body (ECDH on P-256 with a fresh
   ephemeral key, HKDF-SHA256, AES-128-GCM, 4096-byte record size), using the phone's `p256dh` and `auth`
   from `push.subscribe`. `p256`, `hkdf`, `aes-gcm`, `sha2` are already in the lock file. A test vector
   from RFC 8291 section 5 is the unit test.
4. Send: `HostToRelay::Push(PushRequest { id, endpoint, ttl: 3600, urgency: high, topic: "needs-you", body })`
   over the host socket. The relay adds the VAPID header (its own key) and POSTs. It sends back `PushResult`.
   A refused or failed push is logged by counts only, and the device is not removed for it. The relay's
   `PushResult` carries `ok`, the push service's HTTP `status` and a `code` (`push_failed` or `push_refused`).
   On a status of 404 or 410 the add-on drops that endpoint from the device, since the subscription is gone.
5. The browser decrypts the body (the Push API does, for `aes128gcm`), and the service worker shows
   `showNotification(...)`. The notification text comes from the payload, nothing else. The tag is `needs-you`,
   so a newer push replaces the older one instead of stacking.
6. A tap opens the app at the session list.

The add-on never sends VAPID keys: the relay holds them (`GET /v1/vapid` gives the public half to the
phone at subscribe time). The owner does not need to generate VAPID keys (section 14).

---

## 10. The phone web app (`phone/`)

### 10.1 Layout and stack

```
phone/
  index.html            app shell (one page, screens are sections)
  manifest.webmanifest  PWA manifest
  sw.js                 service worker: app shell cache, push, notificationclick
  css/app.css           tokens on :root (light and dark), no framework
  js/
    main.js             router and screens
    noise.js            Noise IK and IKpsk2 over WebCrypto (section 10.3)
    pairing.js          offer parsing (the QR payload), pairing exchange
    relay.js            WebSocket to the relay, binary frames for one channel
    protocol.js         ops and events as in msg.rs (JSON shapes, validation)
    store.js            IndexedDB: keys, devices, settings
    delta.js            applies snapshot, delta, inbox events with seq checks
    push.js             Push subscription (VAPID public key from /v1/vapid)
    ui/*.js             screen renderers (plain DOM, no virtual DOM)
  test/                 node:test: noise vectors, pairing vectors, message vectors, delta rules, a fake relay
  tools/serve.mjs       static server on 127.0.0.1 for dev (no dependencies)
  package.json          devDependencies only: typescript (for `tsc --checkJs`) and nothing at runtime
```

Decision: **plain ES modules with JSDoc, checked by `tsc --noEmit --checkJs`**, no bundler, no framework,
no runtime dependency. Why:
- The app is static. Modules are served as they are, so what is audited is what runs; there is no build
  output that differs from the source.
- The size budget is small (the screens are a few hundred lines each). A framework adds more code than
  the app.
- A TypeScript build step (`tsc` emit) would add a toolchain that must stay in step with the browser
  target. JSDoc gives the same type checks with no emit.
- The crypto is the risky part, and it is ours to write (10.3). Fewer layers under it is the point.

Rejected: React/Svelte/Vue (size, a build, a dependency tree for a 10-screen app); a WASM build of
`csm-remote-proto` (section 10.3).

No third-party script, font or CDN. The CSP is `default-src 'self'; connect-src 'self' wss://<relay>
https://<relay>; img-src 'self' data:; style-src 'self'; script-src 'self'`. The relay origin is set at
build time (one config file, `js/config.js`, generated from the deploy setting, never from a page
parameter). The QR scanner, if used, is the browser's `BarcodeDetector` when present, else paste.

### 10.2 Where the phone's keys and data live

- **Static identity key** (the phone's X25519 key): `crypto.subtle.generateKey({name:"X25519"}, false,
  ["deriveBits"])`. The private key is **non-extractable**: the page can use it for DH and cannot read its
  bytes. The `CryptoKey` object is stored in IndexedDB (structured clone keeps it non-extractable). The
  public key is exported (public keys are always exportable).
- **Ephemeral keys** for each handshake: generated, used, dropped, never stored.
- **Device record** in IndexedDB: `{relay, host_id, host_pub, routing_token, name, level, paired_at, settings}`.
  `routing_token` is 32 bytes and cannot be non-extractable (it goes to the relay as bytes). It is a routing
  credential, not an authentication key: the relay sees only its hash, and the host checks the phone's
  static key inside Noise. Treat it as a secret in the phone's storage all the same.
- **Push subscription** (endpoint, p256dh, auth): from the browser; the host gets a copy through `push.subscribe`.
- No `localStorage` for any of this. Storage calls are wrapped in try/catch; a browser that refuses
  IndexedDB shows "this browser cannot keep a pairing" and does not pair.
- Clearing site data forgets the pairing. The PC then shows the phone as a device that is not connected;
  the person revokes it there. That is the stated behaviour, and the Settings screen says it.

### 10.3 Noise in the browser: the choice

Two options, compared:

| | WebCrypto + a small Noise implementation (chosen) | WASM build of `snow` (or of `csm-remote-proto` + `snow`) |
|---|---|---|
| Primitives | X25519 (`deriveBits`), AES-256-GCM (`AES-GCM` with a 256-bit key, which is what Noise's `AESGCM` uses), SHA-256 (`digest`), HMAC (`sign`), HKDF (built from HMAC) | all in Rust |
| Code to audit | ~250 lines of Noise state (handshake patterns IK and IKpsk2, MixHash, MixKey, EncryptAndHash, split, rekey), plus tests | the `snow` crate plus a wasm-bindgen glue layer (larger, the glue and the generated JS are the audit surface) |
| Size | nothing to download | ~200 to 400 KB of WASM (estimate, to measure in step 2) |
| Browser needs | X25519 in WebCrypto: Chrome and Edge 133+, Firefox 130+, Safari 17+ (**to confirm in step 2 with the current compat tables**) | WebAssembly only, every current browser |
| Constant-time | WebCrypto's implementations (native) | `snow`'s (dalek), in WASM |
| Test vectors | the same `noise.json` as `snow` produces | the same |

Decision: **WebCrypto**. It needs no binary blob, keeps the private keys non-extractable (WASM cannot:
its memory holds them), and the Noise layer is small enough to read. The cost is the browser floor
(an old Android WebView without X25519 cannot pair; the app says so and suggests a current browser).
Fallback if step 2 finds the floor too high for the owner's phones: the WASM option, with the same vectors.

Checks the implementation must pass (step 2):
- `crates/csm-remote-proto/vectors/noise.json`: the full handshake and transport bytes for IKpsk2 and IK,
  with fixed keys (the file says so: "A JS implementation given the same private keys, ephemerals, prologue
  and psk must produce exactly these bytes").
- `pairing.json`: the offer decode, the check code.
- `messages.json`, `frames.json`, `codes.json`, `ids.json`: the app messages, the frame header, refusal
  codes, token hashes.
- Rekey (every `REKEY_INTERVAL` messages) against a vector or against `snow` run in the e2e harness.
- WebCrypto's X25519 `deriveBits` with an all-zero shared secret (a low-order public key) must be
  refused: the Noise spec requires it, and WebCrypto may not check it everywhere. The code checks the
  output is not all zeros.

### 10.4 PWA

- `manifest.webmanifest`: name "CodeShellManager Phone", `display: standalone`, icons (generic, no
  logos of other products), theme colour from the token file. Installable on Android and iOS 16.4+ (home
  screen Web Push on iOS needs the installed app: the add-on's notification path depends on it, step 2 test).
- `sw.js`: caches the app shell (versioned, one cache name per build), never the relay's responses, never
  the keys. Offline, the shell opens and says "no connection". Push events show the notification; the
  payload is the only text used.
- No state in the service worker. Keys and devices stay in the page's IndexedDB.

### 10.5 Screens

1. **Pair.** "Scan the code on your PC" (BarcodeDetector when available, camera permission asked here), or
   "Paste the link" (a text field; the link or the bare payload, `PairingOffer.decode` accepts both). Then
   "Connecting…", the device name the phone sends (default: the browser's name, editable), the check code
   on the phone ("Read the four digits on the PC; if they differ, do not continue"), and "Waiting for the PC
   to allow this phone". Denied and expired are plain messages.
2. **Sessions** (the default screen once paired). Connection state at the top ("Connected", "Connecting…",
   "Offline: showing the last list from 14:02"). Groups: "Needs you" first (approval, then question), then
   "Working", "Idle or done", "Asleep", "Exited". Each row: name, folder's last path segment, status word,
   `message` if known, time since the status change. A pull-to-refresh sends `snapshot`. Unread inbox count
   in the header.
3. **Session.** Name, folder, branch, status, message (from `session.get`), and the operate buttons by level.
   Viewer: none. Operator: Wake (if asleep), Sleep (if awake), Restart, Close (a confirm; a busy refusal
   asks again "This session is busy. Close it anyway?" and sends `force`), Clear status, Send command
   (from `command.list`, a picker of the allowed commands only; `/remote-control` shows "Turn on Remote
   Control"). Every destructive button says what it does in words.
4. **New session.** Folder (from `folder.list`), profile (from `profile.list`), optional name, "Turn on
   Remote Control when ready". Operator only. Free text is limited to the name (max 128 chars, no control
   characters, as `msg.rs` checks).
5. **Settings.** This device: its name, its level (read only; "your PC decides"), "Hide session names in
   notifications" (`settings.set { hide_names }`), "Notify me when a session needs me" (`notify`), Turn
   notifications on or off (the browser permission and `push.subscribe`/`push.unsubscribe`), "Forget this
   phone" (removes the local record only and says the PC still lists it until revoked there), About (version,
   relay host name, the protocol version).

Viewer-level UI hides operator buttons. That is UX only: the add-on refuses the op (`forbidden`) whatever
the UI shows.

Screens use the tokens in `css/app.css` only (no colour literals in the screen code), mirroring CSM's rule.

---

## 11. Local development and testing (no hosted service)

- **Relay:** `cargo run -p csm-relay -- --config relay/relay.dev.toml` with `bind = "127.0.0.1:8787"` and
  `access.open = true` (dev only). The relay refuses to start with no admission rule, so the dev file says
  `open = true` explicitly. Push can be off (`push.enabled = false`) in dev; the push path is tested in
  the add-on's unit tests and in the relay tests.
- **Add-on:** `csm-phone` runs as CSM starts it. For dev without the app, a test entry point drives it
  against a fixed fake CSM on its stdin and stdout, with the state folder in a temp dir, the same way
  `csm-review`'s tests drive it.
- **Web app:** `node phone/tools/serve.mjs` serves `phone/` on `http://127.0.0.1:8080`. WebCrypto and service
  workers both work on `127.0.0.1` (a secure context). The offer's app origin is `http://127.0.0.1:8080`.
- **Phone without a phone:** `phone/test/fake-phone.mjs` (Node 22+) is a second implementation of the phone
  side: it uses the same `noise.js` and `relay.js` against `ws://127.0.0.1:8787`, so it exercises the real
  app code, not a mock. A headless browser (Playwright, already available to the team) can drive the real
  page for one smoke test: pair, see the list, operate one session, check the level gate.
- **End to end harness** (`cargo xtask phone-e2e`, not in `ci`; step 3): starts the relay (the `csm_relay`
  library, as an in-process test server on an ephemeral port), starts the add-on with a fake CSM (a JSON-RPC
  peer that answers `session_list`, `session_wake`, and so on from a fixture, as `csm-review/tests/fixture.rs`
  does), runs the Node fake phone through pairing, the snapshot, a delta after a fixture change, a
  viewer refused `session.wake`, an operator wake, revocation, and a push (against a local fake push
  endpoint on 127.0.0.1 that checks the RFC 8291 decryption). Everything is loopback.
- **Gate:** the Rust parts run in `cargo test --workspace`; the JS tests run with `node --test phone/test`
  in `xtask ci` only when `phone/` exists (step 3 adds that check). The `ci/**` and `ci-full/**` GitHub
  Actions triggers stay as they are: nothing here triggers CI.
- **Never:** no NEXUS host, no cloud machine, no umage host. Tests use the loopback address only.

---

## 12. Checks on facts this design depends on

- The offer URL length: a `PairingOffer` is six short fields (`v`, `r`, `h`, `k`, `i`, `p`), base64url of the
  JSON. With `r` about 40 bytes, `h` 26, `k` 43, `i` 22, `p` 43, the JSON is about 220 bytes and base64url
  about 300 bytes; with the app origin and `/v/<version>/#p=` about 380 to 420 bytes. Under the 512-byte QR
  limit, with room. Step 1 adds a unit test for the worst case (a 256-byte relay URL).
- `msg.rs` says the relay's `Snapshot` splits and that a `Delta` is "changes since seq-1". Section 7 relies on
  consecutive seqs for split deltas and on the inbox sharing the seq. Both need a line in `msg.rs` (step 1).
- `session_list` returns `status` values `working`, `needs_you_approval`, `needs_you_question`, `done_unseen`,
  `error`, `idle`, `asleep`, `starting`, `exited` (`docs/tools.md`). `PhoneStatus` has seven values. The map:
  `working` and `starting` to `working`; `needs_you_*` as is; `done_unseen` to `done`; `error` to `idle`
  with the message (decision 8: or a new `PhoneStatus::Error`, which is a `msg.rs` change); `idle`, `asleep`,
  `exited` as is.
- `session_send_command` types only the `ClaudeCommands` list (`docs/tools.md`), so the phone's command
  allow list can never be wider than the PC's list.
- `profile_list` is Windows Terminal's profiles, not the phone's `profile` concept. The phone's profiles are
  the add-on's own (section 4). Decision 5 confirms this.
- The csm/call rule for input tools (`session_send_command` has effect `input`, i.e. `sends_input`) inside
  `calls`: `docs/addons.md` says palette commands may never run a `SENDS_INPUT` tool, and says nothing about
  `csm/call` in `calls`. Step 1 reads the engine code for this and, if `calls` refuses it, the design needs a
  consent card for `session.command` (decision 9).
- Browser X25519 support: a claim to verify in step 2 against current compatibility tables, not from memory.
- `xtask/src/policy.rs` `DENY_ROOTS` lists the crates whose roots deny the clippy lints. `apps/csm-phone` is
  added there in step 1.

---

## 13. Decisions for the owner

1. **Install with CSM or sample only.** Ship `phone-remote` in `samples/addons/` beside `review` (disabled,
   installed by the installer), or only as a sample people copy. Recommended: ship it, disabled, as `review`
   does. The installer change is a separate step.
2. **The relay URL default.** Default to the umage relay in the add-on, or leave it blank until the person
   types one. Recommended: blank, with a "Use the umage relay" button that fills the URL the owner provides.
3. **`calls` grants.** The add-on can launch, wake, restart, close and type slash commands without a card
   per call; the person approves the list once at enable. Alternative: consent cards for state changes, which
   makes the phone unusable. Recommended: the list, shown in the enable dialog.
4. **Keychain or file for the host key and relay secret.** The brief says files for the host key. The relay
   host secret and the enrolment token are in the keychain. Alternative: keychain for all three.
5. **Phone profiles.** The phone's `profile` is an add-on-owned list (name, label, argv on the PC), not
   Windows Terminal's profiles. Confirm.
6. **Task bodies.** `task.get` returns title and state only in v1. The body (a Markdown string written by
   people or agents) would be untrusted text on a phone; the owner decides whether to send it (and then
   with the bidi and control-character markers).
7. **Inbox and seq.** Share the channel's seq counter (recommended, section 7) or a separate one.
8. **`error` status.** Map to `idle` with the message (no `msg.rs` change), or add `PhoneStatus::Error`.
9. **Input tool in `calls`.** If the engine refuses `session_send_command` through `csm/call` without a card,
   accept a card for `session.command`, or make the engine allow it for this add-on's declared `sends_input`.
10. **Relay admission for umage.** The relay's `access.allowed_hosts_file` or an enrolment token for umage's
    own PCs (section 14).

---

## 14. Owner inputs needed later (not for this step)

| Item | Why | Notes |
|---|---|---|
| Relay hosting with TLS, a DNS name (`wss://`) | the add-on and phones connect to it | Caddy in `relay/Caddyfile` does certificates; any host that passes WebSocket upgrades works |
| Relay admission for umage PCs | `access` in `relay.toml` | an allow-list file or one enrolment token (`csm-relay enrol-token`) |
| A static domain for the web app (HTTPS) | phones open the app and the QR points there | the app is plain files; any static host with the CSP above |
| VAPID keys | Web Push | **not needed from the owner**: the relay creates its key pair on first run in `push.vapid_key_file`; the owner sets `push.vapid_subject` to a contact |
| Enrolment token for a second PC or a team | more hosts than the umage list | created with `csm-relay enrol-token` |
| Installer change (decision 1) | ship the add-on | a release step, `docs/release.md` |
| Apple developer account | not needed | the browser handles Web Push |

---

## 15. Out of scope, again (v1)

Free text and terminal access; diffs and review from the phone; task editing; multiple relays per device;
offline queues; native apps; a relay account system; hosted relays for other people; localisation; a
status-bar indicator (until the `csm/indicator` addition lands); `copy` buttons; the peer profile.

---

## 16. Build steps

Each step ends with the full gate (`cargo +1.97 test --workspace`, `cargo +1.97 clippy --workspace --all-targets -- -D warnings`,
`cargo +1.97 xtask ci --skip-cargo`) and a commit on `phone/addon`. Nothing is pushed.

**Step 1: the add-on, host side** (`apps/csm-phone`, `samples/addons/phone-remote/addon.toml`, `docs/design/phone-remote.md` updates, `msg.rs` doc lines, the `qr` widget and its `qr_modules` move).
- Device store, host key, config, secrets (`addon_secret_*` through a trait, so tests use a fake).
- Relay client (tungstenite, threads), Noise responder (`snow`), pairing exchange, Allow and Deny.
- Op dispatch to CSM calls through a trait (`CsmCalls`), with the fixed mapping of section 4 and the level checks.
- Deltas and snapshots (section 7), coalescing, the split.
- Push encryption (RFC 8291) and sending through the relay's push request.
- Screens: pairing and devices (section 8), the `qr` node in `csm-engine` screen parsing and `csm-ui` drawing.
- Tests (Rust): device store damage and atomic write, the level gate for every op, op validation, a relay fake
  for the channel, the pairing exchange, deltas (gap, split, coalesce), RFC 8291 vector, the QR worst-case length,
  the `qr` widget parse (cap, bad text), and `DENY_ROOTS` updated. The `view_overlay.rs` table is checked by its
  own test if the add-on adds UI-only effect tools (`phone-remote.pair` is `ui`: route it there).
- Commit.

**Step 2: the web app** (`phone/`, its tests in `phone/test`, the browser smoke test).
- `noise.js` on WebCrypto, passing the vectors (section 10.3), `pairing.js`, `relay.js`, `protocol.js`, `delta.js`.
- IndexedDB store, the non-extractable key, the device record.
- Screens (section 10.5), PWA files, service worker, push subscribe and show.
- `tsc --checkJs` clean, `node --test phone/test` passing, a Playwright smoke test against the local relay
  and the local add-on (fixture CSM).
- Commit.

**Step 3: end to end and docs.**
- `cargo xtask phone-e2e` (section 11): relay, add-on, fake CSM, the Node fake phone, push to a local fake endpoint.
- `xtask ci` gets the `phone/` check (tsc and node tests) when `phone/` exists.
- `docs/addons.md` gets a section for the phone add-on and the `qr` widget (the widget table and the screen
  limits), `relay/README.md` gets the dev setup, `docs/design/phone-remote.md` status is updated to "built".
- Commit.

Each step is one commit series on `phone/addon`, and each commit message ends with the attribution lines the
brief requires.

---

## 17. Facts this note corrects or flags

- The screen protocol cannot show an image or a QR code today (section 8.1).
- `msg.rs` does not define what a delta split means or whether inbox shares the seq (section 7); both need
  a doc line before the add-on relies on them.
- The `calls` rule for `SENDS_INPUT` tools via `csm/call` is not stated in `docs/addons.md` (section 12).
- `xtask`'s `DENY_ROOTS` does not list the add-on yet; it must, when the crate exists.
- Browser X25519 support is a thing to measure, not to assume (section 10.3).
