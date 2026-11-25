# CSM as a server for a remote CSM: research

> **Historical design note.** This began as research (2026-11-22) and the sections up to "Decisions"
> are the research as written, when nothing was built: "Nothing built", the `Dispatcher` seam and the
> relay's phone-only limits describe the code of that day. The sections after "Decisions" record
> what was built, step by step. For how it works now, read
> [../remote-access.md](../remote-access.md). File and line references below are to the code as it
> was and are no longer valid.
>
> | Phase | Status now |
> |---|---|
> | P0 prerequisites | Built |
> | P1 MVP over a private network | Built: pairing, snapshots and terminal streams, remote window, `csm attach` |
> | P2 robust | Built: resume from offsets, reconnect, consent cards on peers, relay transport |
> | P3 comfort | Partly: uploads for drops and paste are built; port forwarding for run URLs is not |
> | P4 later | Operator and Viewer levels are built and chosen at pairing; mixed view and several offices in one window are not (one office per window) |
>
> Peers have their own relay endpoints with higher limits (8 MiB/s and 2000 frames/s per channel by
> default, set on the relay), so the relay is usable for terminal streams.

Date: 2026-11-22. Code read: the `v2` branch at that date. Nothing built or changed at the time.
All `file:line` references are to that checkout.

**Goal:** CSM runs on the always-on office machine. At home, CSM on the laptop connects to
it and shows the same sessions, live, with full control.

**Network:** office and home are on the same **WireGuard** network. The laptop can
reach the office machine directly at its WireGuard address. That direct connection is the primary
design target. Relay and internet options get a short "later / for others" section (section 9).

**Firm requirement:** server mode is **off by default** on every install and upgrade.
Nothing listens and nothing is broadcast until the person turns it on in Settings. No agent or MCP
caller can turn it on. See section 4.1.

---

## 0. Summary

- **Recommendation:** a **thin remote window** (option A). The laptop runs the normal CSM UI
  against a `RemoteBackend` that mirrors the office engine's snapshot, proxies tool calls, and
  streams each visible session's **raw PTY bytes** into a local `csm-term` emulator. The office
  engine stays the only owner of sessions, git, files and add-ons.
- **The seam mostly exists.** Screens only read a `UiModel` and draw `TerminalSource`s
  (`crates/csm-ui/src/backend.rs:1-17`). The UI does no disk I/O and spawns nothing; every effect
  is a tool. Two things are not abstracted yet: the `Dispatcher` holds a concrete `Engine`
  (`backend.rs:86-91`), and layout, focus and rail tab are **engine state**, so a second viewer
  would move the office screen's layout (section 2.3).
- **The terminal path fits a byte stream.** The renderer reads a local `Arc<Mutex<Terminal>>`
  (`terminal/source.rs:30-33`). A remote source keeps its own emulator, fed with bytes from the
  server. The server needs two new things: a **per-session output tap with a ring buffer**
  (today the bytes go only to the emulator and the search index, `csm-engine/src/session.rs:256-287`)
  and a **grid-to-VT serializer** in `csm-term` so a client can attach mid-session.
- **Reuse from the phone-remote work:** the Noise parameters, prologue, check code, pairing flow
  and device allow-list ideas from `csm-remote-proto` (now in the private csm-relay repository, vendored here). Do **not** reuse its message set:
  that set has no terminal access by design (`msg.rs:1-13`). Add a second profile ("peer") over
  the same handshake and pairing base. The relay is not needed for WireGuard. Its phone limits at the time
  (256 KiB/s and 50 frames/s per channel, `limits.rs:16-17`) were too tight for terminal
  streams; peer endpoints with their own, higher limits were added later (see the status table).
- **Security:** a paired laptop is effectively the person at the office keyboard: it has a shell.
  So: explicit opt-in, bind only to a chosen address, Noise IK with device keys after a pairing
  code, a new `Caller::Peer`, an audit log, one-click off and revoke-all. Do not rely on
  WireGuard alone (section 4).
- **Effort:** about 8-11 weeks for one developer with agents to reach the "full" phase. The
  MVP (watch and drive sessions over WireGuard, one level of trust) is about 4-5 weeks including
  the prerequisites.

---

## 1. Architecture options

### A. Thin remote window (recommended)

The laptop's CSM window is a view of the office engine. The laptop starts no engine of its own
for that window (or keeps its own in another window; see B).

| Concern | Design |
|---|---|
| State | The server pushes its `Snapshot` (it is already `Serialize`, `csm-engine/src/snapshot.rs:141-185`; `view_get` returns the same values) as a full copy at attach, then deltas keyed by `version`. The client maps it with the existing `map_snapshot` (`engine_backend.rs:583`). |
| Actions | Every button is already a tool call (`docs/tools.md` intro). The client sends `{tool, params}` and gets the `Reply`. This is the same shape as the CLI bridge's `csm.submit` / `csm.wait` (`csm-mcp/src/wire.rs:1-17`). |
| Terminal output | Raw PTY bytes per session, numbered by byte offset, replayed into a local `csm_term::Terminal` (see 1.1). |
| Input | Keys, mouse reports, paste and focus reports go to the server as typed messages. The server applies them through the existing `TerminalHandle::send_key` / `send_mouse` / `paste` / `focus_changed` (`session.rs:350-375`), so "the person answered" and bracketed paste keep working. |
| Size | One PTY size per session; see 1.2. |

#### 1.1 Raw bytes vs grid snapshots

| | Raw PTY bytes into a local emulator | Grid diffs from the server's emulator |
|---|---|---|
| Fits the renderer | Yes: `TerminalSource::terminal()` returns an emulator (`source.rs:30-33`); the remote source just owns one | No: the renderer reads `Terminal::term()` directly (`csm-term/src/lib.rs:301`); a diff format would need a fake `Term` or a second render path |
| Bandwidth | Equals PTY output (see 3.4) | Bounded by screen size; better on slow links |
| Scrollback, selection, search in pane, links | All work locally, as today | Need extra messages |
| Correctness risk | The client emulator must stay in lockstep. Same `csm-term` version on both ends, same size, and **replies discarded** on the client: the server's emulator already answers DA1, cursor reports and the OSC 7501 probe into the PTY (`lib.rs:5-11`, `session.rs:186-188`). Use the `Discard` sink (`source.rs:79-83`). | Low |
| Attach mid-session | Needs a starting state: a serialized grid (below) | Natural |

**Recommendation:** raw bytes, plus a **serializer** for attach and resync. The serializer turns
the server's grid (scrollback, screen, cursor, SGR attributes, modes such as alt screen,
bracketed paste, mouse modes and cursor style, plus the title) into a VT byte string. The client
feeds that string into a fresh emulator of the same size, then the live stream from the
serializer's offset. This is what tmux does on attach. `alacritty_terminal` has no serializer,
so this is new, pure, testable code in `csm-term` (about 500-800 lines with tests). Property
test: feed random program output into emulator A; serialize A into B; the grids must be equal.

Fallback without a serializer (spike only): replay the server's ring buffer. That is only correct
when the ring still holds everything since the session started. A resize nudge makes ConPTY and
TUIs such as Claude Code redraw the screen, but scrollback is lost.

#### 1.2 Who owns the PTY size

A PTY has one size. Today `TerminalHandle::resize` resizes the server's emulator **and** the PTY
(`session.rs:329-349`). The server's own pane reports its size only when it changes
(`EngineTerminal::set_grid`, `engine_backend.rs:690-698`). So "last resizer wins" is already what
would happen. The office pane would then draw at the laptop's size and would not fight back.

Proposed rule (tmux's `window-size latest`):

- The size follows the **driver**: the client (or the server UI) that last typed into the
  session or focused its pane. A pane on another viewer only *requests* a size. It does not
  resize the PTY until that viewer becomes the driver.
- A non-driving viewer draws the PTY-sized grid scaled or clipped into its pane. The view already
  does this for previews (`terminal/view.rs:313-330`: fixed grid, font zoom, clipped at the top).
  Show a small "sized for Laptop · Fit to this window" chip.
- The client's emulator is resized only when the server reports a new PTY size, in stream order
  (a `resized` marker in the byte stream), never when the local pane changes.
- When no one is looking, nothing changes. A session nobody has shown starts at the fallback size
  (`lifecycle.rs:195`, `:511`).

### B. Mixed window: local and remote sessions side by side

Remote sessions appear as a new session kind next to local, WSL and SSH ones ("Office ›
odulodo"). It is attractive, but it is much harder:

- Session ids, groups, layouts, inbox and settings come from two engines. Tools without a
  `session_id` (`layout_set`, `group_*`, `settings_set`, `inbox_next`) have no obvious target.
- The engine owns the inbox order and focus (`snapshot.rs:159-162`). Merging two of each is new
  logic in the UI layer.
- Every tool reply has to be routed back to the right engine.

A cheap form of B is possible later: one **remote-mirror session** per remote session, owned by
the local engine, whose "process" is the peer stream (like an SSH session whose PTY is the
network). Status comes from the remote snapshot rather than OSC parsing. Do this after A, reusing
A's client and stream code.

### C. Other options

- **C1: `csm attach` in a terminal (tmux-like).** A CLI subcommand that connects to the peer,
  subscribes to one session and pipes its bytes to stdout and stdin to it. It runs in any
  terminal, including a local CSM session (a process add-on could offer it under "Remote…" via
  `csm/remotes_changed`, `docs/addons.md:313-343`). Cheap, and a good **test harness** for the
  stream channel. It shows no sidebar status, inbox or tabs. Worth building as part of the MVP
  because it exercises the same server code.
- **C2: remote desktop (RDP, Parsec).** Works today with no code. It is the baseline any design
  must beat: one shared screen, bandwidth-heavy, and no use of the laptop's own window layout.
- **C3: put the laptop's CSM on the office engine through the existing CLI bridge.** Not viable.
  craft-ipc binds loopback only and authenticates with a token file only this user can read
  (`ipc_server.rs:1-13`, `:66-68`). It has no terminal streaming, and opening it to the network
  would be the wrong security model.

### Recommendation

**Option A**, with C1 built alongside as a harness. A reuses almost all of the UI unchanged,
keeps one source of truth (the office engine, which owns sessions, files, git, add-ons and
consent), and matches how the owner works: the office machine does the work and the laptop is a
window onto it. B can come later as a thin layer over A's client.

---

## 2. What makes it easy or hard in the current code

### 2.1 Easy

- **Backend seam.** `trait Backend` (`backend.rs:51-87`) has `model`, `terminal`, `on_reply`,
  `on_event`, `tick`, `screens`, `screen_declared`, `screen_tree`, `screen_event` and
  `run_output`. There are already two implementations (`EngineBackend` and `DemoBackend`,
  `demo/mod.rs:1312`). A `RemoteBackend` is a third.
- **The UI touches no disk and spawns nothing.** A grep of `crates/csm-ui/src` finds only font
  and theme-test file reads. Git, Docs, Notes, Usage, Search and Runs are tool calls
  (`EngineBackend::on_reply` builds the Git, Usage and Docs details from replies,
  `engine_backend.rs:220-236`). These proxy as they are and correctly read the **server's** disk.
- **Notifications are decided in the UI** from snapshot transitions (`crates/csm-ui/src/notify.rs:1-4`).
  A mirrored snapshot gives the laptop its own OS notifications for free.
- **Events are serializable** (`csm-engine/src/events.rs:11-80`) and already pushed over the CLI
  bridge (`wire.rs:17`).
- **Restarts are identified.** `SessionIo::instance` distinguishes a run from a later restart of
  the same session (`session.rs:163-165`). That is the key for stream resume.

### 2.2 Needs refactoring (prerequisites)

1. **`Dispatcher` is concrete.** It holds an `Engine` and calls `engine.submit(Caller::Ui, …)`
   (`backend.rs:86-130`). `CsmApp::new(engine, backend, renderer)` takes an `Engine`
   (`app.rs:549`). The palette, menus and settings read `app.dispatch.registry()`
   (`palette.rs:135`, `:463`, `menus.rs:609`, `settings.rs:438`). Change: a `ToolPort` trait
   (`submit`, `poll`, `tool_info`) with an engine implementation and a remote implementation. The
   remote registry is the server's `ToolInfo` list (the CLI bridge already has `csm.tools`,
   `wire.rs:14`), because add-on tools are dynamic. `EngineBackend::on_reply` also reads effects
   from the registry (`engine_backend.rs:261`).
2. **Engine types in `csm-ui`.** The UI uses `csm_engine::{Event, Reply, Snapshot, SessionView}`.
   `Snapshot` and `SessionView` are `Serialize` only, with `&'static str` fields (`kind`,
   `status_label`, `snapshot.rs:33`, `:58`). Add owned wire DTOs with `Deserialize`.
3. **Crate placement and layering.** `xtask/src/layers.rs:13-30`: `csm-ui` is L4 and may not
   depend on another L4 crate. Only `csm-ui` and apps may use egui. Proposal:
   - `csm-peer-proto` at L0, beside `csm-remote-proto`: frames, messages, vectors. Pure.
   - `csm-peer` at L4, beside `csm-mcp`: the server (on the engine) and the client connection.
     It uses `snow`, a new non-dev dependency; today `snow` is only a dev-dependency of
     `csm-remote-proto` (`Cargo.toml:20`).
   - `RemoteBackend` and `RemoteTerminal` in `apps/codeshell`, at L6, which can depend on both
     `csm-ui` and `csm-peer`. The other way to place them is a new L5 crate added to `EGUI_USERS`.
4. **Output tap.** `SessionIo::sink` (`session.rs:256-287`) feeds the index and the emulator.
   Add a bounded per-session ring buffer with absolute offsets and a subscriber list. It must be
   non-blocking on the PTY reader thread, like the index feed: drop and count, never wait.
5. **Serializer** in `csm-term` (1.1).

### 2.3 State that must become per-client

These live in the engine today and are changed by `ui`-effect tools: `focused`, `rail_tab`,
`layouts` and `layout_key`, `inbox_open` and `inbox_height`, `inspector`, `view`, `windows` and
torn-off windows (`snapshot.rs:152-176`); tools `session_focus`, `layout_*`, `view_set`,
`inspector_show`, `inbox_panel_set`, `window_placement_set`, `group_collapse`,
`group_tear_off` and `group_dock` (`docs/tools.md` toolsets table). The theme is a setting
(`engine_backend.rs:238-245`).

**Proposal:** the client keeps a **view overlay**. It runs calls whose effects are only
`CHANGES_UI_STATE` locally, against its own copy of these fields (persisted per server in the
laptop's state). It forwards everything else. `session_focus` is both: it is local for focus, but
it also wakes an asleep session (`docs/tools.md`: `session_focus`: "wakes if asleep →
`session_wake`"), so the client sends `session_wake` when needed. Server-side MCP callers keep
using the server's own view state.

**Open items before wiring** (implemented as `csm-ui`'s `view_overlay` routes):
- `inbox_next` and `view_set` with a `rail_tab` are forwarded, not local: the engine runs
  `lifecycle::focus` for them (a wake and an acknowledgement). Their declared effects under-state
  this (`inbox_next` is `UI` only). Fixing the declarations changes MCP consent, so the owner approves
  it first.
- An overlay focus does not do `lifecycle::focus`'s server extras: the result acknowledgement and
  the last-activity stamp. Every overlay focus must be forwarded or replayed to the server for them.
- A UI-only call the overlay does not model (window and add-on screen actions, a session's status)
  is routed as not modelled: the client runs it or forwards it, and never drops it.
- Ordering for an overlay focus: the local focus is committed first, then `session_wake` is sent
  when the session is asleep. If the wake fails, the engine has focused nothing, so the client
  restores the overlay it cloned before the focus. The overlay never keeps a focus the server did
  not make.

---

## 3. Protocol and transport

### 3.1 Transport (WireGuard, primary)

- **TCP to the office machine's WireGuard address**, on a fixed port (for example 47300; the
  person can change it). The server listens only on the **interface or address chosen in
  Settings**, never `0.0.0.0` by default. If that address is missing (the tunnel is down at
  start-up), retry with backoff and show it in the indicator. Do not fall back to another
  address.
- **Noise over length-prefixed frames**, no TLS and no WebSocket: `u32 BE length || Noise
  message`, one app message per Noise message (65 535-byte limit, `noise.rs:23`), rekeying as
  `csm-remote-proto` does (`noise.rs:16-19`, `REKEY_INTERVAL` 10 000). Terminal chunks are split
  to fit.
- **Handshakes:** as the phone remote. Pairing uses `Noise_IKpsk2_25519_AESGCM_SHA256`; later
  connections use `Noise_IK_25519_AESGCM_SHA256` (`noise.rs:25-26`). Use a **different prologue**
  (`"csm-peer/1" 0x00 host_id`) so phone and peer handshakes can never be confused
  (`noise.rs:33-38`).
- **Windows firewall:** the first listen triggers a prompt, or the WireGuard adapter may be in the
  Public profile and block inbound traffic. On enable, CSM shows the exact rule (port, remote
  address scope = the WireGuard subnet) and offers to add it. That step needs elevation, so it is
  a separate confirmed action. The installer adds no rule.

### 3.2 Message set ("peer" profile)

JSON control messages plus binary stream frames inside Noise. Every message carries a channel or
stream id.

| Direction | Message | Notes |
|---|---|---|
| C→S | `hello {client, protocol, app_version}` | Refuse on protocol mismatch. Require the same `csm-term` minor version, because emulators must agree byte for byte. |
| S→C | `welcome {host_name, level, device_id, server_version}` | |
| S→C | `snapshot {version, data}` / `delta {from, to, ops}` | Deltas per session row plus whole small fields. A full copy after reconnect. |
| S→C | `event {Event}` | Toasts, `ConsentRequested` and `ConsentResolved`, `FocusRequested` (client decides), `ScreenChanged`. |
| C→S | `call {id, tool, params}` → S→C `reply {id, ToolOutput \| ToolError}` | Long calls are fine: no 60 s limit as in craft-ipc. |
| C→S | `term_subscribe {session, instance?, from_offset?}` | The server answers `term_attach {instance, size, offset, vt}` (serialized grid) or resumes from `from_offset` if the ring still has it. |
| S→C | `term_data {session, instance, offset, bytes}` | Binary. `term_resized {offset, cols, rows}` is in stream order. |
| C→S | `term_unsubscribe {session}` | Sent when the pane is hidden. Snapshot status keeps flowing, so the sidebar stays live. |
| C→S | `term_input {session, kind: key \| mouse \| paste \| focus, bytes \| text}` | Maps to `TerminalHandle` methods. `paste` is text that the server brackets. |
| C→S | `term_size_request {session, cols, rows, driver}` | See 1.2. |
| S→C | `clipboard {session, text}` | OSC 52, to the driver only (`Engine::take_clipboard`, `engine_backend.rs:276`). |
| both | `run_output_*` | Same tail and generation model as `RunOutputHandle` (`runs/output.rs:159-170`). |
| both | `screen_tree` / `screen_event` | Add-on screens: the add-on runs on the server. |
| both | `ping` / `pong`, `bye {reason}` | `bye` when server mode is turned off, the device is revoked, or the server quits. |

### 3.3 Reconnect and resume

- The client reconnects with backoff (1-2-5-10-30 s, as the relay's hosts do,
  the csm-relay repository's `README.md` ("Operating notes")).
- State: a full snapshot after every reconnect. It is small: tens of KB for 50 sessions.
- Terminals: the client sends `(session, instance, last_offset)`. If `instance` matches and the
  offset is still in the ring, the server replays the gap. Otherwise it sends a fresh
  `term_attach`. The ring is per session, for example 2 MiB (about 40 MiB for 20 sessions; make
  it configurable).
- Calls in flight at disconnect: the client shows "unknown result" and refreshes from the
  snapshot. It never retries a non-idempotent call automatically.

### 3.4 Bandwidth, latency, backpressure

These are estimates; measure them first (P0 adds a bytes-per-second counter per session).

- Claude Code while working: a spinner and status redraws at roughly 5-15 per second, a few
  hundred bytes each, plus streamed text. Expect about **2-30 KB/s per busy session**, with
  bursts of hundreds of KB on large diffs or tool output. ConPTY re-encodes output, sometimes as
  full-line rewrites.
- 10 busy sessions, **subscribed only to visible panes**: about 50-300 KB/s, with bursts. That is
  easy for WireGuard on home broadband. Compress per stream (zstd or deflate on batches of about
  10-20 ms; VT text compresses 4-10x). Compress *before* encrypting: in this context, with no
  attacker-controlled secrets mixed into the same stream, the CRIME/BREACH-style risk is
  acceptable, but note it.
- Latency: keystroke echo is one round trip over WireGuard (typically 10-40 ms home↔office).
  This is fine. A mosh-style local predictive echo is not worth it at first.
- **Backpressure:** each subscription has a bounded queue on the server (for example 1 MiB).
  When a client falls behind, drop that session's queue and send `term_attach` (a serialized
  snapshot) when the socket drains: state sync instead of a byte backlog, as in mosh. Input,
  calls and replies have their own priority lane and are never dropped. Snapshot deltas are
  coalesced to the latest version.

---

## 4. Security

**Threat model:** a paired client can type into shells on the office machine. That is code
execution as the user. Treat a paired device like an SSH key to the office machine.

### 4.1 Server mode is off by default; how it is turned on

- **Default off** on every install and upgrade. State migration never turns it on, and an
  imported `.csm` backup never carries it (`state_import` must ignore these keys). When it is
  off: **no listening socket, no mDNS or other discovery, no outbound connection.** Discovery is
  never implemented: the client is given the address in the pairing link.
- **Only the person in the app can turn it on.**
  - New tools `peer_server_enable`, `peer_pair_start`, `peer_device_revoke` and
    `peer_server_disable` (so it is "everything is a tool"). The engine refuses
    `peer_server_enable` and `peer_pair_start` for **every caller except `Caller::Ui`**, in
    `tools::precheck` (`csm-engine/src/tools/mod.rs:58`). Precedents:
    `consent_answer` is UI-only (`engine.rs:510-513`), and `state_import` is refused for MCP
    (`backup.rs:167-174`).
  - **Recommendation: refuse, do not ask.** A consent card is weaker: it can be clicked through,
    and an agent can repeat the request until someone clicks. Also refuse it for
    **`Caller::Cli`**, unlike `state_import`: an agent in a CSM session can run `csm …` through
    its own Bash tool, and the CLI is labelled by the caller (`ipc_server.rs:12-13`), so it
    proves nothing.
  - These settings must **not** be `settings_set` keys. `settings_set` is `state`, which does not
    need consent (`tool.rs:71-79`). `APP_ONLY_KEYS` refuses external callers but allows the CLI
    (`tools/app.rs:140`, `:252`).
  - `peer_server_disable` and `peer_device_revoke` are allowed for every caller, including MCP and
    peers. Turning things off is always safe.
- **Enable flow (Settings › Remote access › "Let another CSM connect to this one"):**
  1. Pick the address: a list of this machine's interfaces with their addresses, with the
     WireGuard adapter highlighted and pre-selected when one exists. No "all interfaces" item by
     default. "All" stays behind an "Advanced" disclosure with a warning. Pick the port.
  2. Show the firewall step (3.1).
  3. Turn on: CSM starts listening and shows a **pairing panel**. The panel holds a pairing link
     (`csm://pair#<base64url offer>`) as text with a copy button, plus a QR code. The offer
     contains the address and port, the host id, the host's static public key, a pairing slot id
     and a 256-bit PSK, valid for 10 minutes and one use. This is the `PairingOffer` shape
     (`pairing.rs:22-31`) with `r` = `tcp://10.x.y.z:47300` instead of a `wss://` relay URL.
     `validate` currently insists on `wss://` (`pairing.rs:56-60`), so use a peer variant or a
     new offer version.
  4. On the laptop: "Connect to another CSM…" → paste the link. The handshake runs.
  5. **Both screens show the 4-digit check code** (`noise.rs:40-49`) and the device name. On the
     office screen, the person chooses **Allow** and the level, or **Deny**. Problem: the owner may
     be at home when he wants to pair. Pairing then needs the office screen, through RDP or by
     pairing while the laptop is in the office. Open question Q2.
  6. **Selector and limits (review fix).** The responder picks `IKpsk2` or `IK` before reading
     message 1, so each connection starts with a cleartext preamble frame: `01` session, or
     `02 || pairing_id` pairing (`csm-peer-proto` `noise::route`, vectors in `preamble.json`).
     No open slot or a wrong id is a silent close, the same either way. A slot is consumed when
     the first transport message decrypts (the PSK is only proven then) and allows 3 handshakes.
     **Check code limit:** the 4-digit code does not stop an active swapper in the middle, who can
     grind ephemerals (about 10^4 tries) until both screens agree. We accept that: pairing is in
     person or over RDP, the link is the secret, and the code guards against mix-ups and a
     passive or non-participating swapper.
- **Always-visible indicator:**
  - On the server, a pill in the title or status bar: "Remote access on · 10.8.0.2:47300". When a
    client is connected it changes to an accent colour: "Laptop connected" with a count. The pill
    also shows in the tray tooltip and icon badge, and appears even when the window is restored
    from the tray. Clicking it opens a popover with the connected devices (last seen, address),
    **Disconnect**, **Turn off** and **Revoke all devices**.
  - On the client, a persistent coloured bar: "Connected to OFFICE (10.8.0.2)". The window title
    is prefixed with the host name, so a remote window can never be confused with a local one.
- **One-click off:** this closes the listener, sends `bye` and closes every connected client at
  once, and clears any open pairing slot. **Revoke all** also deletes every device key. The next
  connection then needs a fresh pairing.

### 4.2 Authentication and encryption

- **Pairing** binds a device's static X25519 key to the host. **Connection:** Noise IK. The host
  accepts only a client static key on its device list. That gives mutual authentication, forward
  secrecy and AES-GCM.
- **Why not rely on WireGuard alone:** WireGuard authenticates *machines on the VPN*, not this
  person's CSM device. Any process or user on any peer that can route to the office address
  (another colleague's machine, a compromised peer, a too-broad `AllowedIPs`) would otherwise get
  a shell. App-level authentication and encryption also keep it safe if the listener is ever
  exposed on another interface. The same protocol then works unchanged over the relay later.
- Device keys are stored in the OS keychain on the client, as `addon_secret_*` already does. On
  the server, the device list holds public keys only.

### 4.3 Authorization (levels)

- **Full** (default for your own laptop): everything the office UI can do, including answering
  consent cards.
- **Operator** (built; chosen at pairing): sessions, input and runs, but no settings, add-ons, grants or Open with
  editing.
- **Viewer** (built; chosen at pairing): snapshot and terminal output only; input is refused on the server.
- The server enforces levels. A new **`Caller::Peer { device_id, name }`** in `tool.rs:155-164`
  lets the engine log, check levels and refuse peer-control tools. Peers are never `Caller::Ui`.
  The server-mode tools above stay UI-only, even for Full: a remote device cannot pair further
  devices or change the listen address.

### 4.4 Consent cards

`ConsentRequested` is raised when an agent on the server (MCP) calls a tool that needs consent
(`engine.rs:414-425`). Today only `Caller::Ui` may answer (`engine.rs:510-513`).

Proposal: forward cards to every connected **Full** client and to the office UI. The **first
answer wins**. `ConsentResolved` already removes the card everywhere (`engine.rs:518`). Allow
`consent_answer` from `Caller::Peer` at level Full only, and record which device answered. This
matters: if the owner is at home, a card shown only in the office times out after 120 s
(`engine.rs:33`).

### 4.5 Audit and revocation

- An append-only log on the server (`remote-access.log` beside the state): enable and disable,
  pairing (device, check code accepted, level), connect and disconnect (device, address), and
  every non-read tool call from a peer (tool, session, effects). Log input as a byte count per
  session per minute, never its content. Show the log in Settings.
- **Revoke one or all** from Settings or the indicator: delete the key and close its connection
  immediately.

### 4.6 Relation to the phone remote

One base, two profiles:

- **Shared:** Noise parameters, check code, the pairing UX (link, QR, check code, Allow/Deny,
  level), the device list, revocation and the indicator.
- **Different:** the phone has a fixed op set with no terminal access, through a process add-on
  and the relay (`msg.rs:1-13`; the csm-relay repository's `README.md`). The peer has the full tool set and terminal
  streams, in core, direct over TCP.
- Keep the profiles separate in the protocol (prologue, op sets) and in the device list (a
  "kind" column). Then a phone can never get peer powers through a bug in level mapping. The
  peer cannot be an add-on: add-ons have no terminal stream access, only `session_read_screen`
  and similar tools.

---

## 5. Concurrency semantics

- **Two typists in one session:** both may type, as in tmux. Bytes interleave per message. The
  server shows "Laptop is typing" on the other viewers' pane headers for about 2 s after input.
  Input makes that viewer the size driver (1.2). An optional "take control" lock can come later.
- **Focus, layout and active pane:** per client (2.3). Sessions, groups, notes, tasks, runs and
  the inbox order are shared.
- **Seen and unseen:** typing acknowledges a result for everyone (`note_interaction`,
  `session.rs:244-249`). That is right: the person saw it.
- **Notifications:** each client decides from its own snapshot (notify rules with its own focus).
  Office and laptop may both notify. Possible refinement: the server suppresses its OS
  notifications while a Full client has had input in the last N minutes (open question Q5).
- **Server quits or restarts:** clients get `bye`, keep the last snapshot greyed out ("Office
  offline · reconnecting"), and reconnect. On start, the server restores its sessions (Claude
  sessions resume their conversation); the session ids persist and new `instance` values force a
  fresh `term_attach`.
- **Server sleeps:** the TCP connection times out (ping every 15 s, dead after 45 s). An
  always-on machine should have sleep disabled; Settings can warn when the power plan sleeps.
- **The client quits:** "Quit" on a remote window **disconnects**. It must never call `app_quit`
  on the server. Quitting the office app is a separate, confirmed menu item ("Quit CSM on
  OFFICE…").

---

## 6. Things that are local on the client and would be wrong remotely

| Feature | Today (code) | Remote behaviour (proposal) |
|---|---|---|
| Open folder | `open_folder` tool, runs on the engine's machine (`docs/tools.md:499`) | Hidden for remote sessions in the MVP. Later: open the server path through a configured client-side handler (for example an SMB share mapping). |
| Open with (editor) | `open_with` (`docs/tools.md:500`) | A **client-side** Open with list per remote host, with templates such as `code --remote ssh-remote+office {path}` or a VS Code Remote Tunnels URI. MVP: hidden. |
| Ctrl+click link | `open_url` (`docs/tools.md:501`) | Open in the **client's** browser after the same http/https check. For `localhost` or `127.0.0.1` links, warn "this points to OFFICE's localhost". Later: offer a port forward over the peer channel (as VS Code does). |
| Run ready URL | `post_run_url` with a loopback target (`runs/mod.rs:297`) | Same as links: client browser, with a port-forward offer later. |
| File drop onto a pane | Paths are pasted with `session_send_text` (`screens/file_drop.rs:1-12`) | The client's paths mean nothing on the server, so the files are uploaded to the office and their office paths pasted. Built: see "Uploads: file drops and image paste" (end of this file). |
| Image paste (Ctrl+V with an image) | Sends raw `^V` so Claude reads the clipboard itself (`terminal/view.rs:506-511`) | Claude reads the **office** clipboard, so the image is lost. A remote window now uploads the clipboard's image and pastes its office path, which Claude Code accepts. Built: see "Uploads: file drops and image paste" (end of this file). |
| OSC 52 copy | `Engine::take_clipboard` → `Notice::Clipboard` (`engine_backend.rs:276`) | Send only to the driving client. Do not change the office clipboard while a peer drives it. |
| Text paste | `TerminalSource::paste` (bracketed) | Text from the client clipboard, bracketed by the server (untrusted rule). |
| New-session folder picker | Native dialog (`apps/codeshell/src/main.rs:170`) | That would browse the laptop. Replace it with a server-side folder browser: a new read tool `fs_list_dir` (folders only, consent for MCP), plus recent and pinned folders. |
| Backup export and import | Native dialogs (`main.rs:174-176`) | Paths on the server. MVP: disabled from a remote window. |
| Git, Docs, Notes, Search, Usage, Runs, Board | Engine tools | Correct as they are: they read the server's disk. Just proxy them. |
| Add-on screens and add-on tools | Run on the server | Proxy `screen_tree` and `screen_event`. |
| Theme, fonts, zoom, window placement, torn-off windows | Partly engine settings | Per-client (overlay). |
| OS notifications and tray | Client UI (`notify.rs`) | As built, a remote window has no tray and no OS notifications (see step 7). |
| `app_quit` | Quits the engine | A remote window's Quit disconnects instead (5). |
| Claude Remote Control links | claude.ai URLs | Client browser; fine. |
| WSL and SSH session kinds | Launched by the engine | Run on the server; correct. |

---

## 7. Phased plan

Estimates are for one developer working with agents, as in `docs/plan.md`.

| Phase | Scope | Effort |
|---|---|---|
| **P0: prerequisites** | `ToolPort` trait replacing the concrete `Dispatcher`; owned wire DTOs for `Snapshot` and `Event`; per-session output ring and subscriber tap in `SessionIo::sink`; bytes-per-second measurement; layer map entries for the new crates. | 1-1.5 wk |
| **P1: MVP over WireGuard** | **Server mode (off by default) with the full enable flow from 4.1:** address picker, firewall step, pairing link and QR, check code, Allow/Deny, always-visible indicator, one-click off, revoke-all, UI-only precheck for the enabling tools. Noise IK transport (`snow`). Snapshot and deltas; tool proxy with `Caller::Peer` (Full level only); terminal subscribe, input and size driver; **grid serializer** for attach; `RemoteBackend` and `RemoteTerminal`; "Connect to another CSM…" on the client; per-client view overlay; local-only features hidden (6); remote Quit = disconnect; `csm attach` (C1) as harness; audit log. | 3-4 wk |
| **P2: robust** | Resume from offsets; backpressure → resync; reconnect UI; consent cards on peers (4.4); OSC 52 routing; run-output streaming; add-on screens; compression; "typing" indicator. | 2-3 wk |
| **P3: comfort** | Client-side Open with and links with a localhost warning; server folder browser (`fs_list_dir`); file and image upload for drops and paste; port forwarding for run URLs; suppressing duplicate notifications. | 2-3 wk |
| **P4: later / for others** | Operator and Viewer levels; relay transport (section 9); mixed view (B); several hosts in one window. | 3+ wk |

### Tests

- **Protocol vectors** (as `csm-remote-proto/tests/vectors.rs` in the csm-relay repository): frames, the peer prologue,
  handshakes with fixed keys, the offer with `tcp://`, refusals.
- **Serializer property tests:** random VT streams (SGR, wide characters, scroll regions, alt
  screen, OSC 8 links); serialize then replay gives an equal grid, cursor and modes. Include
  captured Claude Code and ConPTY output.
- **Lockstep test:** a server emulator and a client emulator fed the same stream (with the
  `Discard` replies) stay equal through resizes.
- **Engine tests:** `peer_server_enable` and `peer_pair_start` refused for `Cli`, `External` and
  `Peer`; `consent_answer` from Peer allowed only for Full; first consent answer wins; revoke
  closes the channel.
- **Default-off test:** a fresh state, a migrated WPF state and an imported `.csm` all have no
  listener. Check that the socket is not bound, not just that the setting is false.
- **Integration (in-process):** server and client on 127.0.0.1 with an ephemeral port, as
  the csm-relay repository's `tests/relay.rs` does: attach, type, resize, drop the connection mid-stream, resume,
  overflow → resync, server restart → new instance.
- **UI (kittest):** a remote window shows the indicator bar, hides the local-only items, and
  Quit disconnects.
- **Manual over real WireGuard:** latency feel, bursts (a large `git diff` in Claude), the
  firewall prompt, and the tunnel down at start-up.

---

## 8. Open questions

1. **Levels:** is "Full" for your own laptop enough for v1, with no Viewer or Operator yet?
2. **Pairing when you are not at the office:** is pairing in the office acceptable (or through
   RDP once)? Or do you want a short typed code (PAKE, such as CPace or SPAKE2) so you can pair
   from home with a code read over the phone? That needs someone at the office screen anyway, to
   press Allow.
3. **Consent cards** from office agents answered from the laptop: yes, first answer wins?
4. **Two typists:** free-for-all (tmux), or a "take control" lock by default?
5. **Duplicate notifications:** should the office machine stay quiet while the laptop is active?
6. **Open with from home:** do you use VS Code Remote-SSH or Tunnels to the office machine? If
   so, client-side templates can use it.
7. **Several offices?** One remote host per window, or a host switcher?
8. **Version skew:** require the same CSM version on both ends (simplest; recommended), or a
   compatibility range?
9. **Remote CLI and MCP:** should `csm --peer office session-list` on the laptop drive the office?
   An agent on the laptop could then act on the office machine. Recommendation: not in v1; later
   only as `External` with consent cards on the office side.

---

## 9. Later / for others: other networks

| Path | Fit | Notes |
|---|---|---|
| **WireGuard or Tailscale** (primary) | Best | Direct TCP to the VPN address. Tailscale works the same way: bind to the `100.x` address. |
| **SSH tunnel** | Good for power users | `ssh -L 47300:127.0.0.1:47300 office`, with the server bound to loopback. The Noise layer stays. |
| **csm-relay** | For people with no VPN | E2E Noise already exists, and the relay sees ciphertext only. *As built:* peer endpoints exist with their own limits (8 MiB/s and 2000 frames/s per channel by default, set on the relay), and the office paces its writes to what the relay announces. The phone endpoint keeps its small caps. A self-hosted relay is fine; umage's relay is for umage only (the csm-relay repository's `README.md`). |
| **Direct public port with TLS** | Not recommended | It exposes the shell port to the internet. If someone insists: Noise is still required, plus rate limits as on the relay. |

---

## 10. Prior art and what to borrow

| Tool | What to borrow |
|---|---|
| **tmux attach** | The server owns the PTYs and clients redraw from server state on attach (our serializer). `window-size latest` / `smallest` for size ownership. Per-client current window and shared sessions. |
| **VS Code Remote / Tunnels** | A thin UI with the work on the remote machine. Port forwarding for `localhost` links. Opening files through a remote URI rather than the local filesystem. Version lockstep between client and server. Device-auth flow. |
| **Zellij web client** | Opt-in web server, off by default, with login tokens you can revoke, and a session list shared by several clients. A model for the opt-in and indicator UX. |
| **Warp** (session sharing) | A visible "shared" state on the session, with viewer vs editor roles: our levels and typing indicator. |
| **mosh** | State sync instead of byte replay when the link is slow (our resync on overflow); roaming and reconnect without losing the session; local echo prediction (not needed on WireGuard). |
| **RDP / Parsec** | The baseline to beat; also the fallback for pairing when away from the office. |

---

## 11. Unknowns to settle early

- Real bytes per second of Claude Code sessions through ConPTY: measure in P0 before choosing
  compression and ring size.
- Whether `alacritty_terminal`'s grid exposes everything a faithful serializer needs (wide
  characters, wrapped-line flags, hyperlinks, the saved cursor, the scroll region, tab stops).
  Likely yes through `Term::grid()`, but it needs a spike.
- Whether `csm-term` replay is deterministic across machines with different fonts and cell sizes.
  `GridSize` carries cell pixels (`view.rs:308`); check that nothing in emulation depends on them
  beyond pixel-size reports.
- The Windows firewall UX for a per-interface rule without elevating the whole app.
- `snow` as a production dependency: review it under `deny.toml`. It is pure Rust and widely
  used, but it is new to the release build.

## Decisions (2026-11-22)

- Themes are per client (2026-11-23): the laptop keeps its own theme choice, like layout.
- `inbox_next` declares that it can wake a session (2026-11-23), so MCP callers are asked.

- Q2 pairing: from home via remote desktop, or with the laptop at the office — both fine; no remote-initiated pairing.
- Q3 consent: cards raised by office agents are answerable from the laptop; first answer wins.
- Layout/grids are per client (laptop screens are smaller): the client keeps its own layout, focus, rail and inspector state.
- Server mode is explicitly opt-in, off by default; agents (MCP or CLI) can never enable it.

## P0 findings: grid serializer spike

Base sha for this step: `277182a05544069b13ea767e0649202f19c209c2`. Emulator: `alacritty_terminal` 0.26.0 (vte 0.15.0). Code: `crates/csm-term/src/replay.rs` (`snapshot`, `render`, `serialize`), tests in `crates/csm-term/tests/replay.rs`. No new crate; `Terminal::title()` is new (the crate keeps the title only as events).

**Exposed and used:** `Term::grid()` with the rows by `Line` (negative lines are scrollback, independent of `display_offset`), `Grid::cursor` and `saved_cursor` (point, template, pending wrap), `Term::mode()`, `cursor_style()`, `colors()` (OSC 4/10/11/12 overrides), each cell's `c`, `fg`, `bg`, `flags`, `zerowidth()`, `hyperlink()` and `underline_color()`.

**Not exposed, so not replayed:**
- Scroll region (`Term.scroll_region`): private, no getter. Content replays; the replay's next scroll uses the full screen. Pinned by `known_gap_scroll_region_is_not_replayed`. A fix needs a DECSTBM shadow in the tap or a crate change.
- Tab stops (`Term.tabs`): private. Custom stops (HTS, TBC) are lost.
- Active character set (`active_charset`) and the kitty keyboard stack: private. Cell characters are already mapped, so content is unaffected; only future input is.
- Hollow block and hidden cursor shapes have no DECSCUSR form; written as a block.
- The alternate screen's inactive buffer is reachable only through `swap_alt()`, which is not reversible: swapping back runs the "enter alternate screen" branch (clears the alternate buffer, copies cursors). `snapshot` clones the alternate grid, swaps, reads, swaps back and puts the clone back. While the alternate screen is active the primary's saved cursor equals its cursor, so the copy the branch makes is harmless. Selection is saved and restored too.

**Behaviour the replay has to work around (each has a test):**
- `put_tab` writes `\t` into a blank cell it passes over, and into a wide character's spacer. The replay prints a space, then sends HT with absolute column moves.
- Overwriting a wide character's spacer blanks the glyph (`clear_wide` sets `' '`). The state that survives is a WIDE glyph with no spacer (erase resets only the spacer's cell); and an orphan spacer after an erase of its glyph. Both are reproduced.
- SGR 0 keeps the template's hyperlink and underline colour (`Attr::Reset` resets fg, bg and flags only). The emitter writes OSC 8 and `59` after every pen change.
- `ESC 7` stores the cursor's current line, and scrolling does not move it. Saved cursors are therefore placed after the content, not inline in it.
- A pending wrap (cursor past the last column) can only be set by printing, and printing overwrites the cell's flags. The replay re-prints the last cell with its own character and pen, or the wide glyph one column to the left.
- Hyperlink ids that the crate generates (OSC 8 without an id) are written back explicitly. Titles and link fields are stripped of control characters before they are written, so a hostile title cannot inject a sequence (tested).

**Wrap markers: known gaps.** `WRAPLINE` is set only by wrapping, so a marker is reproduced when its continuation follows it in the stream. Not reproduced:
1. A tab in the last column of a wrapped row: the wrap needs a print that would overwrite the tab.
2. A wrapped row whose continuation was later erased or overwritten (including a leading spacer whose glyph was overwritten), and a `WRAPLINE` on the bottom row (its continuation is off screen).
3. A saved cursor pending on a wrapped cell: the saved cursor is restored, but re-printing the cell clears its `WRAPLINE`.
4. `WRAPLINE` on a column other than the last (ICH and DCH shift cells with their flags).

Measured: 11 of 360 random streams (three sizes, 250 tokens each) and 2 of 40 deep-scrollback streams differ only in wrap markers or in the history line structure they cause. They are pinned by seed (`KNOWN_WRAP_GAPS`, `KNOWN_SCROLLBACK_WRAP_GAPS`), and each pinned stream must still differ, so a fixed one has to be unpinned. The random check ignores `WRAPLINE` and the leading-spacer flag (`wrap_free`); everything else is compared exactly: characters, zero-width marks, colours, attributes, links, wide glyphs, cursors, saved cursors, modes, title, palette and the scrollback text.

**Not measured yet:** serialization cost and output size per screen (the spike is about correctness); the snapshot clones the alternate grid when the alternate screen is active. The serializer reads only cols and rows: `GridSize`'s cell pixels feed window-size replies and nothing in the grid.

## P1 step 1: peer protocol (`csm-peer-proto`)

Base sha for this step: `c279407f800f8f0324ceadbd0cd5c41f118765d5`.

- **Decided tweaks done.** `inbox_next` declares `UI | LAUNCHES_PROCESS` (MCP callers are asked; it stays
  forwarded by the view overlay, now by its declaration alone: `FORWARDED_UI` is gone). `theme_set` is
  routed `Local`: the overlay holds the client's own `theme`, `theme_dark` and `theme_light`
  (unset until the client chooses); a client checks a theme id against its own `theme_list`.
- **Layers in the crate** (all pure, `vectors/*.json` + `tests/vectors.rs`, `CSM_UPDATE_VECTORS=1`):
  `frame` (`u32 BE len || Noise message`, max 65 535, empty and oversize refused on the header alone),
  `noise` (same IK and IKpsk2 parameters as the phone, prologue `"csm-peer/1" 0x00 host_id`, check
  code domain `"csm-peer check"`, rekey counter), `codec` (frame kinds: control JSON, `term_data`,
  `term_attach` chunks, `term_input`, control fragments for messages above one Noise message; the
  joined control JSON is capped at 4 MiB, an attach at 64 MiB), `msg` (the 3.2 message set plus
  `ConsentCard`, `ConsentAnswer`, `ConsentAck`), `delta` (`diff` and `apply`, property-tested),
  `pairing` and `addr` (`csm://pair#...` offer with a `tcp://` address; `check_bind` refuses
  `0.0.0.0` and `::` unless "all interfaces" was chosen).
- **Host id** is `base32(SHA-256("csm-peer host" || host static public))[..26]`, so an offer cannot
  name a host id that its key does not have; device ids the same with `"csm-peer device"`.
- **Strictness:** a `Hello` or `Welcome` of another protocol version is refused at decode
  (`CodecError::Version`) so the server can answer `Bye { protocol_mismatch }`; an unknown event
  inside `Msg::Event` decodes as `None` (ignore it); any other unknown message type is refused.
- **Attach and term data** use a client-chosen `stream: u32` from `TermSubscribe`, and absolute
  `offset`s in the server's per-session output.

## P1 step 2: transport, pairing, server mode switch (`csm-peer`)

Base sha for this step: `9581982f0ae26fbd1a0e7a3ecce8c2025219978d`.

- **Crate and layers.** `csm-peer` is L4 (beside `csm-mcp` and `csm-ui`; layer map and policy `DENY_ROOTS`
  updated). `snow` 0.10 is now a normal dependency there (it was already in `Cargo.lock` through the
  dev-dependency, so no new crate version; `cargo deny` was not run locally, the CI job covers it).
  The engine (L3) knows the server only through `csm_engine::PeerHost`; `StateOptions::peer_host` defaults
  to `NoPeerHost`, so tests, `--clean`, the demo and an isolated run have no remote access at all.
  `apps/codeshell` wires the real server (`<state folder>/peer`), calls `attach(engine)` and `resume()`.
- **Off by default, and proven by connecting.** `PeerServer::new` touches no disk and no network. The listener
  starts in `peer_server_enable` (UI only) and in `resume()`, which replays only `peer/server.json`, a file
  `enable` writes (and `disable` rewrites as off). It is not in `state.json`, not a setting, not in a `.csm`.
  `tests/peer.rs`: a fresh state, an upgraded `state.json`, a WPF import and a `.csm` import, each carrying
  hostile `PeerServer*` / `RemoteAccess*` keys naming a free port, then `resume()` and TCP connects that must be
  refused, with no `server.json` and no host key created.
- **UI-only tools are a precheck refusal.** `peer_server_enable`, `peer_pair_start`, `peer_pair_answer` are
  refused in `tools::precheck` for `Cli`, `External` and `Peer` (any level), before any consent card, and the
  refusal is written to the audit log. Their effects are declared honestly (state, files, network, extension), so
  consent would apply even if a future change let a caller through. `peer_server_disable` and
  `peer_device_revoke` are allowed for everyone; `peer_status` and `peer_device_list` are reads. `settings_set`
  has no such key (tested: NotFound for every spelling).
- **`Caller::Peer { device_id, name, level }`** is new. Only the server makes it, after a Noise handshake with a
  listed key; `csm-mcp`'s `accept_caller` refuses it on the wire. `is_external()` is true for it (so backup
  import and the app-only settings stay refused for a device) but consent cards are still asked of MCP clients
  only. A peer below Full may only run read-only tools (`level_gate`); only Full exists so far. Non-read calls
  from a device are audited (tool, session id, effects), never typed content.
- **Binding.** The tool checks the address (`check_bind`: an IP, no multicast, never `0.0.0.0` or `::` unless
  `all_interfaces`), the host checks it again, and `try_listen` binds exactly that socket address. A saved
  choice is re-checked on resume. An address that is not on this machine (a WireGuard tunnel that is down)
  leaves the server `enabled` with `waiting` set, retrying 1-2-5-10-30 s, and never listening elsewhere.
  With `all_interfaces` the pairing link has no single address to name, so `peer_pair_start` refuses it.
- **Files** (`peer/`): `host.key` (JSON, private and public half, the public key must match the private one),
  `devices.json` (public keys only; every entry re-validated on load: id derives from the key, name rules,
  no duplicates, at most 32), `server.json`, `remote-access.log` (append-only JSON lines, rotates at 4 MiB).
  The folder is 0700 and files 0600 on Unix; on Windows `icacls` (System32 path, argv, no shell, through
  `csm_engine::proc`) replaces the inherited ACL with the current user, and `peer_status.private_files_restricted`
  says whether it worked. A damaged key or list is an error, never silently replaced (that would unpair everyone).
- **Wire flow** (all of it in `csm-peer-proto`'s vectors): a cleartext selector frame (`01` session, `02 || id`
  pairing), then `IK` or `IKpsk2` with empty handshake payloads, prologue `csm-peer/1 0x00 host_id`. A stranger
  gets a closed socket and nothing else (no slot, wrong id, spent or expired slot, unknown key, garbage, a
  connection that stays silent past the handshake time); at most 8 unauthenticated connections at once.
  Pairing: the slot is consumed when the first transport message (`PairMsg::Hello`) decrypts; both sides derive
  the check code from the handshake hash; the device is added only when the UI answers Allow **with the check
  code** (a wrong code is refused, the third wrong code denies); Accept or Deny travels back over the same
  channel. A slot allows 3 handshakes; it lives `SLOT_SECS` (600) and works once.
- **Session link.** `Hello` / `Welcome` (protocol and term minor must match, else `Bye`), a second connection of the
  same device replaces the first (`Bye Replaced`), pings every 15 s and dead after 45 s, tool calls on their own
  thread (at most 16 at once; results over one Noise message are fragmented, over 4 MiB become an error reply).
  Rekeying is per direction as in the vectors (a test sends more than `REKEY_INTERVAL` messages).
  Snapshots, deltas and terminal streams are the next steps; `TermSubscribe` is answered `Forbidden` for now.
- **Revoke and off.** `revoke` deletes the key (or all), sends `Bye Revoked` and closes at once; `disable` joins the
  accept thread (the port is free when it returns), sends `Bye ServerOff`, withdraws the slot and any pending
  pairing, and saves the choice as off. The engine's exit path calls `shutdown()` (`Bye ServerQuit`), which keeps the
  saved choice so the next start resumes.
- **Consent from a device (decision Q3).** The server forwards `ConsentRequested` as `Msg::ConsentCard` (and
  `ConsentResolved` as an event) to every connected Full device, and sends waiting cards to a device that connects
  later. `consent_answer` is allowed for `Caller::Ui` and for `Caller::Peer` at Full; the first answer wins because
  the request leaves `pending` once, and the later answerer gets `NotFound` (`ConsentAck::AlreadyResolved` on the
  wire). Who answered is audited. Over the wire this is tested with two devices.
- **Not done here (later steps):** the Settings panel, indicator pill and QR code (the tools return the link as
  text), the pairing UX on the client side, snapshots and terminal streaming, the firewall step, `RemoteBackend`.

## P1 step 3: serving sessions to a device (`csm-peer`, `csm-engine`)

Base sha for this step: `d62400a757bd23fcf198e1411de30d771eeb6ce6`.

- **Snapshot and deltas.** After `Welcome` the device gets `Msg::Snapshot`; a publisher thread per connection compares the
  engine's `version` every 50 ms and sends `diff(last_sent, now)`. A burst coalesces, a slow link skips versions, a
  version where nothing the wire carries changed is not sent, and a delta above one control message falls back to a full
  snapshot. Events forwarded besides consent: `Toast`, `Error`, `ScreenChanged` (the rest are the office's own, or are in
  the snapshot).
- **Terminal streams** (`serve.rs`): one thread per `TermSubscribe`, at most 16 per device, reading the P0 output ring at
  its own pace (nothing queues). *Attach* = `TerminalHandle::attach_snapshot` (the serializer under the terminal lock, with
  the offset the emulator has consumed, taken in the same critical section as `Terminal::advance`, so grid and offset are
  one moment) then live bytes from that offset. *Resume* when the device names `(instance, offset)`, the ring holds the
  offset, the resize log still has every change after it and it is not further behind than `stream_backlog_bytes`.
  *Backpressure*: a stream whose unsent consumed bytes exceed the backlog (default 256 KiB), or whose offset the ring
  evicted or dropped, sends a fresh attach instead. Only bytes the server's emulator has consumed are sent, and
  `TermResized` is sent exactly at the offset the resize took effect (a per-session resize log), so the client's emulator
  is in lockstep (tested by comparing `replay::snapshot` of both emulators).
- **Known edge.** An attach flushes an open synchronized update (DEC 2026) first: the grid would otherwise lack bytes the
  offset already counts. The office pane may show that frame a moment early.
- **Input** goes to `TerminalHandle::send_key / send_mouse / paste / focus_changed` (paste is bracketed and stripped of
  ESC by the handle). Operator level or above; audited as a byte count per session per minute (and at disconnect), never
  content.
- **Size.** The device that last typed, focused (`Focus(true)`) or sent `TermResize { driver: true }` drives the PTY size
  (`TerminalHandle::resize_cells`, which keeps the cell pixel size). A device that does not drive only records the size
  it wants and gets it when it starts driving. The office window's own pane still resizes the PTY when it changes (it is
  not a "device"): nothing fights back, last change wins, as in the design.
- **Client helpers** (`csm_peer::client`): `Connection::subscribe/unsubscribe/resync/resize/input` and `TermFollower`, a pure
  tracker that joins attach chunks, checks data continues exactly where the last byte ended (`TermUpdate::Gap` = send
  `resync`) and gives the `(instance, offset)` to resume with. The next step's `RemoteTerminal` is built on it.
- **Consent** was done in step 2; step 3 adds the combined test (the office UI's card closes when a device answers).
- **Not done here:** compression, OSC 52 routing to the driver, run-output streaming, add-on screen proxying, the
  "typing" indicator, a `csm attach` CLI.

## P1 step 4: server mode UI on the office machine (`csm-ui`, `csm-peer`, `csm-engine`)

Base sha for this step: `89f07a013b789a40434a9e371b7892ca5bec2854`.

- **Settings > Remote access** (`screens/remote.rs`, a new section after Add-ons). One switch, "Let another CodeShellManager
  connect to this one", off by default; the page says so and that agents, the CLI and paired devices cannot turn it on.
  Turning it on opens a walk-through (ephemeral UI state, nothing is sent until the last button): **1** the address, from
  `peer_interfaces`, the WireGuard adapter marked and pre-selected, no wildcard row, a port (empty = 47300, 1-65535), and
  under *Advanced* an address that is not in the list and the "every interface" switch with its warning; **2** the firewall
  rule (a `netsh` line on Windows, a sentence elsewhere, scoped to the adapter's network), as text with a copy button:
  CodeShellManager adds no rule. "Turn on and pair a device" calls `peer_server_enable`, then `peer_pair_start`.
- **Pairing panel**: the link as text with Copy, a QR code (`qrcodegen` 1.8, MIT, no dependencies, error level L, drawn in
  the theme's darkest and lightest tokens), the minutes left, and when a device has handshaken its name and the four-digit
  check code with **Allow** (sends the code with `peer_pair_answer`) and **Deny**. The link is dropped as soon as the offer
  is gone or the server is off. A refusal from a tool is shown on the page.
- **Paired devices**: name, level, paired and last-seen ages, a dot while connected, **Revoke** per device and **Revoke all**
  (asks first), through `peer_device_revoke`. Listed while the server is off too.
- **Indicator** in the title bar between the brand and the search field, only while it is on: blue "Remote access on . addr",
  green "Laptop connected" (or "N devices connected"), yellow "Laptop wants to pair", peach while the chosen address is
  missing. It shortens, then shrinks to the dot, when the window is narrow; its accessible name is always the whole text. A
  click opens the page, where the switch (or "Turn off remote access") is the one click that turns it off. The tray tooltip and
  icon badge are not done.
- **Polling**: one `peer_status` at start; then `peer_status` and `peer_device_list` once a second only while the server is on
  or the page is open. Off and closed: no calls (tested).
- **New tool** `peer_interfaces` (read-only, every caller; `PeerHost::interfaces`, implemented in `csm_peer::netif` over
  `if-addrs` 0.15, MIT OR BSD-3-Clause: down, link-local, multicast and unspecified addresses are not offered, WireGuard by
  name `wg*` / `*wireguard*`). `NetInterface::network` (CIDR) is computed on the host side, so the UI needs no `std::net`.
- **Palette**: the enabling tools open the page instead of running with no parameters; `peer_server_disable` runs as is.
- **Demo and tests**: `demo::peer::DemoPeerHost` (memory only, off by default, no socket) backs `--demo`, screenshots and the
  kittests (`tests/remote_access_ui.rs`); `demo_engine_with_peer` lets a test drive "the other side".

## P1 step 5: laptop pairing and the host list (`csm-engine`, `csm-peer` client side)

Base sha for this step: `9c20e082108f5e854b04421d93c70c89ec0a83f6`.

- **Engine seam and tools.** `csm_engine::PeerClientHost` (`peer_client.rs`, default `NoPeerClient`, wired through
  `StateOptions::peer_client`) carries the client's state; the tools are in `tools/peer_client.rs`. `peer_client_pair
  {link, name?}` is UI only: `tools::precheck` refuses it for `Cli`, `External` and `Peer` before any consent card, as
  `peer_pair_start`. `peer_client_pair_status` and `peer_hosts_list` are reads; `peer_client_pair_cancel` is allowed for
  every caller; `peer_host_forget {host_id}` writes files, so MCP callers get a consent card for it.
- **Client** (`csm-peer`): `client::pair_cancellable` (a cancel stops the wait within a quarter second and closes the
  link, so the office withdraws the request; an answer that has already arrived is taken). `client_host::ClientPeer`
  runs one pairing at a time on its own thread and reports one status: `connecting`, `check_code` (the four digits),
  `paired`, `failed` (with why) or `cancelled`. Files in `<state folder>/peer-client/`: `device.key` (this app's key
  pair, made on the first pairing, a damaged file is an error and is never replaced) and `hosts.json` (`hosts::HostStore`:
  host id, name, address, the pinned host key, paired-at, last-connected; every entry is checked on load, a damaged file
  is an error, at most 64 hosts). The name is optional and defaults to the address.
- **Nothing listens, and nothing dials without a pairing.** `ClientPeer::new` touches no disk and no network; reading
  the list creates no folder. The only dial is a pairing the person started in the app.
- **Forget** removes the host entry and the host key pinned with it. This app's own device key is kept, because other
  hosts may still know it (a choice to confirm with the owner: "removes the key" could also mean the device key).
- **Tests.** `crates/csm-peer/tests/pairing_client.rs` (15, real TCP on 127.0.0.1, one engine playing both apps): allow
  (both screens show the same code, the host is stored, a connection with the stored key works), the default name, deny,
  a wrong code at the office then the right one, an expired offer, cancel (the office drops the request and a new pairing
  works), cancel with nothing running, forget (kept across a restart, the device key stays), the precheck refusals for
  each caller (nothing written), bad input, a fresh laptop writes nothing, a damaged host list and a damaged device key
  are errors and are left alone, the list survives a restart, and an unreachable host fails cleanly. Unit tests in
  `hosts.rs` and `client_host.rs`.
- **Not done.** The laptop's "Connect to another CSM…" dialog and the check-code screen (the palette runs
  `peer_client_pair` with no parameters, which is refused as a missing link until the dialog exists). Connecting to a
  stored host from the laptop and `RemoteBackend` (step 6). `last_connected` is stored but nothing sets it yet. The
  laptop's own device name is the computer's name.

## P1 step 6: RemoteBackend and RemoteTerminal (`csm-remote`)

Base sha for this step: `76d93e45674fe1456e663a2ffa96711e2006f868`.

- **Where it lives.** A new crate `csm-remote` at L5. `csm-ui` (L4) cannot depend on `csm-peer` (L4), and
  `apps/codeshell` (L6) would leave the integration test with no library to import. The crate uses no egui of
  its own: it implements `csm_ui::Backend` and `csm_ui::ToolPort` (the default `tick` is not overridden).
  Listed in the layer map and in `DENY_ROOTS` (unwrap, expect, panic, indexing rules).
- **Worker thread** (`link.rs`). One thread owns the `csm-peer` connection, the mirrored snapshot, every
  terminal stream's `TermFollower` and the emulator feed. The UI thread sends `Cmd`s and polls `Up`s; it never
  waits on the network or on an emulator lock (the view only `try_lock`s).
  - Connect, then `tool_list` (the host's tools with their effects; calls that arrive before it wait in order).
  - Snapshot, then deltas via `delta::apply`. A delta that does not apply reconnects for a fresh snapshot.
  - Retries 1, 2, 5, 10, then 30 s. Each terminal resumes from `(instance, offset)`; a gap asks for `term_resync`.
  - Refusals end the worker with `LinkState::Refused` and a reason: revoked, protocol or terminal mismatch,
    replaced by another connection, or the host closing silently at the handshake (not on the list).
  - A call in flight when the link drops is answered "may have run, check the result"; it is never retried.
- **`RemoteBackend`** (`backend.rs`). Calls go through the view overlay: `Local` (view_set, layout_*, inspector_show,
  inbox_panel_set, window_placement_set, group_*, theme_set) are answered on the laptop; `Focus` (session_focus)
  is applied locally and also sent to the host (acknowledgement, wake), and if the host refuses the overlay is put
  back; everything else, including `UiNotModelled`, is forwarded. The model is rebuilt from the mirror and the
  overlay after each poll. `show_terminals(visible)` subscribes new panes and unsubscribes hidden ones;
  `set_pane_driver` marks the typing pane.
- **`RemoteTerminal`** (`terminal.rs`). A `TerminalSource` over a local `csm-term` emulator (replies discarded). The
  emulator is replaced on attach and resized only on `term_resized`, in stream order. Keys, mouse, focus and paste go
  as typed input (paste is bracketed by the host). `set_grid` sends a resize only when the pane's size changes.
- **Mapping** (`map.rs`, pure). Wire snapshot to `UiModel` (the sessions, git, runs and open-with from the host;
  groups, rail, focus, inspector, inbox panel and layouts from the overlay), wire events and outcomes to the
  engine's types, tool rows to `ToolInfo`. Effects are read from their flag names; an unknown flag means "every
  effect", so the tool is forwarded rather than taken as UI-only.
- **Consent.** Cards arrive as `Event::ConsentRequested`; the answer is the host's `consent_answer` tool, so the
  first answer wins on the host as before.
- **Tests.** `csm-remote` unit tests (mapping, backoff, refusals) and `tests/remote.rs` (in-process, real engine
  with a fake PTY, real `csm-peer` server on 127.0.0.1, a TCP proxy the test can cut): the session list; a rename
  as a proxied call with its delta in the mirror; the view calls kept on the laptop and a focus that reaches the
  office; a terminal that follows the office's grid (lockstep after output and after a resize), types, pastes
  and resizes the PTY; a dropped link that resumes with the gap filled in; a revoke that ends the window as
  refused; a consent card shown on the laptop, denied there, and the office's second answer finding nothing.
- **Not done.** The app wiring (the connect dialog, the indicator, hiding the local-only items, Quit as disconnect,
  the window's own overlay persistence: the caller supplies it, nothing saves it yet). `tool_list` has no input
  schemas, so remote tools have `Value::Null` schemas until the protocol carries them. OSC 52 clipboard routing,
  run-output streaming, add-on screen proxying, theme events from the host (the theme is the laptop's own), the
  typing indicator and compression. Every snapshot update sends the full mirror to the UI thread (no coalescing
  beyond the host's). Terminal cell pixel sizes are the default (only columns and rows are replicated).

**Fix round 1 (base `76d93e4`, the step's own commit `397b44e`).** Review found host-supplied times in
`csm-remote::map` doing unchecked i64 arithmetic: a hostile office could panic a debug build inside `rebuild`.
Now `took` and `run` saturate (`saturating_sub`, `saturating_add`) and a test runs every extreme pair of
`started_ms` / `ended_ms` (`i64::MIN`, `i64::MAX`, negatives) through the run view. A run that ends before it
starts shows `0 s`. Not changed in this round: terminals are not marked stale while reconnecting, `Up::Snapshot`
sends are not coalesced, and the bad-delta-forces-a-fresh-snapshot path has no test through the real link.

## P1 step 7: the laptop's connect dialog and the remote window (`csm-ui`, `csm-remote`, `apps/codeshell`)

Base sha for this step: `f0f7c0f2d0317cc333a5dc5762c5c77170888d0d`.

- **Connect dialog** (`screens/connect_remote.rs`, `remote_window::ConnectDialog`). "Connect to another CodeShellManager…" is the
  `peer_connect_open` tool (UI-only, refused for every other caller) and a palette row, opened from the laptop's own window only.
  It pastes a link, takes an optional name, starts `peer_client_pair` and polls `peer_client_pair_status` (0.4 s) while a pairing
  runs. It shows Connecting, the four-digit check code, Allowed, the refusal and Cancelled, and lists the paired offices from
  `peer_hosts_list` with **Connect** (starts a remote window) and **Forget** (asks first, then `peer_host_forget`). Pairing
  is still only started from the app; the dialog never runs in a remote window.
- **Remote window** (`csm-ui::remote_window`, `csm-remote::shared`, `apps/codeshell --remote <host-id>`). Connect starts this exe
  with `--remote <host-id>` (plus `--config-dir` when the laptop uses one) through `csm_engine::proc::spawn_detached`, on a thread.
  The window starts no engine: `csm_remote::window_params` reads `peer-client/hosts.json` and `device.key` (it never makes a key
  and refuses a host that was not paired), then `RemoteBackend` connects. One backend is shared by the window's two faces
  (`RemotePort`, `RemoteView`, `RemoteHandle`), so the window keeps one connection. The worker's state is shown by the strip under
  the title bar, "Connected to OFFICE · address", "Connecting to…", "OFFICE offline · reconnecting", or "OFFICE refused this laptop:
  reason" (theme tokens; `Backend::remote()` supplies it, and the window title is prefixed with the host name).
- **Local-only items** (design section 6): `remote_window::LOCAL_ONLY` lists open folder, open with (and its submenu, the
  "Open with" action), "Open a shell here", backup export and import, and the connect and pair tools. A remote window hides them
  from menus and the palette, and `CsmApp::run` refuses them with a toast before they run (the Settings backup buttons take that
  refusal; they are not hidden). A pasted
  image is refused in the pane (`ViewResponse::image_paste`, `ViewOptions::remote`), a dropped file becomes a toast
  (`file_drop`), and the native folder dialog is never raised (`take_folder_request` and `take_dialog_request`).
- **Links.** Ctrl+click (and the other `open_url` callers) in a remote window goes through `check_link` (the engine's
  http/https check), then the laptop's browser (`RemoteHooks::open_link`). A loopback host (`localhost`, `127.0.0.1`, `::1`,
  `*.localhost`) asks first: "This link points to OFFICE".
- **Quit.** In a remote window Quit (title bar, palette or `app_quit`, and closing the window) **disconnects**: `CsmApp::disconnect` closes the
  window and never calls `app_quit` on the office. "Quit CodeShellManager on OFFICE…" (palette, a confirm) forwards `app_quit` to the
  host; when the host answers, the window disconnects.
- **Per-client view state.** The window keeps its layout, focus, rail, inspector and theme in `peer-client/view-<host id>.json`
  (`csm_remote::connect::{view_path, load_view, save_view}`, written on a worker thread once a second when it changed). `ViewOverlay`
  now serializes. A damaged file is no state; a host id that is not a plain name gets no file.
- **Consent cards** from the office's agents use the existing card UI over the mirrored model; the first answer wins on the office
  (`consent_answer` is forwarded). The backend side is tested in `csm-remote`; the card is not separately kittested here.
- **Tests.** `csm-ui/tests/remote_window_ui.rs` (9): the strip in each link state, the local-only items hidden from the palette and
  menus, refused calls not sent, Quit disconnects and the office quit asks first, links (a loopback link warns), the dialog's
  states (no link no pairing, a refused pairing shows why, the check code), and Connect / Forget on the host list.
  `csm-ui/src/remote_window.rs` unit tests (link check, loopback, dialog answers). `csm-remote/tests/connect.rs` (3: the paired
  office is read, an unpaired one is refused and nothing is made, a saved view is seeded) and `tests/remote.rs`
  (`a_window_shares_one_connection_between_its_port_and_its_view`). Screenshots: `remote_window_screens` in
  `crates/csm-ui/tests/screenshots.rs` (`23-remote-*`, `23-connect-dialog*`).
- **Not done.** A window that cannot connect (no key, an unpaired host, a damaged list) writes its reason to stderr and exits;
  it has no window of its own to say so. The laptop's dialog was reachable from the palette only (the F10 main menu, built later from the tool list, lists it too; no
  title-bar button). A remote window does not know the laptop's theme list (`ConnectParams::themes` is empty), so a saved theme
  is only as good as the window's own list. The window polls its link every 100 ms (the link thread does not wake the UI).
  Run-ready URLs (`post_run_url`) do not go to the laptop's browser yet. Remote windows have no tray, no OS notifications and
  no window placement memory. The localhost warning's dialog answers Enter as "Open" like every confirm dialog.

## P1 step 8: `csm attach` and the end-to-end client test (`csm-remote`, `apps/csm-cli`)

Base sha for this step: `5faf218bf4795a12a0ca6e66de0450eff616b47e`.

- **`csm attach`** (C1, a harness and a way in from any console). `csm attach --offices` lists the paired offices,
  `csm attach --list` the office's sessions (`session_list`), `csm attach <session>` (id or unique name, `--host` when
  several are paired, `--config-dir` for a non-default app folder) attaches this console. It is a command, not a tool:
  it is a terminal stream, and `docs/tools.md` says so. It never pairs: it only dials an office from the app's
  `peer-client/` folder (`csm_remote::attach::connect` → `connect::paired`), and a missing key or host is refused
  with nothing written (tested through the real folder in `tests/attach_folder.rs`, where the office is paired by a
  laptop engine's `peer_client_pair`).
- **The loop** (`csm_remote::attach::run`, pure against a `Console` trait). It subscribes to stream 1, draws the
  office's serialized grid after clearing the console, then the live bytes, and joins a `TermFollower` as the
  remote window does. Keys go as `term_input` key bytes, a paste as `Input::Paste` (the office brackets it). The console
  resize is sent as a driver resize only when the console changed size itself (a `Resized` event), or once on the first
  attach when the session has another size (`Sizing`). A frame that reports another device's size makes the console
  resync and draw at the session's size, and never claims that size back, so two consoles of different sizes settle
  (fix round 1: the office gives the driver role to every driver resize, so an echo-driven claim looped). A gap asks
  for `term_resync`. The session's
  `TermClosed` ends the attach (`SessionEnded`), a dead link ends it (`LinkLost`, with the reason in plain words).
- **Detach** is Ctrl+] then `d` (`attach::Keys`, a pure state machine: Ctrl+] Ctrl+] sends one Ctrl+], Ctrl+] then
  another key sends that key). It unsubscribes and closes; the session keeps running (tested: still listed, its
  process still alive).
- **The console** (`apps/csm-cli/src/attach_cmd.rs`) is `crossterm` 0.29 (MIT; features `events`, `bracketed-paste`,
  `windows`; new to the build: it is the one safe raw-mode crate, since no `unsafe` is allowed outside `csm-pty`).
  Raw mode and the alternate screen with bracketed paste, restored by a guard whatever ends the attach. Key bytes
  are xterm-style (`key_bytes`, unit tested), arrows in CSI form. `csm attach` is listed in `csm --help`; its exit
  codes are 0 (detached, or the session ended), 2 (usage), 3 (office unreachable or link lost). `cargo deny` was not run
  locally (CI covers it).
- **Test harness.** The in-process office moved to `crates/csm-remote/tests/common/mod.rs` (`Office::boot`,
  `Office::restart`, `free_port`), shared by `remote.rs` (unchanged cases), `attach.rs` (eight cases on a real office:
  draw and type, Ctrl+] Ctrl+], paste, the console's size on attach and on change, two consoles of different sizes
  settle with a bounded number of redraws, a session closed under the console, a dropped link) and `e2e.rs`.
- **End to end** (`tests/e2e.rs`, three tests, one process: the office's engine and `csm-peer` server, two laptops with a
  `RemoteBackend` each, the fake PTY the only fake): (1) two laptops type into one session in order, both panes match
  the office's emulator, and a resize from one follows to the PTY and both panes; (2) an agent's consent card is shown
  on both laptops, laptop A answers, the card closes on laptop B and the office (the agent's call goes ahead), and a
  second answer finds nothing (the first answer won); (3) the office restarts (engine and server stopped, the same
  folder and port, sessions restored with their ids), both laptops reconnect, the restored session is a new process
  so each laptop attaches afresh (the old screen is gone), and typing works again.
- **Not done in this step.** Manual tests over a real WireGuard tunnel (firewall prompt, tunnel down at start-up, a large
  `git diff` burst, latency feel) are still to do. `csm attach` does not reconnect: a lost link ends it (the remote
  window does reconnect). Its keys are CSI-only (no application cursor mode), it sends no mouse or focus reports, and
  when another device resizes the session the console redraws at that size (it does not resize back). The
  tray tooltip and icon badge are not done (step 4). Still open from P1, all listed above: OSC 52 routing to the
  driver, run-output streaming, add-on screen proxying, the typing indicator, compression, the window's own reconnect
  error screen, and the step 6 fix round items (no coalescing of `Up::Snapshot`,
  the bad-delta path through the real link).
- **Fix round 1 (review of step 8).** The resize ping-pong between two consoles of different sizes is fixed (above,
  tested). Office-supplied names, ids, statuses and the office's name print through `printable`, where control
  characters show as `?` (tested). The `csm attach` loop still writes the session's bytes raw, as ssh does.

### P1 status (after step 8)

- **Done:** the office side (steps 1-4: protocol, transport, pairing, serving snapshot, deltas and terminal streams,
  the Settings page, the indicator, revoke and off); the laptop side (steps 5-7: pairing from the app, the host list,
  `RemoteBackend` and `RemoteTerminal`, the connect dialog, the remote window with its local-only rules, Quit as
  disconnect, the per-client view state); `csm attach` and the in-process end-to-end tests (step 8).
- **Open for P1:** the manual WireGuard checks, and the items listed under step 8 "Not done". P2 (resume from offsets
  with the reconnect UI, consent cards on peers already work, OSC 52, run output, add-on screens, compression, the typing
  indicator) and P3 are not started except where the list above says otherwise.
- **User docs:** `docs/remote-access.md` (office: Settings › Remote access; laptop: Connect to another CodeShellManager;
  `csm attach`), linked from `docs/README.md`. `docs/tools.md` notes that `csm attach` is not a tool; its catalogue
  is unchanged (no tool was added or changed in this step).

## P1 client review: fix round (`csm-remote`, `csm-ui`, `apps/codeshell`, `csm-engine`)

Base sha for this round: `9c20e082108f5e854b04421d93c70c89ec0a83f6` (the review was of the client side, steps 5-8, at `4fb7458`).

- **Blocker: terminals were never subscribed in the real remote window.** `Backend::show_panes(visible, driver)` (a
  new default no-op on the trait) is called at the end of each frame with the sessions whose panes the frame drew
  (planned panes, floats, torn-off panes; live sessions only) and the focused session. `RemoteBackend::show_panes`
  subscribes the shown panes, drops the rest, and marks the focused pane as the driver. `RemoteTerminal::set_driver`
  sends only a change, so the per-frame call costs nothing. Tests: `csm-remote/tests/remote.rs` drives a pane through the
  `Backend` trait only (`shown` then the office's grid, the focused pane's resize reaching the PTY, hidden unsubscribed);
  `csm-ui/tests/remote_window_ui.rs` checks that the frame reports its panes and the focused one, and that the board shows none.
- **Blocker: a remote window could not close.** `background::close_requested` in a remote window sets `disconnect` and
  sends no `CancelClose` (a cancel was re-queued by the event loop every pass). `logic` does not treat the close as
  pending once `disconnect` is set. `apps/codeshell` stops the link once (`RemoteHandle::stop`, after saving the view
  state) and then sends `Close`. Tests: `remote_window_ui.rs` (a close, repeated closes, and Quit followed by a close:
  never `CancelClose`, never `app_quit`). The real window close is still to be checked by hand.
- **Office events on the laptop.** `map::event` no longer applies `WindowShowRequested`, `NotificationTestRequested`,
  `QuitReady`, `ClipboardOffered` or `SettingChanged`. Settings stay per client: a host's setting is not followed (the
  laptop's own preferences are its own). Test in `map.rs`.
- **Stale snapshot.** With a snapshot in hand, `Reconnecting` and `Stopped` mark the model `stale`; the canvas is veiled
  (`theme::alpha::STALE`) and the strip keeps saying why. Calls made while the link is down still fail at once. Tests in
  `csm-remote` and `csm-ui`. The veil does not reach floating panes or torn-off windows (they draw in their own layers).
- **Paired devices and the pairing tools.** `peer_client_pair_status`, `peer_client_pair_cancel`, `peer_hosts_list` and
  `peer_host_forget` are refused for `Caller::Peer` (`PEER_REFUSED`). The CLI and MCP callers keep them (cancel and forget
  for MCP still ask first). Test in `pairing_client.rs`.
- **Loopback links.** `is_loopback` parses the host: `localhost` and `*.localhost`, IPv4 in 127/8 and 0/8 in every spelling a
  browser accepts (`127.1`, `0x7f000001`, `2130706433`, `0177.0.0.1`), IPv6 `::1` and `::`, and IPv4 in IPv6 form
  (`::ffff:127.0.0.1`, `::127.0.0.1`). Unit tests in `remote_window.rs`.
- **Not done in this round.** Board previews in a remote window are blank (the board would subscribe every session; the
  host allows 16 streams per device). Remote terminals are not veiled inside floating panes. Still open from step 8: the
  manual WireGuard checks.

## P2 follow-ups (`csm-remote`, `csm-peer`, `csm-peer-proto`, `csm-engine`, `csm-ui`, `apps/codeshell`)

Base sha for this round: `b16b6c48f57aaa493d627f3e32d4f9cc2d4e1060` (the final P1 reviews, after the office moved to `v2`).

- **Mirror queue.** `Up::Snapshot` no longer carries a copy. The worker keeps the newest mirrored snapshot in a
  [`Latest`] slot and sends one wake (`Up::Snapshot`) while none is pending; the window takes the slot when it handles the
  wake. A chatty office costs one copy, not a queue. Test: `link.rs` (`the_window_gets_only_the_newest_snapshot...`).
- **Office notifications are rate-limited.** At most 10 office events (toasts and the like) per second reach the window;
  the first one dropped in a second is replaced by one "More notifications were dropped." toast. Consent cards and their
  resolutions are never limited (a person waits on them). Test: `link.rs` (`notices_are_admitted...`).
- **A delta that does not apply asks for a snapshot, on the same link.** The worker sends the new protocol message
  `SnapshotRequest` (client to office; vectors regenerated) instead of dropping the connection. While it waits, deltas are
  ignored (the snapshot is the new base). The office's publisher answers with a full `Snapshot` and goes on from that
  version. Test: `csm-remote/tests/remote.rs` (`a_delta_that_does_not_apply_is_fixed_with_a_snapshot_on_the_same_link`):
  the office is built with `ServerConfig::corrupt_next_delta`, a test-only knob that sends one delta with a wrong base;
  the test checks the session arrives and the connection count does not change.
- **Terminals are veiled while the link is down.** A stream that is running goes to `STALE` when a reconnect starts and
  back to `RUNNING` when the link is live again. The UI reads it as `SourceState::Stale`: the pane draws its last screen
  under the stale veil (`theme::alpha::STALE`). Test: `remote.rs` (`a_terminal_is_veiled_while_the_link_is_down...`).
- **The inspector's width is per client.** `ViewOverlay::inspector_width` holds it; `inspector_panel_set` is applied to the
  overlay (`Route::Local`), seeded once from the first snapshot, and the remote model reads the overlay. `FIXED_NOT_MODELLED`
  no longer lists it. Tests: the overlay-versus-engine walk in `view_overlay.rs`, and `map.rs`
  (`the_inspector_width_is_the_clients_own_not_the_offices`).
- **`remote_window_open` reaches a launcher.** `StateOptions::remote_window_launcher` (default: refused). For the app's own
  window the tool checks the host id is a paired host, then calls the launcher. `apps/codeshell` wires it to start this exe
  as `--remote <host-id>` (argv, never a shell). The Connect button submits the tool; the engine's reply is the toast.
  The `RemoteHooks::open_window` closure is gone. Tests: `csm-peer/tests/pairing_client.rs`
  (`connect_starts_a_remote_window_only_for_a_paired_host...`), `csm-ui/tests/remote_window_ui.rs`.
- **The check code is for the app's own window.** `peer_client_pair_status` returns the state and the outcome to the CLI and
  MCP callers, and the digits only to the app's window. Test: `pairing_client.rs` (`the_check_code_is_shown_to_the_app_only`).
- **Paired devices and the host list.** `peer_hosts_list`, `peer_host_forget`, `peer_client_pair_status` and
  `peer_client_pair_cancel` were already refused for `Caller::Peer` (the P1 fix round, `PEER_REFUSED`); the existing test
  still holds.
- **A cancelled pairing ends during connect and handshake.** The dial checks the cancel flag before each attempt; the
  handshake's frame reads check it every quarter second (`LinkError::Cancelled`). One TCP connect attempt can still take up
  to the connect timeout (10 s); the flag is checked again right after it. Test: `pairing_client.rs`
  (`a_cancel_ends_a_pairing_that_waits_in_the_handshake_at_once`).
- **Trust is written down.** `docs/remote-access.md`: `csm attach` and the remote window carry this laptop's device key.

**Decisions to confirm.**
1. `SnapshotRequest` is added without bumping `PROTOCOL_VERSION` (still 1): nothing has shipped with the peer protocol yet.
   If a release has gone out by the time this lands, bump it.
2. Notices: 10 per second, one note per drop burst. Consent cards are never limited.
3. A laptop's Connect now goes through the tool, so an agent that somehow reached `remote_window_open` as the app's window
   would start a window too. The tool still refuses every caller but the app's window.

**Not done.** A blackholed host still holds the pairing thread for one connect timeout before a cancel is seen (the
standard library has no cancellable connect). Board previews in a remote window are still blank. The manual WireGuard
checks are still open.

## P2: remote over the relay (`csm-relay`, `csm-peer`, `csm-remote`, `csm-engine`)

> **Where the relay lives.** The relay server (`csm-relay`) and the Phone remote add-on are in the private
> `umage-ai/csm-relay` repository. CSM does not build the relay. CSM keeps a vendored copy of the wire
> protocol in `vendor/csm-remote-proto` (see `vendor/csm-remote-proto/CSM_VENDOR.md`).
>
> **Testing through a real relay.** `crates/csm-peer/tests/peer_relay.rs` runs a CSM peer through the
> real relay binary. Build the relay from `umage-ai/csm-relay` (`cargo build -p csm-relay`), then run:
> `CSM_RELAY_BIN=<path to csm-relay.exe> cargo test -p csm-peer --test peer_relay -- --ignored`.
> Without `CSM_RELAY_BIN` the tests panic with the same instruction, so they never pass by accident.

Base sha for this round: `a40fe7079452eb999e1baf6fc0a2c71b1827ecd2`.

- **Relay: a peer namespace.** Hosts and channels are keyed by `(Kind, host id)`, `Phone` or `Peer`. `/v1/peer-host` and
  `/v1/peer-client` are the peer endpoints; a phone can never reach a peer host and the reverse (tests in
  the csm-relay repository's `tests/relay.rs`). Both refuse any `Origin` header. The peer caps (`peer_*` in `[limits]`, defaults in
  `csm-remote-proto::limits`) are higher than the phone's, with a 1024-frame client queue for bursts.
- **Transport.** `csm-peer::transport::Stream` is TCP or a relay channel; the Noise link, the server and the client use it
  unchanged. A relay channel is a byte stream over relay frames (writes cut into 16 KiB frames; the write side is a bounded
  queue, so a burst waits instead of being dropped). `csm-peer::relay_pump`: `dial` (the laptop's channel, one WebSocket
  per channel) and `RelayHost` (the office's one WebSocket for all channels, with reconnect backoff 1-2-5-10-30 s, the
  pairing slot replayed after a reconnect, and the allow list resent on `resync`).
- **Routing token.** A device's relay token is `sha256("csm-peer relay token" || device public key)`; the relay's allow list holds
  `token_hash` of it. Both sides derive it from keys they already have, so no extra message exists. Revoking a device resends
  the allow list, and the relay closes its channel with `revoked`.
- **Offer.** `PeerOffer` has an optional `r: {url, host_id}`. Old links decode unchanged. `url` is `wss://`, or `ws://` to
  loopback only (a plain-text relay would carry the enrolment token in the clear). The laptop tries the direct address,
  then the relay (`client::connect_saved`, `open_route`). `hosts.json` keeps the route (`Host::relay`).
- **Pairing over the relay.** The offer's pairing id is the relay's pairing slot: `PairingOpen` at `pair_start`, and
  `PairingClose` after the answer is queued (the relay's queue keeps the answer ahead of the close). Closing at consumption
  killed the pairing channel in flight; the end-to-end test caught it.
- **Keys.** The office's relay secret and the enrolment token live in the keychain (`codeshell-peer`, keys `relay.host-secret`
  and `relay.enrol-token`); the address is in `peer/relay.json`. The relay runs only while remote access is on
  (`enable` restores it, `disable` stops it).
- **Engine.** `peer_relay_status` (read-only, App) and `peer_relay_set` (UI-only, like `peer_server_enable`). `PeerHost` gained
  `relay_set` and `relay_status` with defaults for the no-op host.

**Decisions to confirm.**
1. The relay's peer endpoints share the operator's access rules (`[access]`), so one token admits phones and offices alike.
   A separate peer allow policy is possible if you want it.
2. Plain `ws://` is accepted only to `127.0.0.1`, `localhost` or `[::1]` (tests, a relay on this PC), checked by a strict
   parser (`csm_peer_proto::pairing::relay_url_allowed`): userinfo (`@`), backslashes, fragments, whitespace and control
   characters are refused, and the host must be exactly one of those literals. Anything else must be `wss://`. The
   check guards the address in an untrusted pairing link and in `hosts.json`, so a parser that disagrees with it could
   send plain text to another machine; the unit tests list the bypasses that are refused.
3. The office's relay identity is its own secret (not the phone add-on's), so the two can be set up independently.
4. `SnapshotRequest`-style versioning is not needed here: the relay carries the existing peer protocol unchanged.

**Review round 1 (the relay fixes).**
- *Ordering.* The office's control messages (`PairingOpen`, `PairingClose`, `Allow`) and its frames share one queue, so
  they reach the relay in the order the server sent them. Two queues let `PairingClose` overtake the pairing answer,
  and the relay then closed the laptop's channel before the Accept (the relay tests caught it).
- *Bounded writes.* A channel's writer waits at a gate of 256 queued frames; the pump keeps at most 128 more pending
  while the pacing holds them. The pump sends each channel's frames no faster than the relay's per-channel caps
  (`Pacer`, the peer defaults), so a burst waits at the office instead of tripping `rate_limited`.
- *No stall on inbound.* The relay thread hands inbound data to each channel with a non-blocking send into a queue of
  1024 frames. A channel whose reader falls that far behind is ended (the relay is told), instead of blocking the thread
  that serves every channel.
- *Locks.* The relay is taken out of its mutex before it is dropped (`Inner::relay_stop`), so no join runs under the
  server's locks. `RelayHost::drop` detaches the thread: it stops at its next step, and a connect or a welcome wait is
  bounded (10 s each), so the drop never waits on the network.
- *Epochs.* Each channel opening gets an epoch; a writer still holding an earlier incarnation of the same channel number
  cannot write into a new one.

**Not done.**
- ~~The Settings › Remote access form~~ Done in round 2 (see below).
- The routing token is `sha256(device public key)`, so anyone who knows a device's public key can open relay channels
  for it (Noise still stops them reading or pairing, but they use the office's slots). A per-device secret mixed into
  the token would close that; it changes the allow list format, so it is a follow-up.
- The pairing link's QR carries the relay only through the offer's JSON, which is already what the QR encodes; no QR
  test was added.
- Manual checks through a real relay (TLS, a public host) are not done; the tests use `ws://127.0.0.1`.

**Review round 2 (the relay form and the token).**
- *The form.* Settings › Remote access has "Also reachable through a relay" (`screens/remote.rs`, `relay_card`). The
  toggle opens the address and token fields (off: the relay is removed). Save, Change and Turn off go to
  `peer_relay_set` through the dispatch, so the UI thread never waits. The card shows `peer_relay_status`: connected, or
  why not. The pairing link is made again when the relay changes, so the offer names the current route. The token is a
  password field and leaves the page state at once when sent. Tests: `relay_tests` (the params, replies, Debug output),
  and `the_relay_card_sends_the_token_once_and_keeps_it_out_of_the_logs` (kittest, demo host).
- *The token stays out of logs.* `backend::redacted_params` replaces `access` with `<redacted>` in the debug line and the
  call history; the engine still gets the real params. `RelaySetRequest` and `RelayConfig` print their tokens and host
  secret as `<hidden>`.
- *The token's meaning.* `access` absent keeps the saved token only for the same address, else removes it; `""` removes
  it; text saves it. Before, an absent token was deleted on every save, so changing only the address lost the token.
- *Not started while off.* Saving a relay while remote access is off used to start it. It is now only saved, and
  `peer_relay_status` lists it as not connected ("remote access is off, so the relay is not running"). Turning remote
  access on starts it (`restore_relay`), as before.
- *Still open (the routing secret).* The routing token is still `sha256("csm-peer relay token" || device public key)`,
  so the allow list can be opened by anyone holding a device's public key. Fixing it needs a per-device routing secret:
  the laptop makes it, sends its hash in pairing, the office stores that hash per device, and the laptop keeps the secret
  with its saved host. That touches the pairing message, the device records and the saved host, so it is not
  a cheap change and is left for a round that changes those formats anyway. Noise still gates every session.

## P2 follow-ups, final review round 1 (`csm-peer`, `csm-remote-proto`, `csm-remote`, `relay`, `csm-engine`)

**Already in place when this round started (checked, with the test that holds each one).**
- *Snapshot coalescing and the notice limit (follow-up 1).* `csm-remote/src/link.rs` keeps one snapshot slot and one
  pending wake (`Latest`), so the window's queue never grows with the office's deltas. Office notices pass through a
  fixed rate (`NOTICES_PER_SEC`, one "more dropped" note); consent cards are never limited. Test:
  `the_window_gets_only_the_newest_snapshot_and_one_wake_at_a_time`, `notices_are_admitted_at_a_fixed_rate...`.
- *A delta that does not apply (follow-up 2).* The worker asks for a fresh snapshot on the same link
  (`Msg::SnapshotRequest`, answered in `csm-peer/src/serve.rs`), and the terminals are marked stale while it is
  away. Test: `remote.rs`, the corrupted-delta test. That test (`a_delta_that_does_not_apply_is_fixed_with_a_snapshot_on_the_same_link`) uses the
  `corrupt_next_delta` knob of a fake office, a proxied link, and checks that no reconnect happened.
- *Inspector width per client (follow-up 3).* `inspector_panel_set` is a `Local` route in the fixed table
  (`csm-ui/src/view_overlay.rs`), so the width is the window's own. The table test enforces each UI tool's route.
- *`remote_window_open` (follow-up 4).* `StateOptions::remote_window_launcher` is wired by `apps/codeshell`, which starts
  `codeshell --remote <host-id>` by argv. The Connect button reaches it. `peer_client_pair_status` leaves out the check
  code for every caller but the window (`tools/peer_client.rs`). Test: `csm-peer/tests/pairing_client.rs` (the launcher).
- *Refusals for a paired device (follow-up 5).* `PEER_REFUSED` covers `peer_hosts_list`, `peer_host_forget`,
  `peer_client_pair_status` and `peer_client_pair_cancel`. Test: `pairing_client.rs`, the loop over the four ids.
- *Cancel during connect and handshake (follow-up 6).* `dial` checks the cancel flag between attempts, and the
  handshake's reads give up within a quarter second of a cancel (`client::pair_cancellable`). Tests:
  `a_cancel_ends_a_pairing_that_waits_in_the_handshake_at_once` and the cancel tests in `pairing_client.rs`.
- *The trust note (follow-up 7).* `docs/remote-access.md` says that `csm attach` and the remote window carry this
  laptop's device key, and what that lets a program on the laptop do.

**Relay flow control and close (the final review's blockers and majors).**
- *Closing never waits.* `Sink::close` is a separate method. The office's close is a `Cmd::Close` on the unbounded
  command queue, not counted by the gate, so a full gate cannot hold it. The laptop's close sets a flag the pump checks,
  and queues a close only if there is room. An Open that arrives while the gate is full and the stream is closed at once
  does not deadlock (test: `an_open_arriving_while_the_gate_is_full_does_not_deadlock`).
- *A stalled relay ends the connection.* The relay socket has a 5 s write timeout (`WRITE_STALL`). A relay that stops
  reading ends the session, the gate and the windows are released, and the reconnect loop takes over. Test:
  `a_relay_that_stops_reading_cannot_hold_a_channel_close`.
- *End-to-end window.* The laptop reports what its reader has taken: a text `{"t":"ack","frames","bytes"}` on the peer
  client socket (peer clients only; the relay forwards it as `FrameKind::Ack`, counts only). The office keeps at most
  512 frames and 2 MiB in flight per channel (`WINDOW_FRAMES`, `WINDOW_BYTES`), and a writer waits at the window.
  A stall of 30 s with no acks ends the channel. A well-behaved office never fills the relay's queue (the window stays
  under the 1024-frame queue). A slow laptop therefore slows the office and the backlog rule applies (a fresh screen
  when far behind). Tests: `the_window_makes_a_writer_wait...`, `closing_a_window_releases...`, and the relay test
  `a_slow_laptop_reader_keeps_the_link_and_the_final_screen` (4 MiB while the laptop does not read).
- *Rates from the relay.* The Welcome carries `frames_per_second` and `bytes_per_second` for the kind of host. The office
  paces to them (zero means the old defaults, for an older relay). `max_channels` is the kind's own cap.
  Test: `the_relays_announced_rates_are_used...`.
- *No head-of-line blocking.* A channel that waits for pacing keeps its items in order in its own queue (`stalled`); the
  other channels are sent. Control messages still go ahead. The outbox bound counts all waiting items.
- *Bounded inbound on the laptop.* The laptop's inbound queue is a bounded `sync_channel`; a reader that falls behind ends
  the channel, as the office does.
- *Test knobs.* `corrupt_next_delta` exists only with the `test-knobs` feature (enabled by `csm-remote`'s dev-dependencies)
  or in this crate's own tests. Production builds cannot set it.

**Not done, and why.**
- The reviewer's "stalled relay makes PeerServer::shutdown return in a bound" is covered one level down
  (`RelayHost`: the close and the drop return at once). The full `PeerServer::shutdown` over a stalled relay is
  not a separate test; `try_bye` still waits for the gate, and the gate frees within the 5 s write timeout.
- The round-robin between channels is by queue order, not by a fair scheduler: a channel at its window does not
  hold others (its writers wait), but a saturated channel still fills the shared outbox bound (512). A test with two
  saturated channels is not written.
- The routing secret (the per-device routing token derived from the public key) is still open, as recorded above.

---

## Uploads: file drops and image paste (P3, `csm-peer-proto`, `csm-peer`, `csm-remote`, `csm-engine`, `csm-ui`)

Status: steps 1 to 3 are built, and the final review's fix round 1 is in (protocol 3). The office side
(`csm-peer` uploads, the `session_drop_paths` tool), the laptop side (`csm-remote/src/upload.rs`, run by
the link's worker) and the UI (the upload card, file drops and image paste in a remote window) are in.
The rows for file drop and image paste in section 6 point here.

Where the build differs from the text below, the build is right and this note is the record:
- **No three-lane writer (the "Laptop writer order" paragraph in section 2 is superseded).** The
  laptop's worker sends one upload frame per turn of its loop, and it drains the window's commands
  (keystrokes included) before each turn. Frames are never reordered on the wire, so a keystroke
  queues behind what is already in flight: the bound on keystroke latency is the in-flight credit, up
  to 2 files x 8 chunks x 32 KiB (512 KiB), not one chunk. The worker never waits on a disk: sizes and
  file reads run on their own threads and feed it through small bounded channels (a slow share delays
  its file only). Frames are never reordered on the wire because the relay carries the Noise
  session as one byte stream: a frame that overtook another would break decryption.
- **The laptop paces its upload chunks on a relay channel.** The relay closes a peer channel that
  goes over its bucket (the peer defaults: 8 MiB/s and 2000 frames/s per channel, with a burst of two
  seconds), and it does not queue what it cannot pass on. A client's `Ready` does not announce those
  caps, so the laptop paces to half the defaults (`CLIENT_BULK_BYTES_PER_SECOND` and
  `CLIENT_BULK_FRAMES_PER_SECOND` in `csm-peer/src/relay_pump.rs`). The other half is the margin for
  control, terminal input and acks, which share the bucket. The pacing sits in the channel's write
  (`RelayStream::paced`), under the link's writer lock: a bulk write waits for its tokens there, and
  every other write only debits them. A keystroke waits behind what is in flight, which is the
  bound above. A 64 MB file takes about 16 seconds over a relay, not 4.5 minutes. The credit window
  (8 unacknowledged chunks per file) bounds what is ever in flight.
- **Local drops also paste through `session_drop_paths`.** Its quoting follows the session's shell
  kind: native for a local session, POSIX for WSL and SSH. For a WSL session the drive path is
  translated to the Linux one (`C:\x` is `/mnt/c/x`, and `\\wsl$\<distro>\x` is `/x`), and a path
  that has no such form (a UNC share, a `..` part) is refused, because the WSL program cannot be
  sure to read it. The tool refuses drops of more than 20 paths and drops that cannot be typed safely.
- **SSH sessions refuse uploads** (`UploadRefused::NotSupported`, the laptop's toast says files cannot
  be uploaded into an SSH session yet). The file would sit on the office, where the remote host cannot
  read it. The paste of a local drop into an SSH session is unchanged.
- **The credit is enforced on the office.** The office stores the bytes it has acknowledged before it
  sends each `UploadAck`, and the reader refuses a chunk that ends more than `UPLOAD_CREDIT` chunks past
  that count with `Overrun`. A compliant laptop never trips it (its own window is the same bound).
- **A folder quota.** The uploads folder holds at most `UPLOAD_FOLDER_QUOTA` (1 GiB) across all
  sessions, finished and partial files together. A begin that would go over it is refused `TooLarge`.
  The count is set from the folder when the office starts, after the cleanup, and it is given back
  when an upload does not keep its file.
- **Partial files do not collide.** Two files of one drop with one name in one millisecond get
  `<stamp>-<name>.part` and `<stamp>-<name>.1.part`, and so on; a partial file is never overwritten.
- **Cancel and a closed link are logged.** An upload that is cancelled, or that ends because the link
  closed (a revoke, the server turned off, a dropped link), writes `upload_cancelled` with reason
  `cancel` or `link_closed`. Nothing is sent back for either. A cancel that arrives after `UploadEnd`
  wins if it arrives before the rename; the rename itself is not interruptible.
- **Forbidden is checked at `UploadBegin` only.** A device that is revoked, or a server turned off,
  mid-upload ends the link, and the link's end deletes the partial files (see section 7). No second
  check is made at `UploadEnd`.
- **The upload itself is not a tool.** It is a laptop action (it reads the laptop's files and
  clipboard), so only the paste is a tool, as the rule for files on the laptop requires.

**Goal.** A remote window (laptop, showing an office's sessions) can drop files onto a pane and paste
an image (Ctrl+V with an image on the laptop's clipboard). The laptop uploads the bytes over the
existing end-to-end encrypted peer link (direct or relay). The office writes them to disk and pastes
the office paths into the session the way a local drop does: bracketed, quoted for the session's
shell, never followed by Enter.

### 1. Messages

Control messages are `Msg` variants (JSON, inside Noise). Bulk bytes are a binary frame kind, so they
never pass through JSON or base64. Every upload has an `id: u64` chosen by the laptop, unique per link.

| Direction | Message | Fields and rules |
|---|---|---|
| C→S | `UploadBegin` (control) | `id`, `session` (an existing session id), `name` (a label only, see 4), `size` (declared bytes), `kind: file \| image`. Refused when `size` is over the limit for the kind. |
| C→S | `UploadChunk` (binary frame, `Frame::UploadChunk`) | `id`, `offset`, `bytes`. `offset` must equal the bytes received so far. At most 32 KiB per chunk (`UPLOAD_CHUNK`, one Noise message). At most 8 unacknowledged chunks per upload (section 2). Sent only after `UploadAccepted`. |
| C→S | `UploadEnd` (control) | `id`, `sha256` (hex of the whole file). The written file is checked against `size` and `sha256`. |
| C→S | `UploadCancel` (control) | `id`. The office deletes the partial file. Also sent by the laptop on the Cancel button. |
| S→C | `UploadAccepted` (control) | `id`. Sent after `UploadBegin` passes the checks and the `.part` file exists. The laptop sends no chunk before it. |
| S→C | `UploadAck` (control) | `id`, `received` (bytes written to the `.part` file so far). Sent after every 4 chunks are written and at `UploadEnd`. This is the only credit for the chunk window (section 2). |
| S→C | `UploadDone` (control) | `id`, `path` (absolute office path of the final file). |
| S→C | `UploadRefused` (control) | `id`, `reason: TooLarge \| TooMany \| Forbidden \| NoSession \| BadName \| NotAnImage \| Overrun \| DiskFull \| Corrupt \| Stalled \| Failed | NotSupported | FolderFull`| NotSupported`. `Overrun` is a chunk beyond the credit (section 2). `Forbidden` covers server mode off, a revoked device and a level below Full. `NotSupported` is an SSH session (see the build notes above). `TooLarge` is a size over the limits; `FolderFull` is the folder quota (the office first removes the oldest finished uploads, then refuses). |

Paste is not part of these messages. After the laptop has `UploadDone` for every file in a drop, it calls
a tool (section 5).

`PROTOCOL_VERSION` in `csm-peer-proto/src/msg.rs` is now 3 (1 before uploads; 2 and 3 during the upload work,
which changed the messages and the credit rules). Client and server must match, so a
mismatched pair refuses at `Hello` as today. The regenerated vectors cover every new message and the new
frame kind, plus a refusal of an older peer. The relay protocol (`csm-remote-proto`) carries frames
opaquely and needs no version change unless its own frame kinds change. The relay's `FrameKind` counters
see the new bytes as ordinary data.

### 2. Frames, chunk size, flow control and fairness

**Frame size.** Every non-control frame is one Noise message (at most `MAX_FRAME_LEN` = 65,535 bytes,
so at most `MAX_PLAINTEXT` = 65,519 bytes of plaintext). Only control JSON is fragmented. So an upload
chunk is one binary frame of at most `UPLOAD_CHUNK` = 32 KiB (`MAX_TERM_CHUNK`, the same as a terminal
chunk). Its plaintext is 1 kind byte + 8 bytes `id` + 8 bytes `offset` + up to 32,768 bytes of data, so
at most 32,785 bytes, and 32,801 bytes on the wire with the 16-byte tag. That leaves room in the
Noise limit. The codec refuses a chunk over `UPLOAD_CHUNK` on encode and decode, as it does for
`term_data`. (A 256 KiB chunk cannot be one frame and is not used.)

**Direction.** Upload data goes laptop to office only. The sender is the laptop's single writer for the
link. Office to laptop carries only small control messages (`UploadAccepted`, `UploadAck`, `UploadDone`,
`UploadRefused`). The existing `WINDOW_FRAMES` / `WINDOW_BYTES` (512 frames / 2 MiB per channel) and the
office's pacing govern the office's writes. They do not govern the laptop's writes, so uploads need
their own credit scheme.

**Credit (per upload).**
- The laptop sends no chunk before `UploadAccepted`. After that it may have at most `UPLOAD_CREDIT` = 8
  chunks (256 KiB) sent but not yet acknowledged for one upload. It never sends the ninth.
- The office sends `UploadAck {id, received}` after every `UPLOAD_ACK_EVERY` = 4 chunks (128 KiB) have
  been written to the `.part` file (the `write` call returned; no fsync per chunk), and once more at
  `UploadEnd`. `received` is the byte count written so far. Reverse traffic is one small message per
  128 KiB. The ack is the only credit; `UploadProgress` is removed.
- The office keeps a bounded queue per upload, with room for `UPLOAD_CREDIT` chunks, between its link
  reader and its disk writer thread. Because the laptop never has more than 8 chunks unacknowledged, the
  reader never waits for the queue, so reading the link never waits for the disk. A chunk beyond the
  credit (a ninth unacknowledged chunk) is a protocol violation: the office refuses with `Overrun`, deletes
  the partial file and keeps the link up. The office checks this on each chunk against the count it has
  acknowledged (the build notes above), so it does not depend on how fast the disk is.
- Memory on the office is bounded: at most 8 active uploads (section 3) times 8 chunks times 32 KiB,
  which is 2 MiB of queued chunks in the worst case.

**Laptop writer order (superseded: the build has no lanes, see the build notes above).** The original design had three lanes, drained in order:
1. Control messages and `TermInput` (keys, paste, mouse). These are never queued behind a chunk.
2. Other terminal frames and the rest of the protocol traffic.
3. Upload chunks.

The writer takes one chunk from lane 3 only when lanes 1 and 2 are empty. A write that has started
finishes first, so a keystroke waits for at most one 32.8 KiB frame. On the direct link that is one TCP
write, which is about 26 ms at 10 Mbit/s. Active uploads are served round robin, one chunk each per turn,
so a multi-file drop does not finish one file before starting the next.

**Relay pacing (as built).** The relay enforces a per-channel, per-direction token bucket on each client
frame (`ChannelLimiter`): the peer defaults are 8 MiB/s and 2000 frames/s, with a two-second burst. A
frame over the bucket closes the channel `RateLimited`, and so does a frame the relay cannot pass on
(its host queue is full). It does not queue. The relay's `Welcome` carries the caps to the office
only, so the laptop uses the defaults with margin: half of each (the build notes above). Only upload
chunks wait for tokens; control and terminal input always go, and they debit the same bucket. The
wait is in the channel's write, not in a separate queue, because the Noise messages must keep the
order they were encrypted in. The test `a_big_relayed_upload_keeps_the_channel_and_the_terminal_stream`
(`csm-peer/tests/peer_relay.rs`, run with `CSM_RELAY_BIN`) sends 25 MiB through the real relay while
the terminal streams, and checks the channel stays open and the output keeps flowing.

**Concurrency.** At most 2 uploads are active per device on the laptop, and the rest wait in the laptop's
queue. The office refuses a ninth active upload across all devices (`TooMany`, with a total cap of 8).

**Fairness with terminal output.** Upload data never travels office to laptop, so terminal output is
never queued behind an upload chunk. The office's acks are small control messages on the control lane.

**Stall.** The laptop starts a 30 s timer (`UPLOAD_STALL`) while it has chunks unacknowledged. It resets
the timer on every `UploadAck`. On expiry it sends `UploadCancel`, shows "upload stalled", and the
upload is over. The office applies the same 30 s rule from the other side: no chunk for an open upload
in 30 s ends it with `Stalled` and deletes the partial file. Neither rule drops the link.

### 3. Limits (configurable constants in `csm-peer-proto/src/limits.rs`)

| Limit | Value | Enforced by |
|---|---|---|
| File size | 64 MiB | laptop before reading; office at `UploadBegin` and while writing |
| Image size | 32 MiB | same (images are a kind of their own) |
| Drop total | 256 MiB | laptop before the first `UploadBegin` |
| Files per drop | 20 | laptop before the first `UploadBegin` |
| Chunk | 32 KiB (`UPLOAD_CHUNK` = `MAX_TERM_CHUNK`) | codec, on encode and on decode; a larger chunk is refused |
| Unacknowledged chunks per upload | 8 (`UPLOAD_CREDIT`, 256 KiB) | laptop never sends a ninth; office refuses `Overrun` |
| Ack interval | every 4 chunks (`UPLOAD_ACK_EVERY`, 128 KiB), and at `UploadEnd` | office |
| Stall | 30 s (`UPLOAD_STALL`) with chunks outstanding (laptop) or no chunk (office) | both ends |
| Active uploads | 2 per device (laptop); 8 in total (`UPLOAD_ACTIVE_TOTAL`) | laptop queue; office refuses `TooMany` |
| Queued bytes on the office | 8 uploads x 8 chunks x 32 KiB = 2 MiB at most | office per-upload queues |
| Uploads folder | 1 GiB in all (`UPLOAD_FOLDER_QUOTA`), finished and partial files together; over it, `TooLarge` | office, reserved at begin |


### 4. Storage, names and sanitising

- **Layout:** `<data>/peer/uploads/<session-id>/<unix-ms>-<sanitised-name>`, for example
  `.../uploads/<session-id>/1795800000123-report.pdf`. Never inside a project folder.
- `<session-id>` is used as a folder name only after it matches `[A-Za-z0-9_-]{1,64}` and names an
  existing session. Anything else is `NoSession`.
- **Partial files:** the office writes `<stamp>-<name>.part` (or `<stamp>-<name>.n.part` when that name is
  taken, see the build notes above) and renames it to the final name only after the
  `UploadEnd` checks pass. A rename never overwrites: if the name exists, a suffix ` (n)` is added.
- **Directory:** created with the owner-only permissions the peer identity files use (`fsperm.rs`).
- **Sanitising the name** (the laptop's name is a label, never trusted):
  1. Keep only the last component after splitting on both `\` and `/`.
  2. Reject `.` and `..`; an empty result becomes `file`. (An image's name is always `clipboard`, see 7, so an empty image name never shows.)
  3. Drop control characters and invisible format characters (the same rule as `csm_core::dropped_paths`).
  4. Replace `< > : " | ? *` with `_`. Trim trailing dots and spaces.
  5. A reserved Windows name (`CON PRN AUX NUL COM1-9 LPT1-9`, with or without an extension) gets a `_` prefix.
  6. Cap at 120 bytes of UTF-8, cut on a character boundary and keep the extension.
  7. For kind image the name is forced to `clipboard.<ext>`, where `<ext>` comes from the sniffed type.
- **Image check:** the first chunk of an image upload must start with the magic bytes of PNG, JPEG, GIF,
  WebP or BMP. Otherwise `NotAnImage`.
- **Integrity:** the office hashes while writing and compares with `UploadEnd.sha256`. On mismatch it
  deletes the file and refuses with `Corrupt`.
- **Reparse points:** the office never follows a symlink or junction under the uploads root.

### 5. Paste after the drop

- The laptop waits until **every** file in a drop is `UploadDone`. If one file fails, it pastes nothing
  from that drop, shows which file failed and why, and leaves the finished files on the office for the
  7-day cleanup. (All-or-nothing avoids a prompt with half the paths.)
- Then it calls a **new office tool** `session_drop_paths {session_id, paths[]}` in `csm-engine`. The tool
  builds the payload with `csm_core::dropped_paths::build_dropped_paths_payload` using the **office's**
  quoting for that session's shell kind (native, WSL or SSH), and sends it with `session_send_text`,
  `force: true`, as a bracketed paste, never followed by Enter. Quoting happens on the machine that owns
  the shell, never on the laptop.
- The local drop (`screens/file_drop.rs`) moves to the same tool, so both paths share one code path.
  The tool's declared effect is "types text into the session".
- The window ends with `FocusTerminal` for the session, as today.

### 6. Image paste flow

- Today the terminal view sends a raw `^V` on Ctrl+V with an image (`terminal/view.rs:506-511`), so Claude
  reads the **office** clipboard. In a remote window that is wrong, so the rule changes: **a remote window
  never sends raw `^V`.**
- On Ctrl+V in a remote window the clipboard is read on a background thread, never in a frame.
  - If the clipboard has an image: encode it as PNG (unless it is already PNG), reject it above 32 MiB,
    and upload it as one file of kind `image`. When it is done, call `session_drop_paths` with that path.
  - If it has text: paste the text, which the office brackets (`term_input {kind: paste}`), since it is untrusted.
  - If it has nothing usable: a toast ("nothing to paste").
- Local windows keep `^V` unchanged.

### 7. Refusals, failure cases and restart

- **Refused unless:** server mode is on, the device is not revoked, and the device's level is Full (an
  upload counts as typing into the session). Otherwise `Forbidden`, checked at `UploadBegin` only. A
  revoke or a server turned off mid-upload closes the link, and that ends the upload (see the build
  notes above: there is no recheck at `UploadEnd`).
- **Link drop mid-upload:** restart, not resume. The office deletes the `.part` file when the link closes.
  The laptop shows "upload failed: the link dropped", and the user drops again. A paste is not idempotent,
  so nothing retries it automatically. (Resume would need an offset handshake and a partial-file index, for
  a case that is rare on a LAN or a relay.)
- **Cancel:** the laptop sends `UploadCancel`; the office deletes the partial file and sends no reply.
  Cancelling a drop cancels every upload in it.
- **Disk full or write error:** `DiskFull` or `Failed`; the partial file is deleted; the link stays up.
- **Office quits mid-upload:** the `.part` files are removed by the cleanup on the next start.
- **Revoke or server mode off mid-upload:** the office sends `Bye`, as for terminals, and deletes the partial files.
- Nothing in these paths panics: all input is untrusted (section 4 and CLAUDE.md, "never crash on input").

### 8. Cleanup

- On office start, on a background thread (never before the UI shows), delete every entry under
  `<data>/peer/uploads/` older than **7 days** by modification time. `.part` files older than 1 day go too.
- Only canonical children of the uploads root are touched. Reparse points are skipped, not followed.
  Nothing outside the uploads tree is touched.
- Cleanup errors are logged to `remote-access.log` as a count and never stop the server.

### 9. Logging and audit

- `remote-access.log` gets one line when an upload starts, and one when it finishes, is refused or is cancelled:
  `upload begin session=<id> device=<id> name="<sanitised>" size=<bytes> kind=<file|image>`,
  `upload done session=<id> device=<id> size=<bytes> path=<path>`, and
  `upload refused session=<id> device=<id> reason=<reason>`. A cancel, or an upload that ends because
  the link closed, is `upload_cancelled session=<id> device=<id> reason=<cancel|link_closed>`. The build
  writes these as JSON events (`upload_begin`, `upload_done`, `upload_refused`, `upload_cancelled`).
- Never the contents, never the hash, never the laptop's own path.
- The paste itself goes through the existing tool audit, as `session_drop_paths`.

### 10. Build steps

1. **Protocol and office side (`csm-peer-proto`, `csm-peer`, `csm-engine`).** The new messages and frame
   kind, `PROTOCOL_VERSION` 3, regenerated vectors. A new `csm-peer` upload store: limits, sanitising,
   `.part` files, hashing, `UploadAck` and the credit, stall, cleanup and the log lines. The `session_drop_paths` tool
   with its declared effect. Tests with the in-process peer harness (`csm-peer/tests`): upload round trip;
   each refusal; a corrupt hash; a ninth unacknowledged chunk refused `Overrun`; a chunk over `UPLOAD_CHUNK` refused by the codec; a name with `..` or a reserved name; a link drop mid-upload that leaves no
   `.part`; the stall; a terminal stream still getting its frames during a large upload; cleanup that does
   not touch files outside the root.
2. **Laptop side (`csm-remote`, `csm-ui`, `apps/codeshell`).** The upload client (background reads, chunking,
   credit and acks, the three-lane writer), the progress toast with cancel, a queue of drops, the drop path in
   `screens/file_drop.rs` for remote windows, and moving the local drop to `session_drop_paths`. Tests: the writer lane order with a keystroke during a chunk stream, a
   multi-file drop through the relay test rig, cancel, folder refusal, and the all-or-nothing paste.
3. **Image paste (`csm-ui`, `csm-remote`).** The remote Ctrl+V branch (no raw `^V`), the clipboard read and PNG
   encode on a background thread, the 32 MiB check, and the image magic check on the office. Tests: an image
   paste produces a paste of the office path; a clipboard with no image still pastes text; a local window
   still sends `^V`.

Each step leaves `cargo test --workspace` and clippy green. The P3 rows in section 7 are updated when step 3 lands.
