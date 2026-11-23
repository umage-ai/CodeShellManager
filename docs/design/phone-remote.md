# Phone remote: design note

Status: **step 1 partly built** (branch `phone/addon`). The host core is in `apps/csm-phone/`: device store, config, ops mapping, pairing, Noise channels and the relay client (`ws://` only), all tested. Section 18 lists what is not built and where the code deviates from this plan.
Revision 3: fixes the first review round (indicator, add-on effects and caller rules, host-wide push
tracker, CSP and self-hosting, settings screen, QR length, storage first run, attacker-facing rules,
facts) and the second (launch rate limits and a live-session cap, the in-flight slot for waits, dev CSP,
the push host list, the pairing race, the damaged `config.json` write, the full `HelloData`).

Read with `docs/addons.md` (the add-on protocol, screens), `relay/README.md` (the relay),
`crates/csm-remote-proto/` (the wire protocol and its vectors) and `docs/design/remote-csm.md`
section 4.6 (how this relates to the peer remote).

The owner's decisions (taken, not up for debate here): the phone remote is a **process add-on**,
public in this repository, **off by default**. It connects **outbound** to a self-hostable relay
(`csm-relay`, built in `relay/`). Everything is end to end encrypted with Noise; the relay is
untrusted. umage hosts a relay for umage's own use only. The phone web app is a static site on a
umage static domain, **not** served by the relay. Scope is view and operate, with the fixed op set
(no free text, no terminal reading). Push notifications show the session name, with a "hide names"
switch. A pairing screen in CSM shows a link and a QR code, a check code, Allow or Deny, and a level;
a device list with revoke; and an indicator.

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
| QR matrix (`qr_modules`, Low error correction) and drawing (`qr`) | `crates/csm-ui/src/screens/remote.rs` (uses `qrcodegen`) | built for the peer pairing screen |
| Screen tree parsing and validation (`kind`, `checkbox`, `segmented`, ...) | `crates/csm-core/src/addons/screen/mod.rs` | built; no image, no QR node |
| Secrets in the OS keychain for add-ons (`addon_secret_*`, effect `stores_secrets`) | `docs/addons.md` "Secrets" | documented as built; the tools are refused for every caller except the add-on that stored the secret |
| Caller kinds `ui`, `cli`, `external`, `addon`, `peer` | `crates/csm-engine/src/plugins.rs` (`caller_json`) | built |
| Standing grants (`[process] calls`) for an add-on's `csm/call` | `plugins.rs` (`standing_call`), `engine.rs` (the consent check) | built: a listed tool runs without a consent card whatever its effects |
| `screen_open` (a `ui` effect tool, runs directly) | `docs/tools.md`, `docs/addons.md` "The tools" | built |
| Session/inbox/task tools the add-on needs | `docs/tools.md` | built (names below) |
| Source policy: process spawn only in listed modules; no clippy-denied panics in `DENY_ROOTS` | `xtask/src/policy.rs` | `apps/csm-phone/` must be added to `DENY_ROOTS` |

Relevant crypto already in `Cargo.lock` (no new downloads for the add-on): `snow` 0.10, `p256` 0.14,
`hkdf` 0.13, `aes-gcm` 0.10, `sha2` 0.10, `tungstenite` 0.29 (through `tokio-tungstenite`).

---

## 1. Components and names

| Part | Name and place | Language | Notes |
|---|---|---|---|
| Add-on (host side) | `apps/csm-phone/`, binary `csm-phone`, add-on id `phone-remote` | Rust | Copies the shape of `apps/csm-review`: a library (`csm_phone`) with the logic, a thin `main.rs`. Lint roots as in `DENY_ROOTS`. |
| Relay | `relay/` (existing) | Rust | No change for v1. The add-on's dev dependency on `csm-relay` runs the real relay on 127.0.0.1 in its tests. The relay refuses a second pairing channel with `not_allowed` (section 4.2). |
| Phone web app | `phone/` at the repository root | plain ES modules, JSDoc types | Static files only. Its own tests run with Node. Not a Cargo member. |
| Shared protocol | `crates/csm-remote-proto/` (existing) | Rust | Gets the Noise and pairing vectors the browser must pass. |
| CSM additions | `csm-core` (`qr` module, the `qr` node), `csm-engine` (`csm/status` handling), `csm-ui` (drawing) | Rust | Listed for review in sections 8.2 and 8.3. |

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
- An indicator in CSM for the add-on's state (section 8.3): the Settings row and the title bar.
- Session list with status and "needs you" first; session detail.
- Operate by level: wake, sleep, restart, close (with the busy confirmation), new session from an
  allow-listed folder and profile, the allow-listed slash commands, clear status, and "Turn on Remote
  Control" for a session.
- Web Push for "needs you" with a "hide names" switch, sent whether or not a phone is connected.
- A PWA: installable, the app shell works offline, no data stored outside the phone's IndexedDB.
- A settings screen in CSM: relay URL, enrolment token, folders, profiles, command allow list, push.

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
- A tab badge on the screen (the tab strip has no badge; section 8).

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
effects = ["stores_data", "stores_secrets", "accesses_network", "sends_input", "launches_process", "reads_session_content"]
calls = [
  "session_list", "session_get", "session_recent", "inbox_list",
  "task_list", "task_get", "settings_get",
  "session_new", "session_wake", "session_sleep", "session_restart",
  "session_close", "session_status_clear",
  "session_send_command",
  "screen_open",
]
```

Effects, honestly:

| Effect | Why |
|---|---|
| `stores_data` | device store, host key file, config, push subscriptions, under `addon-data/phone-remote/` |
| `stores_secrets` | the relay enrolment token and the relay host secret, through `addon_secret_set` |
| `accesses_network` | the WebSocket to the relay and the push requests the relay makes on its behalf |
| `sends_input` | `session_send_command` (a slash command from the allow list, typed and Enter) |
| `launches_process` | `session_new`, `session_wake`, `session_restart` start agent programs |
| `reads_session_content` | the add-on reads session names, status text and task titles and sends them to paired phones. It does not read terminal text or task bodies. |

`calls` lists what runs without a consent card per call. The enable dialog shows the whole list.
Those tools change sessions, so this list is a real grant: the person approves it once, at enable.
Decision 3 in section 13 asks whether the owner accepts that. The grant is real: a tool listed in
`calls` runs for the add-on's own `csm/call` without a card (`engine.rs`, the consent check, checks
`standing_call` first), so `session_send_command` (effect `sends_input`) runs through `calls` too.
This closes what was decision 9 in review round 1 (section 12).

`screen_open` is in `calls` because the pairing tool opens the pairing screen through it (section 8).
`addon_secret_set` and `addon_secret_get` are not in `calls`: they are the add-on's own tools, allowed
by its `stores_secrets` effect, and refused to every other caller (`docs/addons.md`, "Secrets").

**Add-on tools** (toolset `addons`, declared in `initialize`). An add-on tool may only name the effects
above. It may not name `ui`: that effect belongs to core UI-only tools and is not in the add-on's
declared list, so it would be refused (`docs/addons.md`, "Tools"). No add-on tool is routed through
`crates/csm-ui/src/view_overlay.rs`; that table is for core tools only.

| id | effects | callers | what |
|---|---|---|---|
| `phone-remote.status` | `[]` | any | relay state, number of paired devices, the device store state (ok, damaged). Device names and levels are returned to the `ui` caller only. No offer, no secret. |
| `phone-remote.pair` | `[]` | `ui` only; refused for `cli`, `external`, `addon`, `peer` | opens the pairing screen (`screen_open`, through `csm/call`). It opens **no** pairing slot: the person presses "Start pairing" on the screen. It writes nothing, so it declares no effect. |
| `phone-remote.revoke` | `stores_data` | `ui` only | `device_id`. The same change as the Revoke button on the devices screen. |
| `phone-remote.enable_access` | `stores_data`, `accesses_network` | `ui` only | `on` (bool): the second switch, so enabling the add-on alone does not connect. |

Why `pair` declares `[]` and not `stores_data`, as review round 1 suggested: the tool writes nothing.
Declaring a write it does not make would be the dishonest form of the effect. Declaring `[]` still
runs for `ui` callers and is refused for the others by the add-on's own check, which is the rule that
matters here.

Why the tools with `stores_data` run without asking: `stores_data` is not a consent effect, and
`accesses_network` asks no one for an MCP client (`docs/addons.md`, "Effects and consent"). That is
why the caller check in section 4.1 is the only gate, and why it must hold.

These tools are for CSM's own UI, the palette and the person. A phone never calls them.

---

## 4. Op to CSM tool mapping

The add-on checks the device's level with `Op::required_level()` (already done in `msg.rs`), then
validates the arguments (`Op::validate()`), then maps the op. Policy (is this folder allowed, is this
command configured, is this profile known) is the add-on's own, read from its config. Every mapping
is one fixed tool, and no argument from the phone reaches a CSM tool unchecked.

| Op | Level | CSM tool(s) | Notes |
|---|---|---|---|
| `hello` | viewer | `settings_get` (`ClaudeCommands`, for `remote_control`) | Looks the device up in the store by routing token. Unknown: `bye` with `revoked`. Replies with `HelloData` (all fields of `msg.rs`): `protocol` (the version the add-on speaks), `level`, `host_name` (the config's `host_name`, or the OS host name when empty, cleaned by the section 4.1 cleaner and capped at `MAX_NAME_CHARS`), `hide_names`, `notify`, `push_ready` (the relay has a VAPID key and this device has a subscription), `remote_control` (from `settings_get ClaudeCommands`), `max_message_bytes` (48 KiB). |
| `ping` | viewer | none | |
| `snapshot` | viewer | `session_list` (+ `inbox_list` for the count) | Starts a new `seq` (section 7). Served from the host-wide tracker's last list, when it is fresh (under 1 s old); otherwise a new `session_list`. |
| `session.list` | viewer | `session_list` | Same data as `snapshot`, one message. |
| `session.get` | viewer | `session_get` | Returns `SessionInfo` with `message` (the program's own status text). |
| `inbox.list` | viewer | `inbox_list` | `InboxItem` per entry. |
| `task.list` | viewer | `task_list` (open tasks, `include_closed` false) | Titles and state only. |
| `task.get` | viewer | `task_get` | Title, state, session. The body is **not** returned in v1 (section 13, decision 6). |
| `folder.list` | viewer | `session_recent`, `session_list` (folders), add-on config | Only folders on the add-on's allow list (exact match, section 4.1). `pinned` is true for allow-list entries marked pinned in config. |
| `profile.list` | viewer | none (add-on config) | Profiles are the add-on's own: a name, a label, a command on the PC. A phone sees name and label, never the command. |
| `command.list` | viewer | `settings_get` (`ClaudeCommands`), add-on config | The intersection: the PC's configured slash commands and the add-on's allow list. Default allow list: `/remote-control`. |
| `session.wake` | operator | `session_wake` | |
| `session.sleep` | operator | `session_sleep` | |
| `session.restart` | operator | `session_restart` | Slow: it waits for the program to exit. The reply is sent when CSM answers; the channel stays open meanwhile. It holds one global slot (section 4) while the `csm/call` is in flight. Counts against the launch limit (section 4.3). |
| `session.close` | operator | `session_close` (`force` as sent) | A busy session without `force` gets `busy` back; the phone asks again. |
| `session.new` | operator | `session_new` | `folder` is replaced by the allow-list entry's own spelling (never the phone's string). `command` comes from the profile (never from the phone). `kind` local only. Counts against the launch limit and the live-session cap (section 4.3). With `remote_control`, see the wait rule below. |
| `session.command` | operator | `session_send_command` (`command`, `force` as sent) | `command` must be on the `command.list` intersection, else `command_not_allowed`. CSM itself refuses a session mid-turn. |
| `session.status_clear` | operator | `session_status_clear` | |
| `push.subscribe` | viewer | none (device store) | The endpoint must be https and on the relay's allow list (`push.allowed_hosts`); checked here too, so a refused one never reaches the relay. |
| `push.unsubscribe` | viewer | none | |
| `settings.set` | viewer | none (device store) | `hide_names`, `notify`. Per device. |

Error mapping, from CSM's error to the phone's `ErrorCode`:
`unknown session` to `unknown_session`; a busy refusal to `busy`; a refused command to
`command_not_allowed`; a folder off the list to `folder_not_allowed`; any other tool failure to
`failed` with CSM's message (plain text, shown as is); CSM not answering (the 30 s `csm/call` timeout,
`docs/addons.md` "Messages") to `unavailable`. The phone never sees CSM's raw JSON.

**Concurrency.** Every op runs on a worker. The phone channel loop never waits for a CSM call.
- Per channel, at most 8 requests in flight; beyond that the add-on answers `rate_limited`.
- Per add-on, a **global in-flight cap of 8** CSM-bound requests across all channels (the same number
  CSM allows the add-on in `csm/call`). A request beyond it is answered `rate_limited` at once; it is
  not queued. Ops that do not call CSM (`hello`, `ping`, `push.*`, `settings.set`, `command.list` from
  config) are not counted.
- `MAX_CHANNELS_PER_HOST` (16) bounds the channels. Together, 16 channels times 8 is a bound on
  queued work per channel, and the global cap bounds the work sent to CSM.
- A `session.restart` can take a long time: it holds one global slot while its `csm/call` is in flight and
  does not hold the other requests of its channel.
- **The `remote_control` wait holds no slot and no reply.** `session.new` with `remote_control` is answered at
  once when `session_new` returns: `ok` with `remote_control: "pending"` in `data`. The global slot is released
  then. The wait for idle is a timer inside the add-on (a per-session entry in a pending-waits table, checked on
  each tracker refresh and on a 5 s tick). When the session reports idle, the add-on calls
  `session_send_command` with `/remote-control`, which takes a slot for that call only. When that call ends, or
  the wait ends at 2 minutes, the add-on sends one `Event::RemoteControl { session, ok, code, message }` on the
  channel (`code` and `message` only when `ok` is false; the session stays). That event is a `msg.rs` addition in
  step 1, with its vector line. If the channel has closed by then, the event is dropped and the phone reads the
  setting on its next `session.get`. At most **2 pending waits per device** and **8 in total**; a further
  `session.new` with `remote_control` is answered `rate_limited` before `session_new` is called. So eight such
  requests never block the other CSM-bound ops, and none of them holds a request id after its reply.
**Calls from the add-on** use `csm/call` as in `docs/addons.md` ("Process add-ons", "Messages"). Their
results go back to the request that asked. A `csm/call` that is not answered in 30 seconds fails with
`unavailable`.

### 4.1 Attacker-facing rules

These hold whatever the relay, a phone or the UI does.

- **Caller check on add-on tools.** `phone-remote.pair`, `phone-remote.revoke` and
  `phone-remote.enable_access` refuse every caller whose `kind` is not `ui`. Refused callers are
  `cli`, `external` (MCP), `addon` (another add-on or this add-on's own `csm/call`) and `peer` (a
  paired peer device). The refusal is a plain tool error. Level changes need no tool: they are a
  screen event, and only the person's clicks on the add-on's own screen send events.
- **The offer never leaves the screen.** The offer URL (it carries the pre-shared key) and the PSK
  never appear in a tool result, a `csm/log` line, `plugin_status`, a `csm/status` text or any file
  except the screen tree sent to CSM for the pairing screen. Step 1 checks that no core tool that
  reads screen trees is open to MCP callers; if one is, the QR node is hidden from it.
- **Revoke and level changes close channels on the host.** Revoke removes the device from
  `devices.json`, sends the new `Allow` to the relay and sends `Bye { revoked }` on each open channel
  of that device, then closes those channels itself. A level change does the same with
  `Bye { level_changed }`. The host does not rely on the relay: a hostile relay can ignore `Allow`.
- **Folder matching is exact on canonical paths.** Each allow-list entry is canonicalised when the
  config loads (absolute, normalised, the Windows case folded as the OS does). A requested folder is
  canonicalised the same way and must equal an entry; a prefix never matches, and a path with `..`
  after canonicalisation is refused. The add-on passes the entry's own spelling to `session_new`.
- **Session names are cleaned.** `msg.rs` `clean()` does not strip bidi or zero-width characters.
  Before a session name goes to a phone or a push, and before a name from `session.new` is passed on,
  the add-on removes every control and format character (Unicode categories Cc and Cf, including
  U+202A to U+202E, U+2066 to U+2069, U+200B to U+200F, U+2060 and U+FEFF). The same clean function
  applies to a profile label and to the names in `devices.json` when they are shown.
- **Pairing limits** (section 8): 5 attempts per slot, one pairing channel at a time, 10 minutes per
  slot, one use.
- **Push endpoints**: https and on the host list (section 4, `push.subscribe`), checked on the add-on
  side before the relay sees them. The list is `config.json` `push_hosts`, with a default of the known
  browser push services (`fcm.googleapis.com`, `updates.push.services.mozilla.com`, `web.push.apple.com`,
  `*.notify.windows.com`). The relay keeps its own list as the second check; the two may differ, and
  the stricter one wins.
- **Launch limits** (section 4.3): the rate and the live-session cap hold whatever a phone asks.

### 4.2 Threat model

Actors and what each can do.

- **A hostile relay.** It sees metadata: the host id, the timing and sizes of frames, the hashes of
  device routing tokens, the push endpoints it forwards. It cannot read or forge Noise messages, and it
  cannot make a phone trust a different PC, because the phone pins `host_pub` from the QR. It can drop,
  delay or replay frames (Noise rejects replays), refuse pushes, or ignore `Allow`. The host closes
  revoked channels itself (section 4.1). Out of reach: hiding that the PC talks to the relay.
- **A hostile phone (a paired device).** A viewer can read the list and the detail that the level
  allows. An operator can operate allow-listed sessions and commands, and nothing else. It cannot read a
  terminal, a screen, a diff or a task body. It can flood reads and other ops: the per-channel and global
  concurrency caps bound how many run at once, and a `rate_limited` answer costs it nothing else. The
  concurrency caps do **not** bound rate. A `session.new`, `session.wake` or `session.restart` returns
  quickly, so concurrency alone would let an operator phone launch agent sessions one after another. The
  launch bucket (6 a minute, 30 an hour, per device) and the live-session cap of 5 (section 4.3) bound
  that cost. A stolen operator phone therefore costs at most the bucket's rate and the cap's sessions, and
  the person can revoke it at the PC.
- **A person in the office.** Shoulder-surfing or photographing the QR during its 10 minutes can let
  that person offer a pairing. The Allow step is on the PC, after the check code matches on both
  screens; a stranger's phone shows a request the person can deny. The slot is one use and expires.
  Out of reach: a person who photographs the QR and is at the PC when the check code matches.
  **The race.** A photographed QR lets an attacker's phone open the one pairing channel first. The check code
  does not help there: the attacker's phone shows the same digits the PC shows. The relay allows one pairing
  channel per slot and refuses a second one with `not_allowed` before the host sees it
  (`relay/src/state.rs`, `open_channel`). So the add-on never learns that a second phone tried, and it cannot
  count the attempt or answer it with a Busy reason (there is none: `DenyReason` only exists inside a completed
  Noise handshake). The defence is on the screens (section 8). The owner's phone, refused during someone else's
  pairing, shows "Pairing is in use by another phone, or this code has expired or been used. If another phone
  is pairing, check the PC and press Deny there." The PC's pending request shows the device name the phone sent,
  as that phone's own claim, not as a fact. The owner's refused phone cannot confirm the check code, so when it
  reports that the pairing is in use and the PC shows a request nobody started, the answer is Deny. Deny closes
  the channel, and the person then pairs again with a new slot.- **A same-user shell agent** (an agent or a program running as the person's Windows or Unix user,
  including an MCP agent with a shell). It can read `host-key`, `devices.json`, `config.json` and the
  keychain entries the user owns, and it can run `csm-phone` or CSM tools by itself. The owner-only
  file permissions do not stop code running as the same user. **Out of reach**: nothing in this add-on
  keeps the device store or the host key from same-user code. The design does not claim otherwise. A
  copied host key lets that code impersonate the PC to a phone that is already paired, and a copied
  `devices.json` lets it know the routing-token hashes; it does not, by itself, let it pair a new phone
  (that needs the Allow on the PC).
- **A compromised PC** (malware with the person's rights, or an administrator). Out of reach.

### 4.3 Launch limits (against a hostile or stolen operator phone)

Concurrency caps bound how many calls run at once, not how many run in a day. `session_new`, `session_wake`
and `session_restart` start agent programs that cost CPU, memory and paid API use, so they have a rate:

- **Per device, one launch bucket** for `session.new`, `session.wake` and `session.restart` together:
  **6 per minute and 30 per hour**. The (N+1)th launch inside the window is answered `rate_limited`
  before any CSM call is made. The counters are in memory and reset when the add-on restarts, which is
  acceptable because a restart also needs the PC's user.
- **A cap on live sessions phones created.** The add-on records the id of every session that a
  `session.new` created, and counts only those that `session_list` still shows as live (not `exited`).
  Default cap: **5** (`config.json` `max_phone_sessions`, range 1 to 20). A `session.new` at the cap is
  answered `rate_limited`, and `session_new` is not called.
- **Revoke clears nothing**: a revoked device's sessions keep running (the person closes them), but its
  bucket is gone with the device.
- These limits are the add-on's own policy and are not CSM's. The phone cannot change them.

---

## 5. The add-on's own storage

Folder: `<state folder>/addon-data/phone-remote/` (the `data_dir` of `initialize`; the folder is
created by CSM when the process starts, and the add-on creates its files below).

**`data_dir` is `null`** when the run has no state folder, under `--clean`, or in an isolated run
(`docs/addons.md`, "Start and stop"). Then the add-on **refuses to serve**: it reports "no data folder,
so the phone remote cannot keep its pairings; start CSM normally" in `plugin_status`, opens no relay
connection and offers no pairing. Nothing is written in that case.

| File | Contents | Permissions | If damaged |
|---|---|---|---|
| `host-key` | the host's X25519 static private key (32 bytes, raw, no text) | owner-only (0600; Windows: one ACE for the current user, as `fsperm.rs`) | **error**: the add-on stops serving and says so. It is never replaced. |
| `devices.json` | `{"version":1,"devices":[...]}` (see below) | owner-only | **error**, the same. A backup `devices.json.bak` (the last good copy, written before each change) is shown in the error, and the person may restore it. Nothing is restored by itself. |
| `config.json` | the relay URL, the app origin, the folder allow list, the profiles, the command allow list, `enabled` (the second switch), push on or off, `push_hosts`, `max_phone_sessions`, `host_name` | owner-only | error: the add-on runs with defaults (no relay, nothing allowed) and says why. **The first settings write** does not overwrite it: the damaged file is renamed to `config.json.damaged-<time>` first (the same rule as `devices.json`), the screen says so, and the new file is written atomically. Nothing is replaced silently. |
| `push.json` | not a file: push subscriptions live in the device store per device. | | |

**First run, and the missing-file cases.** Decided by which files exist, in the data folder:

| `host-key` | `devices.json` | Result |
|---|---|---|
| absent | absent | **first run**: create `host-key` (new random key, 0600) and an empty `devices.json`. Both are written atomically. |
| present and valid | present and valid | normal |
| absent | present | **error**: the key is gone but phones are paired. Nothing is created. The screen offers the explicit recovery (below). |
| damaged | any | **error** (the file is not touched) |
| present and valid | absent | **error**: the device list is missing. Nothing is created: a silently empty list would hide that phones are paired. The screen offers the explicit recovery. |

Recovery, for the two error rows: "Start a new device list" keeps the damaged or missing files as
`<name>.damaged-<time>` where they exist, creates a new key and an empty list, and removes every
pairing. It needs a confirm. Nothing does it automatically.

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
  appear: "Open the folder" and "Start a new device list" (above). The second one needs a confirm.
- A "damaged" file with a name that is not `phone-remote`'s own is never touched.

**Secrets.** The relay enrolment token (when the relay uses one) and the relay host secret (32 bytes,
base64url; its hash is the host id) go through `addon_secret_set` / `addon_secret_get`, with keys
`relay.enrol_token` and `relay.host_secret`. They never go into `config.json` or `state.json`. The
host id the relay checks is a hash of the host secret, so losing it changes the host id: the same
"explicit recovery" rule applies.

The enrolment token goes from the settings screen straight to `addon_secret_set` (section 10.5). The
add-on never writes it to a file, a log, `plugin_status` or the screen tree: the `text_input` uses
`submit` (not `change`), and the screen is re-rendered with `reset: true` so the field empties.

The host key is a file, not a secret in the keychain, because the brief asks for the device store and
host key as files (decision 4 offers the keychain for both).

**Path rules.** Every path is built inside the data folder from fixed names. No path comes from a
phone, a relay message or a config value.

---

## 6. The relay connection

- One outbound WebSocket to `wss://<relay>/v1/host`. Plain `ws://127.0.0.1` is allowed only when
  `config.json` `app_origin` is `http://127.0.0.1:<port>` (the dev setup of section 11), so a production
  config never reaches a `ws://` relay. The relay URL is in `config.json`.
- Hello: `HostToRelay::Hello { v, host_id, host_secret, access }`. `access` is the enrolment token,
  when set, read from the keychain.
- Then `Allow { tokens }` with the hashes of every device's routing token (`token_hash`), and
  `PairingOpen` only while the pairing screen has an open slot (at most 600 s).
- Phone channels are binary `HostFrame`s (`version | kind | channel u32 BE | payload`). The add-on keeps a
  map from channel to device and enforces one Noise session per channel.
- Reconnect with backoff 1, 2, 5, 10, 30 s (as `relay/README.md`). After every connect the add-on
  sends `Allow`, and an open pairing slot again, since the relay forgets them when the host disconnects.
- Silence: the relay drops a host silent for 90 s. The add-on sends a WebSocket ping every 30 s.
- Threads, not an async runtime: one reader and one writer thread per socket, bounded channels
  (64 frames per phone channel). A full queue closes that channel with `rate_limited`, not the host.
- The relay connection exists whenever `enabled` and the second switch are on. It does **not** depend on
  a phone being connected: the push tracker (section 7.1) uses it.

Noise (the add-on side uses `snow`):
- Pairing: `Noise_IKpsk2_25519_AESGCM_SHA256`, the add-on is the responder, the prologue
  `csm-remote/1\0<host_id>`, PSK from the offer, msg 1 carries the phone's `PairMsg::Hello`.
- Devices: `Noise_IK_25519_AESGCM_SHA256`, msg 1 may carry the phone's hello JSON. The add-on accepts a
  static key only if its routing token and static key match a device-store entry.
- Rekey after `REKEY_INTERVAL` (10 000) messages per direction. A channel is closed after
  `MAX_CHANNEL_SECS` (3600) so the phone reconnects.
- Every app message is JSON of at most 48 KiB (`MAX_APP_MESSAGE_BYTES`); `msg::encode` refuses bigger.

---

## 7. Session deltas and the host-wide tracker

Two parts: one tracker for the whole add-on (it does not depend on any channel), and per-channel
delta state that only exists while a phone is connected.

### 7.1 The host-wide tracker

Runs while the add-on is running and the relay connection is on (section 6). It is the only place that
decides pushes.

1. It subscribes to `session_changed` and `inbox_changed` in `initialize`. An event refreshes the
   tracker's list. Refreshes are **coalesced** globally: at most one `session_list` per 200 ms, whether
   or not any phone is connected.
2. Each refresh calls `session_list` once and stores the result as `last_list` with a time.
3. It keeps, per session: `last_status`, `since` (below), and the **episode flag** `notified`.
4. A status change **into** `needs_you_approval` or `needs_you_question` opens a needs-you episode for
   that session. Its `notified` flag is false, so it produces one push request per device (section 9).
   The episode closes when the status leaves the needs-you group; a new one later opens a new episode.
5. **Baseline.** The first refresh after the add-on starts sets every session's `last_status` and
   `notified` (true for any session already needing you) and sends **no** push. A restart must not
   push again for a session that was already waiting. This is a rule, not a decision.
6. Pushes are decided per device here (section 9), so `notify`, `hide_names` and the subscription are
   read from the device store at decision time.

`since` is stamped by the tracker: when it first sees a session's current status, it records the add-on's
own clock (Unix seconds). After an add-on restart, `since` is the time of the first refresh, not the
time the status really changed. The phone shows "since" as that value, and the design does not claim it
is exact.

### 7.2 Per channel

Each paired phone channel has its own state in the add-on: `last_sent: map<session id, SessionInfo>`,
`seq`.

1. The tracker tells each open channel that the list changed (a notification inside the add-on, not a
   CSM call). The channel diffs its own `last_sent` against the tracker's `last_list`: upserts for new or
   changed entries, `removed` for ids no longer present.
2. A `SessionInfo` is changed when any of `name` (cleaned, section 4.1), `folder`, `branch`, `status`,
   `group`, `since` differs. `profile` is **not** compared: `session_list` does not return a profile, so
   v1 never sends one (the field stays absent, and the phone shows nothing for it). `message` is never
   sent by a delta: it needs `session_get`, which only a phone that asked for a session's detail does.
   (So a detail screen shows the message; the list does not.)
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

## 8. Pairing and devices: the screens, the QR code and the indicator

Three add-on screens (`docs/addons.md`, "Screens": at most 4 per add-on). The add-on declares them in
`initialize`:

```json
"screens":[
  {"id":"phone-remote.pair","title":"Pair a phone","icon":"phone","scope":"global","summary":"Pair a phone with this PC"},
  {"id":"phone-remote.devices","title":"Phones","icon":"devices","scope":"global","summary":"Paired phones and their levels"},
  {"id":"phone-remote.settings","title":"Phone remote settings","icon":"list","scope":"global","summary":"Relay, folders, profiles, commands and push"}
]
```

Icons must exist in CSM's icon set (`crates/csm-ui/src/icon_data.rs`; an unknown one draws none). That
set has `monitor`, `list` and others, but **no `phone` or `devices`**. Step 1 adds those two icons to
`icon_data.rs` (a small CSM-side change, listed with the other additions in section 8.3). The
settings screen uses an existing icon.

**Opening the pairing screen.** The `phone-remote.pair` tool (ui caller only, section 4.1) calls
`screen_open` through `csm/call`. `screen_open` is a `ui` tool that runs directly (`docs/tools.md`, the
`screen_open` row). Opening the screen opens **no** pairing slot.

**Pairing screen** (`phone-remote.pair`), before a slot is open:

```
Pair a phone                                         [Add-on: Phone remote]
Press "Start pairing", then scan the code with the phone or open the link on it.
[ Start pairing ]                                    (button, event: true)
```

While a slot is open:

```
Pair a phone                                         [Add-on: Phone remote]
Scan this code with the phone, or open the link on it.
[ QR code of the offer URL ]                 (qr widget, section 8.2)
https://phone.umage.example/v/0.1.0/#p=...   (text, mono, selectable)
Expires in 9:42 · the code can be used once  [ Stop pairing ]
--- after the phone connects ---
Device: Pixel 9      Check code: 8216
"Read the same four digits on the phone. If they differ, press Deny."
Level:  [ Viewer | Operator ]                (segmented, key "level")
[ Allow ]  [ Deny ]                           (buttons, event: true)
```

- The screen is a **tree** the add-on builds and CSM draws; the add-on never draws. Every control sends
  an event: `click` for Start, Stop, Allow and Deny, `select` for the level. There is **no CSM confirm**
  on any of them: `action_policy` runs an add-on's own tools directly, and the buttons send events, not
  tool calls. Allow is the confirm: it is only enabled once the check code is shown.
- "Stop pairing", or expiry, sends `Deny` (`denied` or `expired`) and closes the slot with
  `PairingClose`. Deny does the same.
- On Allow, the add-on creates the device (random `device_id`, 32-byte `routing_token`, stores
  `token_hash` and the phone's static key), appends it to `devices.json`, sends
  `Allow { tokens }` to the relay, sends `Accept { routing_token, level, device_id }` over the channel
  and closes the slot. The order matters: the device is saved before `Accept` is sent, so a crash never
  leaves a phone paired that the PC does not know.
- Limits from `limits.rs`: 5 attempts per slot, one pairing channel at a time, 10 minutes per slot.
  A sixth attempt is refused and shown on the screen.
- **A second phone while one is pending.** The relay refuses the second pairing channel with `not_allowed`
  (section 4.2), so the add-on never sees it: it does not count refused attempts and the PC shows no count
  line. The refused phone maps `not_allowed` during pairing to "Pairing is in use by another phone, or this
  code has expired or been used. If another phone is pairing, check the PC and press Deny there." (not the
  generic text). The PC's pending request keeps its own device name and check code. If the add-on itself sees
  a second pairing channel on its relay connection (a hostile relay forwarding one), it closes that channel
  with `not_allowed` and does not run Noise on it. A distinct relay code and a PC count line would need a relay
  change, which v1 does not make (section 13, decision 15).- The check code is `check_code(handshake_hash)` on both sides. The screen says: "If the digits differ,
  press Deny." That is the whole defence against a swapped QR.
- The offer URL and PSK go only into the screen tree (section 4.1). They are never in a log or a result.

**Devices screen** (`phone-remote.devices`): a `list` of devices (`title` = name, `subtitle` = level,
last seen as "5 min ago", `badges` = "needs push" when notify is on but no subscription), and a detail
area with: level `segmented` (Viewer or Operator; the add-on sends `Bye { level_changed }` on the
channel, closes it, and the phone reconnects), `checkbox` "Notify me about sessions that need me", and a
`Revoke` `danger` button.

**Revoke confirm is the add-on's own.** CSM draws no confirm for it (`action_policy` runs an add-on's
own tools with no confirm). So the button is an event: the first press re-renders the detail with
"Revoke Pixel 9? [Yes, revoke] [Cancel]"; "Yes, revoke" sends a second event; the add-on then does what
`phone-remote.revoke` does: the device leaves `devices.json`, the new allow list goes to the relay, and
the add-on closes that device's channels itself (section 4.1). The phone gets `Bye { revoked }`. The
button does not call the tool, so the ui-only check on the tool does not apply to it, and the event
itself only comes from the person's click.

**Indicator.** The v1 indicator is a CSM notification the add-on sets, `csm/status` (section 8.3), not a
custom text in `plugin_status` and not a tab badge: `plugin_status` reports only state (`starting`,
`running`, `crashed`, `gave_up`, `stopped`), `last_error`, `problems` and the stderr tail, and the tab
strip has no badge. The indicator shows in Settings > Add-ons on the add-on's row, and in the title bar
as one dot (section 8.3).

### 8.1 Whether the screen protocol can show a QR code

**No, not today.** The widgets are listed in `docs/addons.md` ("The widgets"): there is no image node.
`markdown` shows no images (alt text only). `code`, `diff` and `text` are text. A screen cannot carry a
picture, so the add-on cannot send a PNG of the QR code, and a data URI would not help either.

### 8.2 The smallest addition: a `qr` node

A new node, drawn by CSM:

```json
{"type":"qr","key":"offer","text":"https://phone.umage.example/v/0.1.0/#p=...","label":"Scan with the phone","size":"md"}
```

- `text`: required, 1 to **1024** bytes, no control characters. Longer, or bad text, is a bad prop: the
  node draws a notice and nothing else breaks. The cap is 1024 because the worst case is about 615
  bytes: a 256-byte relay URL (the maximum `PairingOffer::validate` allows) gives 432 bytes of JSON, a
  576-byte payload and a 615-byte URL with an example origin. 512 would not hold it. Step 1 adds that
  worst case as a unit test.
- `size`: `sm`, `md` (default), `lg`, as token-driven pixel sizes in `csm-ui`'s theme. Colours are
  the theme's `text` on `base` (a QR is dark on light in both themes; the quiet zone is drawn).
- `label`: optional caption under it.
- Error correction: **at least M**. `qrcodegen`'s `encode_text` raises the level when it fits, so the
  matrix may carry more than M; it never carries less. The peer screen keeps `Low`. The move makes the
  level a **parameter**: `qr_modules(text, ecc)`.
- The pure function moves from `csm-ui/src/screens/remote.rs` to `csm-core::qr`, behind a `qrcodegen`
  dependency added to `csm-core` (pure: a string in, a grid out). The peer screen calls the moved
  function with `Low`, so its output does not change. The screen parser in `csm-core/src/addons/screen`
  uses it. Only `csm-ui` draws it.
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

### 8.3 CSM-side additions in scope for step 1 (for review)

These are CSM changes, so each needs its own CSM-side review section. They are in v1 because the owner
asked for an indicator and a pairing screen with a QR code, and neither works without them.

1. **`csm/status`** (a new process-to-CSM notification, like `csm/log`):
   `{"method":"csm/status","params":{"text":"Connected to relay, 2 phones","tone":"ok"}}`.
   - `text`: plain text, 1 to 80 characters after cleaning (section 4.1); `tone`: `ok`, `warn`, `error`
     or `idle`. Anything else is dropped and listed in `problems`.
   - Latest wins per add-on. CSM coalesces sends to one per 500 ms per add-on. The text is held in
     memory only, not in `state.json`, and is cleared when the add-on stops.
   - Shown in two places: the add-on's row in Settings > Add-ons (text and tone), and the title bar as one
     dot for the worst tone of all running add-ons, with the add-on's text as a tooltip. Clicking the dot
     opens Settings > Add-ons. The dot is hidden when every tone is `idle` or no add-on set one.
   - Review points for CSM: a new `csm/` method (any other method is still answered `-32601`); a
     capability name `status` in `initialize` so an add-on can check it; the text is drawn as plain text
     (never as markup); no tool exposes it to MCP callers (it is output, not a tool).
2. **Two icons**, `phone` and `devices`, in `crates/csm-ui/src/icon_data.rs`. Review: SVG paths in the
   same style as the existing set.
3. **The `qr` node** (section 8.2): the parser in `csm-core/src/addons/screen`, the drawing in `csm-ui`,
   the moved `csm-core::qr`.

### 8.4 Other small additions to propose (not built here)

- `copy` on a `button`: `{"action": {"copy": "text"}}`, a button that copies its text (a link is
  otherwise hard to copy on a phone-less screen). Optional; the selectable text covers v1.

---

## 9. Push notifications

Chain: CSM status change, the host-wide tracker (section 7.1), RFC 8291 encryption in the add-on, relay
POST, push service, phone service worker.

1. **Trigger.** A session's status changes **into** `needs_you_approval` or `needs_you_question` (the
   tracker's episode, section 7.1). It does not depend on any phone being connected: a phone that is
   closed or asleep still gets the push. One push request per device per episode. Pushes to one device
   are coalesced to at most one per 30 seconds: a push held by the window is sent when it opens, with the
   current count `n` and, if names are not hidden, the newest session's name. The relay caps 60 per host
   and 20 per endpoint per hour as a second line.
2. **Per device.** A push goes to a device only if: the device is paired, `notify` is on, it has a
   push subscription, `push` is on in `config.json`, and the relay connection is up. If the relay is
   down, the push is dropped and counted in the log (counts only, never names).
3. **Payload** (`PushPayload`): `{"t":"needs_you","n":<sessions needing you>,"s":"<name>","sid":"<id>"}`.
   With **hide names** on for that device, `s` and `sid` are left out: the notification says
   "2 sessions need you" and no names. `n` is always sent.
4. **Encryption.** The add-on encrypts the JSON as an RFC 8291 `aes128gcm` body (ECDH on P-256 with a fresh
   ephemeral key, HKDF-SHA256, AES-128-GCM, 4096-byte record size), using the phone's `p256dh` and `auth`
   from `push.subscribe`. `p256`, `hkdf`, `aes-gcm`, `sha2` are already in the lock file. A test vector
   from RFC 8291 section 5 is the unit test.
5. **Send.** `HostToRelay::Push(PushRequest { id, endpoint, ttl: 3600, urgency: high, topic: "needs-you", body })`
   over the host socket. The relay adds the VAPID header (its own key) and POSTs. It sends back `PushResult`.
   A refused or failed push is logged by counts only, and the device is not removed for it. The relay's
   `PushResult` carries `ok`, the push service's HTTP `status` and a `code` (`push_failed` or `push_refused`).
   On a status of 404 or 410 the add-on drops that endpoint from the device, since the subscription is gone.
6. **Browser.** The browser decrypts the body (the Push API does, for `aes128gcm`), and the service worker
   shows `showNotification(...)`. The notification text comes from the payload, nothing else. The tag is
   `needs-you`, so a newer push replaces the older one instead of stacking.
7. A tap opens the app at the session list.

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

**Origins and the CSP.** The app runs on the umage static domain, and it must reach **whatever relay the
offer names**, because self-hosters point their PC at their own relay and the phone uses the umage app.
So the CSP cannot pin one relay at build time. The chosen approach (decision 11 in section 13, the
recommended option):

- The production CSP is `default-src 'self'; connect-src 'self' wss: https:; img-src 'self' data:;
  style-src 'self'; script-src 'self'`. No third-party script, font or CDN. The production app refuses an
  offer whose relay is `ws://`, always.
- The **dev** server (`phone/tools/serve.mjs`) sends its own CSP, with `connect-src 'self' ws://127.0.0.1:*`
  and nothing else added. It is the only place `ws://` is allowed, and it is used only with an
  `app_origin` of `http://127.0.0.1:<port>` (section 11). The production CSP is never served on
  127.0.0.1 and the dev CSP never on the umage domain.
- The relay comes from the offer. The app accepts a relay URL only if it is `wss://`, or `ws://127.0.0.1`
  when the page itself is the dev origin, and it sends no token to a relay before the host's key from the
  QR is pinned. A relay that is not the PC's own cannot read or forge a Noise session, because `host_pub`
  comes from the QR.
- The offer's **app origin** is set by the PC's `config.json` key `app_origin`. Its default is the umage
  site. It must be `https://` or `http://127.0.0.1:<port>`, validated on the PC, and it is the origin that
  appears in the QR. A self-hoster who runs their own copy of `phone/` sets it; one who uses the umage app
  does not.

The alternative (decision 11, not recommended): self-hosters must deploy `phone/` as well, and the CSP
pins their relay at build time. It is safer against a compromised page and more work for every
self-hoster.

The QR scanner, if used, is the browser's `BarcodeDetector` when present, else paste.

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

## 10.6 The PC's settings screen (`phone-remote.settings`)

The CSM-side screen the person uses to set up the add-on. It is the only place the relay URL, the
enrolment token, the folders, the profiles, the command allow list and push are set. Without it, the
person would have to hand-edit `config.json`, and the enrolment token would have no path to
`addon_secret_set`. Every control sends an event to the add-on, which validates it and writes
`config.json` or the keychain. The screen only draws what the add-on sends.

- **Relay.** A `text_input` for the relay URL (`submit`), a "Use the umage relay" `button` that fills the
  URL the owner provides (decision 2; until the owner gives one, the button is hidden), and a status line
  from `csm/status`. The URL must be `wss://`, or `ws://127.0.0.1` for dev.
- **Enrolment token.** A `text_input` with `submit` and `submit_label` "Save token", `reset: true`, and no
  `on_change`. On submit the add-on calls `addon_secret_set` (`relay.enrol_token`) and re-renders with the
  field empty. The token is never echoed, never rendered back, never logged. The screen shows only
  "A token is saved" or "No token".
- **Folders.** A `list` of allow-list entries with a remove `button` each, and a `text_input` (`submit`)
  to add one. The add-on canonicalises the entry and refuses it unless it is an existing directory. A
  "pinned" `checkbox` per entry (for `folder.list`).
- **Profiles.** A `list` of profiles (name, label). A form: name (1 to 32 characters, letters, digits and
  `-`), label (max 64), and command. The command is stored in the form `session_new` takes (one command
  string, which CSM runs as the program's own command: the WPF rule for a user's own command applies).
  The phone never sees the command. Remove and add are `button`s.
- **Commands.** A `checkbox` per slash command from `settings_get ClaudeCommands` (only the ones the PC
  lists). The default is `/remote-control` checked. The add-on never offers a command the PC does not list,
  because `command.list` intersects the two lists (section 4).
- **Push.** A `checkbox` "Send push notifications for sessions that need me" (`push`, default on).
- **Access.** The second switch: a `checkbox` "Connect to the relay" (`enable_access`, section 3). The
  add-on's enable is the first switch, made in CSM's Settings > Add-ons; this one connects.
- **App origin.** A `text_input` (`submit`) for `app_origin` (default: the umage site). The add-on
  validates it (https, or `http://127.0.0.1:<port>`).

The device list and the pairing screen are the other two screens (section 8).

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
  workers both work on `127.0.0.1` (a secure context). The offer's app origin is `http://127.0.0.1:8080`,
  set in the dev `config.json` as `app_origin`. `serve.mjs` sends the dev CSP of section 10.1
  (`connect-src 'self' ws://127.0.0.1:*`), so the page can reach the dev relay on 127.0.0.1:8787. Only
  this server sends it; the production host sends the production CSP.
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

Each line says what was checked, against which file.

- **Add-on effects.** The add-on's tool effects may only name the effects `[process]` declares
  (`docs/addons.md`, "Tools"). Section 3 uses only those names. `ui` is not one of them, so no add-on tool
  can open a screen or be routed through `view_overlay.rs`. (Review round 1 found the old names `ui`,
  `state` and `read`; they are gone.)
- **Caller kinds.** `ui`, `cli`, `external`, `addon`, `peer` (`crates/csm-engine/src/plugins.rs`,
  `caller_json`; `docs/addons.md`, "Who is calling"). Section 4.1 refuses every kind but `ui` for the three
  tools. Tests cover each refused kind (section 16, step 1).
- **Standing grants close decision 9.** `crates/csm-engine/src/engine.rs` (the consent check): a call from
  `Caller::External` whose client is `add-on <id>` skips the consent card when `plugins::standing_call`
  finds the tool in that add-on's `[process] calls`, whatever the tool's effects. The `calls` list excludes
  only a fixed set of names (`docs/addons.md`, "Standing consent"), and `session_send_command` is not in
  it. So `session_send_command` runs through `calls`, and no card is needed. The grant is still the
  person's: it is in the enable dialog, decision 3. The one thing that still applies: a tool that refuses
  add-on callers still does (`precheck`, `level_gate`).
- **Screen opening.** `screen_open` is a `ui` tool that runs directly (`docs/addons.md`, "Tools" and the
  `screen_open` row under "The tools"). It is in `calls` so the pairing tool can open the screen. Step 1
  checks that `precheck` does not refuse an add-on caller for it; if it does, the pairing tool must be
  refused and decision 12 applies.
- **Screen buttons and tools.** `crates/csm-core/src/addons/screen/mod.rs`, `action_policy`: an add-on's
  own tools run `Direct`, so a button that runs one has no confirm. The design uses events for every
  button (sections 8 and 10.6), so no screen button runs a tool.
- **Screen count and icons.** At most 4 screens (`docs/addons.md`, "Declaring"). The icon set is in
  `crates/csm-ui/src/icon_data.rs`; `phone` and `devices` are not in it (step 1 adds them).
- **Relay timeouts.** The add-on's `csm/call` timeout is **30 seconds** (`docs/addons.md`, "Messages"), not
  10. The screen/open answer timeout is 10 seconds (`docs/addons.md`, "Declaring"), which is separate.
- **Offer URL length.** A `PairingOffer` is six short fields (`v`, `r`, `h`, `k`, `i`, `p`), base64url of
  the JSON. The worst case is a 256-byte relay URL (the maximum `PairingOffer::validate` allows): 432 bytes
  of JSON, a 576-byte payload, a 615-byte URL with an example origin. The `qr` text cap is therefore **1024**
  (section 8.2), not 512. Step 1 adds this worst case as a unit test on the `qr` node.
- **QR error correction.** `qr_modules` in `crates/csm-ui/src/screens/remote.rs` uses `QrCodeEcc::Low`. The
  moved function takes the level as a parameter (section 8.2). The design does not say "M from `qr_modules`".
- **`msg.rs` delta and inbox lines.** `msg.rs` says the relay's `Snapshot` splits and that a `Delta` is
  "changes since seq-1". Section 7 relies on consecutive seqs for split deltas and on the inbox sharing the
  seq. Both need a line in `msg.rs` (step 1).
- **Session fields.** `session_list` returns `id, name, kind, folder, group, status, branch,
  remote_control, remote_control_url` (`docs/tools.md`). It returns **no** `since` and **no** `profile`.
  `since` is stamped by the add-on's tracker (section 7.1). `profile` is not compared and is not sent in v1.
  `SessionInfo` keeps both fields as optional.
- **Status values.** `session_list` returns `working`, `needs_you_approval`, `needs_you_question`,
  `done_unseen`, `error`, `idle`, `asleep`, `starting`, `exited` (`docs/tools.md`). `PhoneStatus` has seven
  values. The map: `working` and `starting` to `working`; `needs_you_*` as is; `done_unseen` to `done`;
  `error` to `idle` with the message (decision 8: or a new `PhoneStatus::Error`, which is a `msg.rs` change);
  `idle`, `asleep`, `exited` as is.
- **`session_new` takes a command string.** Its parameters are `folder, command?, name?, group?, kind?, ...`
  (`docs/tools.md`). The profile stores the command as one string; no argv is joined, and the add-on never
  builds a shell line from the phone's input.
- **`session_send_command`** types only the `ClaudeCommands` list (`docs/tools.md`), so the phone's
  command allow list can never be wider than the PC's list.
- **`profile_list`** is Windows Terminal's profiles, not the phone's `profile` concept. The phone's profiles
  are the add-on's own (section 4). Decision 5 confirms this.
- **Secrets.** `addon_secret_set`, `addon_secret_get`, `addon_secret_delete` are refused for every caller
  except the add-on that stored the secret (`docs/addons.md`, "Secrets"). They need `stores_secrets`
  declared, not `calls`.
- **Data folder and `null`.** `data_dir` is `null` under `--clean`, in an isolated run and with no state
  folder (`docs/addons.md`, "Start and stop"). Section 5 refuses to serve in that case.
- **`xtask/src/policy.rs` `DENY_ROOTS`** lists the crates whose roots deny the clippy lints. `apps/csm-phone`
  is added there in step 1.
- **Browser X25519 support** is a claim to verify in step 2 against current compatibility tables, not from memory.

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
5. **Phone profiles.** The phone's `profile` is an add-on-owned list (name, label, command on the PC), not
   Windows Terminal's profiles. Confirm.
6. **Task bodies.** `task.get` returns title and state only in v1. The body (a Markdown string written by
   people or agents) would be untrusted text on a phone; the owner decides whether to send it (and then
   with the bidi and control-character markers).
7. **Inbox and seq.** Share the channel's seq counter (recommended, section 7.2) or a separate one.
8. **`error` status.** Map to `idle` with the message (no `msg.rs` change), or add `PhoneStatus::Error`.
9. **Closed by reading the code.** `session_send_command` runs through `[process] calls` without a consent
   card (section 12, "Standing grants"). No owner decision is needed beyond decision 3, which approves the
   list. If the owner wants a card per input anyway, that is a change to `calls` for this one tool.
10. **Relay admission for umage.** The relay's `access.allowed_hosts_file` or an enrolment token for umage's
    own PCs (section 14).
11. **Connect-src for the web app.** The recommended option (section 10.1): `connect-src 'self' wss: https:`,
    with the relay taken from the offer and `app_origin` in `config.json` for self-hosted copies of `phone/`.
    The alternative pins one relay at build time and makes every self-hoster deploy `phone/`.
12. **The CSM-side additions.** `csm/status` (the indicator), the `phone` and `devices` icons, and the `qr`
    node are in v1 (section 8.3). Recommended: yes, each with its CSM-side review. The alternative is no
    indicator in v1, which the owner's decision does not allow.
13. **Baseline on start.** The first refresh after start sends no push (section 7.1, rule 5). Recommended:
    yes. The alternative pushes for every session already waiting when the add-on starts, which repeats
    pushes after each restart.
14. **Launch limits.** Section 4.3 proposes 6 launches a minute and 30 an hour per device (`session.new`,
    `session.wake`, `session.restart` together), and at most 5 live sessions that phones created
    (`max_phone_sessions`, range 1 to 20). The numbers are a starting point for the owner to confirm;
    the design does not claim they are right for every machine.
15. **Second phone during pairing.** The relay refuses it with `not_allowed`, and the PC shows no refused-attempt
    line (section 4.2). Alternative: a relay change with a distinct `pairing_busy` code (`codes.rs`, `codes.json`,
    the relay README), a relay-to-host notice such as `RelayToHost::PairingRefused { attempts }`, the PC line, and
    step 1 tests against the real relay. Recommended: no relay change in v1; the phone's text and the PC's Allow
    screen cover it. The owner decides.

---

## 14. Owner inputs needed later (not for this step)

| Item | Why | Notes |
|---|---|---|
| Relay hosting with TLS, a DNS name (`wss://`) | the add-on and phones connect to it | Caddy in `relay/Caddyfile` does certificates; any host that passes WebSocket upgrades works |
| Relay admission for umage PCs | `access` in `relay.toml` | an allow-list file or one enrolment token (`csm-relay enrol-token`) |
| A static domain for the web app (HTTPS) | phones open the app and the QR points there | the app is plain files; any static host with the CSP above (section 10.1) |
| The umage relay URL | the "Use the umage relay" button (decision 2) | until given, the button is hidden |
| VAPID keys | Web Push | **not needed from the owner**: the relay creates its key pair on first run in `push.vapid_key_file`; the owner sets `push.vapid_subject` to a contact |
| Enrolment token for a second PC or a team | more hosts than the umage list | created with `csm-relay enrol-token` |
| Installer change (decision 1) | ship the add-on | a release step, `docs/release.md` |
| Apple developer account | not needed | the browser handles Web Push |

---

## 15. Out of scope, again (v1)

Free text and terminal access; diffs and review from the phone; task editing; multiple relays per device;
offline queues; native apps; a relay account system; hosted relays for other people; localisation; `copy`
buttons; the peer profile; a tab badge (the tab strip has none); a status indicator beyond `csm/status`
(the status bar is not built; the title-bar dot is the v1 indicator).

---

## 16. Build steps

Each step ends with the full gate (`cargo +1.97 test --workspace`, `cargo +1.97 clippy --workspace --all-targets -- -D warnings`,
`cargo +1.97 xtask ci --skip-cargo`) and a commit on `phone/addon`. Nothing is pushed.

**Step 1: the add-on, host side** (`apps/csm-phone`, `samples/addons/phone-remote/addon.toml`, `docs/design/phone-remote.md` updates, `msg.rs` doc lines, the CSM-side additions of section 8.3: `csm/status`, the two icons, the `qr` node and the moved `csm-core::qr`, and the settings screen of section 10.6).
- Device store, host key, config, first-run table (section 5), secrets (`addon_secret_*` through a trait, so tests use a fake).
- Relay client (tungstenite, threads), Noise responder (`snow`), pairing exchange, Allow, Deny, Stop.
- Caller checks for `phone-remote.pair`, `revoke`, `enable_access` (section 4.1).
- Op dispatch to CSM calls through a trait (`CsmCalls`), with the fixed mapping of section 4, the level checks and the global in-flight cap.
- The host-wide tracker (section 7.1), deltas and snapshots (section 7.2), coalescing, the split.
- Push encryption (RFC 8291), per-device decisions, sending through the relay's push request (section 9).
- Launch limits and the live-session cap (section 4.3); the `remote_control` wait timer (section 4).
- `msg.rs` `Event::RemoteControl` (section 4) with its vector line. No `pairing.rs` change: there is no `DenyReason::Busy` (section 4.2).
- Screens: pairing, devices and settings (sections 8 and 10.6); the `qr` node in `csm-core/src/addons/screen` and drawing in `csm-ui`.
- Tests (Rust), each one a real bug it would catch:
  - device store: damage (each damaged form), atomic write, the `.bak`, the first-run table (all five rows), `data_dir` null refuses to serve;
  - the level gate for every op; op validation; the fixed mapping of each op to its one tool;
  - caller refusal for each of `cli`, `external`, `addon`, `peer` on `pair`, `revoke` and `enable_access`, and that `ui` is accepted;
  - the offer and PSK absent from every tool result, `csm/log` line, `plugin_status` and `csm/status` text;
  - revoke and level change close the device's channels on the host, with the relay's `Allow` ignored;
  - exact folder matching (a `..` path, a prefix of an entry, a case variant on Windows) and the passed spelling;
  - session names stripped of bidi and zero-width characters in deltas and push payloads;
  - the global in-flight cap answers `rate_limited` and does not queue;
  - **launch limits (section 4.3)**: the 6th `session.new` inside a minute is refused, the (N+1)th inside
    the window is refused, and `session_new` is never called for it (a fake `CsmCalls` counts calls);
    the same bucket covers `session.wake` and `session.restart`; the live-session cap refuses a `session.new`
    at 5 and admits one again after a created session exits; a viewer never reaches the bucket;
  - **the `remote_control` wait**: eight such requests do not hold a global slot (a second op on another
    channel completes while they wait), and the 2 pending waits per device cap refuses the third;
  - **the pairing race (section 8)**: the tests run the real relay (`csm_relay`, a dev-dependency, on 127.0.0.1).
    With one pairing slot open and its channel active, a second phone's `Open` is refused with `not_allowed`, the
    add-on sees no second `Open`, and the first channel still completes the exchange and can Allow. A fake relay
    that forwards a second `Open` makes the add-on close that channel without running Noise on it;
  - **the `remote_control` event**: the reply is `ok` with `remote_control: "pending"` at once; the later result
    arrives as `Event::RemoteControl` (ok, and failed with its code); the event is dropped without error when the
    channel is gone;  - **damaged `config.json` first write** renames the file to `config.json.damaged-<time>` and does not
    overwrite it;
  - `HelloData` carries every field of `msg.rs`, and `host_name` is cleaned and capped;
  - the `push_hosts` check refuses a non-listed push host before the relay sees it;
  - a relay fake for the channel; the pairing exchange; deltas (gap, split, coalesce);
  - **no channel open, a session goes into `needs_you_approval`, exactly one push request reaches the fake relay**;
  - a restart does not push for a session already waiting (the baseline rule);
  - RFC 8291 vector; the QR worst case (256-byte relay URL) at the 1024-byte cap; the `qr` node parse (cap, bad text);
  - `csm/status` length cap, tone check and coalescing;
  - `DENY_ROOTS` updated.
  - `view_overlay.rs`: no change. No add-on tool carries `ui`; the table test still passes unchanged.
- Commit.

**Step 2: the web app** (`phone/`, its tests in `phone/test`, the browser smoke test).
- `noise.js` on WebCrypto, passing the vectors (section 10.3), `pairing.js`, `relay.js`, `protocol.js`, `delta.js`.
- The relay URL check (`wss:`, or `ws://127.0.0.1` for dev) and the `app_origin` rule (section 10.1).
- IndexedDB store, the non-extractable key, the device record.
- Screens (section 10.5), PWA files, service worker, push subscribe and show.
- `tsc --checkJs` clean, `node --test phone/test` passing, a Playwright smoke test against the local relay
  and the local add-on (fixture CSM).
- Commit.

**Step 3: end to end and docs.**
- `cargo xtask phone-e2e` (section 11): relay, add-on, fake CSM, the Node fake phone, push to a local fake endpoint.
- `xtask ci` gets the `phone/` check (tsc and node tests) when `phone/` exists.
- `docs/addons.md` gets a section for the phone add-on, the `csm/status` notification, the `qr` node (the widget table and the screen limits), `relay/README.md` gets the dev setup, `docs/design/phone-remote.md` status is updated to "built".
- Commit.

Each step is one commit series on `phone/addon`, and each commit message ends with the attribution lines the
brief requires.

---

## 17. Facts this note corrects or flags

- The screen protocol cannot show an image or a QR code today (section 8.1); the `qr` node is the smallest
  addition (section 8.2).
- The indicator is not a `plugin_status` text and not a tab badge (neither exists). It is `csm/status`
  (section 8.3), a CSM change in step 1.
- Add-on tool effects use only the names `[process]` declares. `ui` is not one of them, so the pairing tool
  opens its screen through `screen_open` in `calls` (section 3).
- The caller kinds are `ui`, `cli`, `external`, `addon` and `peer`. The three tools that change or open
  things refuse every kind but `ui` (section 4.1).
- Standing `calls` grants skip the consent card whatever the tool's effects. That closes decision 9 (section 12).
- `msg.rs` does not define what a delta split means or whether inbox shares the seq (section 7.2); both need
  a doc line before the add-on relies on them.
- The `csm/call` timeout is 30 seconds, not 10 (section 12).
- `session_list` has no `since` and no `profile` (section 12); `since` is stamped by the add-on.
- The QR text cap is 1024 bytes, and the EC level is a parameter (section 8.2).
- `xtask`'s `DENY_ROOTS` does not list the add-on yet; it must, when the crate exists.
- Browser X25519 support is a thing to measure, not to assume (section 10.3).
- A flood by a paired operator is a rate problem, not a concurrency one. Concurrency caps alone do not bound
  it, so section 4.3 adds the launch bucket and the live-session cap (review round 2).
- `msg.rs` `clean` only validates; the add-on's own cleaner (section 4.1) is what strips bidi and zero-width
  characters. `HelloData` has `protocol`, `level`, `host_name`, `hide_names`, `notify`, `push_ready`,
  `remote_control`, `max_message_bytes` (section 4).
- `pairing.rs` `DenyReason` has three values and gets no `Busy`. The relay refuses a second pairing channel with
  `not_allowed` before any Noise step (`relay/src/state.rs`, `open_channel`), so the phone's own text covers the
  case (section 8, decision 15). Review round 3.

---

## 18. Build status (step 1, partial)

Built in `apps/csm-phone/` (lib `csm_phone`, program `csm-phone`; layer 6; in `DENY_ROOTS`):

- `store.rs`: host key (32 bytes raw, owner-only where Unix has it), `devices.json` with the first-run table of
  section 5, damage detection, atomic writes with `.bak`, `start_new` recovery, damaged `config.json` renamed before
  the first write.
- `config.rs`: the add-on's settings, validated once at load; a remote `ws://` relay is refused.
- `policy.rs`: folder matching on canonical keys (exact, `..` refused), name cleaning (Cc and the listed Cf ranges),
  the launch bucket (6 a minute, 30 an hour), the live-session count, the global in-flight cap of 8 (refuses, never queues).
- `ops.rs` and `csm.rs`: each op's level check, then the one CSM tool section 4 names; `Op::validate` runs at parse.
  `CALLS` lists the tools the add-on may call.
- `pairing.rs`: the slot (one channel at a time, 5 attempts, 10 minutes, one use).
- `host.rs`: the protocol core with no sockets: Noise IKpsk2 for pairing and IK for paired phones (`snow`), Allow
  saves the device before `Accept`, Deny, expiry, stop, revoke and level change (the host closes the channels itself),
  channel limits, and the handling of every incoming frame without panics.
- `relay_client.rs`: the outbound WebSocket with the 1, 2, 5, 10, 30 second backoff, a ping every 30 s, and the
  allow list and open slot re-sent after every connect.

Tests: unit tests in each module; `tests/protocol.rs` drives `Host` with a test phone written with `snow` and the
shared `csm-remote-proto` types (pairing allow, deny, expiry, wrong PSK, a second channel, five attempts and the
sixth refused, levels, one CSM call per read op, folders, commands, push host, launch bucket, live cap, hostile
names, oversize requests, revoke, level change, a different key, 4000 random frames); `relay_client.rs` tests the
reconnect against a fake relay on 127.0.0.1 (the relay is dropped and the host comes back and re-sends its allow list).

### 18.1 Not built (step 1 still open)

- The stdio shell (`main.rs` exits with a message), the manifest `samples/addons/phone-remote/addon.toml`, the four
  add-on tools (`phone-remote.status|pair|revoke|enable_access`) and their caller checks, `screen_open` wiring.
- The three screens (pairing, devices, settings), the `qr` node, `csm/status`, the `phone` and `devices` icons, and the
  settings screen of section 10.6.
- `wss://` (needs a TLS feature of `tungstenite`; an owner decision on the crate set). Until then only `ws://127.0.0.1`
  relays work.
- The host-wide tracker and session deltas (section 7): the ops answer snapshots on request only; no unsolicited
  `Snapshot` or `Delta` events are sent yet, and the 200 ms coalescing is not built.
- Push encryption (RFC 8291) and the `PushRequest` sending (section 9). `push_ready` is `push && subscription`, not a
  check of the relay's VAPID key.
- `remote_control`: `session.new` answers `"not_built"` (section 4's wait and `Event::RemoteControl` are not built;
  `msg.rs` has no `RemoteControl` event yet).
- Worker threads: CSM calls run inline on the host's thread, so a slow `session_restart` delays other channels. The
  in-flight cap is enforced, but with inline calls it never holds more than one at a time.
- Rekeying is wired (`rekey_outgoing`/`rekey_incoming` at 10 000 messages) but not tested with a message count that
  reaches it.
- ACL-based owner-only permissions on Windows (section 5 says reuse `fsperm.rs`): not done; files get the default ACL.
- The tests that run the real `csm-relay` in process (section 16, step 1): **not built, and blocked by the layer rule**.
  `csm-relay` is layer 6 and `csm-phone` is layer 6, and `xtask` checks dev-dependencies too, so a dev-dependency
  on `csm-relay` fails `xtask ci`. Options for the owner: give the relay test harness its own layer (or exempt test-only
  edges in `xtask/src/layers.rs`), or move the relay-level tests into a test-only crate. The reconnect test uses a small
  fake relay instead.
- The web app `phone/` (step 2) and the end-to-end task (step 3): not started.

### 18.2 Deviations from this plan, with the reason

- Threads: the relay client uses one thread with a one-second read timeout instead of a reader and a writer thread. One
  owner of the socket is simpler to reason about, and the host has no shared state to lock.
- The host key stays a 32-byte file, as section 5 says. The public half is derived with snow's own X25519 primitive
  (`host::public_of`), so the file does not need to hold it.
- Phone device ids are `d_` plus 16 hex characters from the OS random source (`getrandom`).
- `devices.json` entries do not carry the routing token, only its hash (as section 5 already says).
- `session.new` returns `remote_control: "not_built"` rather than `"pending"` (see 18.1): the answer says what is true.
