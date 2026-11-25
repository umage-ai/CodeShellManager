# Remote access: a laptop window onto an office CodeShellManager

One CodeShellManager (the **office**, always on at work) can show its sessions on another machine
(the **laptop**, at home). The laptop shows the office's sessions live, types into them and answers
their consent cards. The office does the work: its sessions, files, git and add-ons stay on the
office, and the laptop only shows the screen and sends what you type.

This works over a private network such as WireGuard or Tailscale. The laptop must be able to reach
the office at the office's address on that network.

> **Off by default.** Nothing on the office listens, and nothing on the laptop dials out, until you
> turn it on in the app. No agent, MCP client, `csm` command or paired device can turn it on or pair
> a new device; only the person at the app can (design: [design/remote-csm.md](design/remote-csm.md),
> section 4).

## Set up the office

1. In the office's CodeShellManager, open **Settings › Remote access**.
2. Switch on **Let another CodeShellManager connect to this one**. A walk-through starts.
3. **Address.** Pick the address the laptop will use. A WireGuard adapter is marked and selected when
   there is one. Keep the port (47300) unless something else uses it. Do not pick "every interface"
   unless you need it: it is under **Advanced** with a warning.
4. **Firewall.** The app shows the exact Windows rule for the port, scoped to the adapter's network,
   as text with a copy button. CodeShellManager does not add the rule itself; add it from an
   administrator prompt if Windows blocks the connection.
5. **Turn on and pair a device.** A pairing link (`csm://pair#…`), its QR code and a timer of ten
   minutes appear. The link is a secret: send it only to your own laptop.

The office's title bar shows **Remote access on · address** while it is on, and **Laptop connected**
(or the number of connected devices) when a laptop is connected. Click it to open the page, where
**Turn off remote access** closes the port, disconnects every laptop and clears any pending pairing.

Paired devices are listed on the same page. **Revoke** removes one device; **Revoke all** removes them
all, and the next connection needs a fresh pairing. Every enable, pairing, connection and revocation
is written to `remote-access.log` in the office's `peer` folder, beside the state file.

Keep the office awake. When the office sleeps, the link drops and the laptop shows the office as
offline until it wakes.

## Pair the laptop

1. In the laptop's CodeShellManager, open the command palette and choose **Connect to another
   CodeShellManager…**.
2. Paste the link from the office (or its text). Give the office a name if you like; it defaults to
   the address.
3. Both screens show the same four-digit **check code**. Compare them, choose what the device may do
   (**Full**, **Operator** or **Viewer**; see *Levels* below) and click **Allow** on the office.
   (**Deny** on the office refuses the pairing; a wrong code is refused too.)
4. The laptop lists the office under **Paired offices**. **Connect** opens a remote window for it;
   **Forget** removes it from this laptop, with the office's pinned key. This laptop's own device key stays,
   because other offices may know it.

Pairing is started only from the app's own windows, on either side. It is not possible from the
command line, from an agent or from another paired device.

## Trusted and untrusted devices

Each paired device is either **trusted** or not. This is the office's decision, kept in the
office's own device list, and a device cannot change it for itself (`peer_device_trust` is the
app's own window only).

- **Trusted** devices act without prompts: typing into a session, starting a session or a program,
  and everything else the Full level allows run at once. Use it for a laptop you sit at.
- **Untrusted** devices still read, browse and use the remote window, but a call that types into a
  session (`session_send_text`...) or starts a process (`session_new`, run commands...) waits for
  you: a card on the **office's** screen says "Paired device *name* wants to ...", and nothing runs
  until you allow it on the office. An untrusted device cannot see or answer cards, and "always allow" does not
  exist for it.
- At pairing, the **Trust this device (no prompts)** box next to **Allow** is off unless you tick
  it. A device paired before this existed was migrated as trusted, so it keeps working. Pairing the
  same device again resets it to what you tick then. The **Trusted** box beside each paired device
  changes it later, at once, even while it is connected.
- The keystrokes of a remote window or `csm attach` are not tool calls (they go straight to the
  session's terminal stream, like typing at the office) and never ask. For an untrusted device that
  is the one input path left open; the cards guard the tools an agent would use to drive the office.
- Anyone who can use the laptop's user account can use its device key (`peer-client/device.key` is a
  file readable by that account) and `csm attach`, so a trusted laptop is exactly as safe as its user
  account. Trust only a machine you would hand the office's keyboard to.

## Levels

When you allow a pairing you choose the device's level, and the device is told it in the answer:

- **Full** (the default): everything the office's own window can do; an untrusted device still needs a
  card for typing and launching (see below). Uploads and consent cards are Full only.
- **Operator**: watches sessions and types into a session's terminal and resizes it; tool calls that
  change anything are refused.
- **Viewer**: only watches; it cannot type, resize or change anything.

The level is the office's decision, kept in its device list. Pair the device again to change it.

## Using a remote window

- The window's title starts with the office's name, and a strip under the title bar says
  **Connected to OFFICE · address**. It also shows **Connecting to…**, **OFFICE offline · reconnecting**
  (the link retries after 1, 2, 5, 10 and then every 30 seconds; a silent close during the handshake, which is also what a Wi-Fi flap or a restarting office looks like, is retried five times before the strip says **refused**) or **OFFICE refused this laptop**
  with the reason.
- Layout, focus, the rail, the inspector, the theme and grids belong to the laptop: they are kept
  per office in the laptop's `peer-client` folder. Sessions, groups, notes, tasks, runs and the inbox
  are the office's.
- **Consent cards** raised by agents on the office show on the laptop as well. The first answer
  (on either machine) wins, and the card closes everywhere.
- **Quit** in a remote window disconnects. It never quits the office. To quit the office, use
  **Quit CodeShellManager on OFFICE…**; it asks first.
- Closing a remote window (its ✕ or Alt+F4) disconnects the same way. A window that has lost its host shows its last
  picture greyed out, with the strip saying it is reconnecting.
- Some things stay on the office or are refused in a remote window, because they would act on the
  laptop's machine: opening a folder or a file in an editor, the native folder picker and backup
  export and import. Files and images are not refused: they are uploaded (see *Uploads* below).
- **Links.** Ctrl+click opens a web link in the laptop's browser after the same http/https check. A
  `localhost` or `127.0.0.1` link warns first: it points to the office, not to the laptop.

## Attach from a console: `csm attach`

`csm attach` shows one office session in any terminal, the way tmux attaches. It is a harness for
the link and a quick way in from a plain console. It uses the laptop's paired offices and never pairs.

```text
csm attach --offices                 list the paired offices
csm attach --list                    list the office's sessions (id, status, name)
csm attach "Build"                   attach to a session by name (or id)
csm attach --host OFFICE "Build"     when several offices are paired
```

- The console shows the session's screen and types into it. The console's size becomes the session's
  size, so resizing the window resizes the session.
- **Ctrl+] then d detaches.** The session keeps running on the office. Ctrl+] Ctrl+] types one Ctrl+].
- Ctrl+C and the other keys go to the session, as in a local terminal.
- `--config-dir DIR` names the laptop's CodeShellManager config folder when it is not the usual one
  (`CSM_STATE_PATH` works too); the paired offices are read from its `peer-client` folder.
- Exit codes: 0 when you detach or the session ends, 2 for a usage problem, 3 when the office cannot
  be reached or the link drops.

**Trust.** `csm attach` and the remote window carry the trust of this laptop's device key. Anything
that can run `csm` on this laptop, or open this laptop's CodeShellManager, can type into the office's
sessions, with the same level this laptop has there. The office cannot tell such a
program from you at the keyboard. Only pair laptops you trust, and forget an office you no longer use.

## Troubleshooting

| What you see | What to check |
|---|---|
| The laptop says it cannot connect and the office shows nothing | The office is on (Settings › Remote access), the laptop reaches the office address (`ping` it over the VPN), and the firewall rule exists for the port. |
| "The office does not accept this device" | The device was revoked, or the office forgot the pairing. Pair the laptop again. |
| "The two CodeShellManager versions…" | Update both to the same version. The protocol and the terminal emulator must match. |
| "This office is not paired with this laptop any more" | Forget and pair again. |
| The office's address is missing (an adapter is down) | The office keeps retrying in the background and listens nowhere else. Bring the adapter up. |

## Uploads

- **Dropping files** onto a pane of a remote window uploads them to the office. **Pasting an image**
  (Ctrl+V with an image on the laptop's clipboard) uploads the image as `clipboard.png`. When every
  file is on the office, its path is pasted into the session, quoted for that session's shell (native
  for a local session, POSIX for WSL), as a local drop is: a bracketed paste, never followed by Enter.
  Claude Code reads the image from that path. In a WSL session the path is the Linux one
  (`/mnt/c/...`), so the program inside WSL can open it.
- **SSH sessions cannot take uploads yet.** The file would sit on the office, where the remote host
  cannot read it, so the laptop says so and nothing is sent.
- A progress card shows each drop. **Cancel** stops it: the office deletes the partial file and nothing
  is pasted. If one file fails, nothing from that drop is pasted.
- **Folders are refused** with a message: drop the files in them, or copy the folder to the office.
- **Limits:** 64 MB a file, 32 MB an image, 256 MB and 20 files a drop. The office refuses an upload
  when remote access is off, when the device has been removed, or when the device may not type into
  sessions. Several uploads may be running at once: at most two from each laptop. The uploads folder
  holds at most 1 GB in all. When an upload would go past that, the office first removes the oldest
  finished uploads (only inside the uploads folder) to make room; if that is not enough, the upload is
  refused with "The office's uploads folder is full; files older than 7 days are removed
  automatically". An upload larger than the limits above is refused as too large.
- **Where the files go:** `<office data folder>\peer\uploads\<session id>\<time in ms>-<name>`, for
  example `...\peer\uploads\<session id>\1795800000123-report.pdf`. Never in a project folder. The name
  is the laptop's, cleaned up: no folders, no `..`, no reserved Windows names, at most 120 bytes.
  A partial file is written as `<name>.part` and renamed only when the whole file has arrived and its
  checksum matches.
- **Cleanup:** when the office starts and then every hour while it runs, uploads older than 7 days are
  deleted (partial files older than a day), and their space counts as free again. Nothing outside the
  uploads folder is touched.
- **Logging:** `remote-access.log` records an upload's start, end, refusal or cancel, with the file's
  name, size, session and device. An upload that ends because the link closed is logged as cancelled
  with the reason `link_closed`. It never records the contents.
- A link that drops during an upload ends it: nothing is pasted, and the laptop says so. Drop the
  files again once the link is back.

## Limits (for now)

- A remote session's links and run URLs open in the laptop's browser; port forwarding is planned.
- There is one office per window; several offices are shown as several windows.
- At the Full level a paired laptop can do everything the office's own window can do (an untrusted one asks first for typing and launching; see Trusted and untrusted devices). See *Levels* for Operator and Viewer.
- Anything that can run `csm` on this laptop can type into the offices it is paired with (see the trust
  note under `csm attach`).

## Resuming, connections and limits

- **Resuming.** When the office's app starts with remote access saved as on, it comes back by itself
  and says so: "Remote access resumed: N devices, listening on ...". Look at the device count.
- **Every interface.** A choice saved as "every interface" is honoured on a restart only when its
  keyed hash (the key is in the OS keychain) shows this app wrote it. If it does not (the file was
  edited, or there is no keychain), the office listens on this PC only (loopback) and **Remote access**
  shows a button to listen on every interface again; one click confirms it, and the app then saves it
  signed. A direct connection from another machine does not work in between; a relay still does.
- **Connections per device.** One device may hold up to four connections at once (a reconnect while
  the old one is still timing out, a second window). A fifth replaces the oldest.
- **One slow device.** Every connection has its own bounded send queue and writer. A device that
  stops reading is closed when its queue fills; it never holds up the others or the thread that reads
  from it. At most eight consent answers are handled at once, all devices together.
- **Strangers.** Connections that have not finished the handshake are kept in two pools (direct and
  relay), at most four from one address; when a pool is full the oldest is closed to make room, and the
  first frame must arrive within 3 seconds. A pairing offer is not used up by failed handshakes from
  someone who saw its id (it is closed after 30), only by the device that proves the offer's key.
- **The log.** `remote-access.log` also records reads of session content (`read`) and terminal
  subscriptions (`term_subscribe`). Identical entries within 30 seconds (a scanner's refusals, a screen
  that polls) are written once and followed by a `..._repeated` line with the count. Five older files
  (`remote-access.log.1` to `.5`) are kept.
- **Key files.** The keys are created only when the `peer` folder has been limited to your account
  (Windows: an ACL for your account's SID, read from the process token with `whoami /user`, not from
  environment variables). If that cannot be done, no key is written and the app says why;
  `private_files_restricted` in the status says whether an existing folder is limited.
- **Names.** A host paired without a name is called by its address; if that is too long for a name, a
  shortened form is used.

## Through a relay (no VPN)

A direct address needs the laptop to reach the office: the same LAN, or a VPN such as WireGuard. A
relay removes that need. Both machines connect **outbound** to a relay (`csm-relay`, in the private `umage-ai/csm-relay` repository, self-hosted from there), which
copies the encrypted frames between them. The relay never holds a key that matters and cannot read a
frame; the session is the same end-to-end session as on the direct address.

- **Office:** Settings › Remote access › "Also reachable through a relay" gives the relay address
  (`wss://…`; `ws://` only to this PC) and, when the relay asks for one, the enrolment token (a
  password field, cleared as soon as it is sent). The token and the office's relay secret are kept in
  the OS keychain, never in a file. The address goes to `peer/relay.json` in the app's data folder.
  The card shows whether the relay is connected, and "Change…" and "Turn off the relay" change it.
  Leaving the token empty keeps the saved one for the same address; a different address does not get
  the saved token, so it is removed and must be typed again. The pairing link then names the relay too:
  an open pairing link is made again when the relay changes, but a link already copied keeps the
  route it was made with. The relay runs only while remote
  access is on: while it is off the card says the relay is not running, and turning it off stops it.
- **Laptop:** pairing and connecting try the direct address first, then the relay. The saved office
  (`hosts.json`) keeps the relay route, so a later connection needs no setting. A laptop paired before
  the office had a relay keeps working directly; pair again to add the route.
- **Routing token.** Each pairing gets its own random routing token (32 random bytes made by the office
  and sent to the laptop in the pairing answer; the relay only ever holds its hash), so a token cannot be
  worked out from a device's public key. A pairing made before this keeps the token derived from the
  device key until you pair that laptop again, so an existing laptop keeps working; the same fallback
  applies between an older and a newer build (the new fields are optional and ignored by the older one).
- **Enrolment token.** The saved enrolment token belongs to the address it was saved with and is dropped
  if `relay.json` names another address. A pairing link that is open when you set up or change the relay
  is sent to the new relay (with the time it has left), and an expired link is never sent again.
- **Trust:** the relay sees who connects, when, and how much, never what is said. Revoking a device
  takes its relay access away at once, as well as its keys.
- **Limits:** a relay channel carries terminal streams, so its per-channel caps are higher than a
  phone's (configurable on the relay: `[limits] peer_*`). A relay run by someone else is that
  operator's to secure; the `Origin` check keeps browsers out of the peer endpoints.
- **Bursts:** the office paces its writes to the per-channel caps the relay announces when it connects
  (2000 frames and 8 MiB a second by default), so a large burst such as `cat` of a big file waits at the
  office instead of tripping the relay's limit and ending the session. A relay configured with lower
  peer caps is followed, not refused: the office reads the caps from the relay's welcome.
- **Slow laptops:** the laptop acknowledges what its reader has taken, and the office keeps at most
  512 frames and 2 MiB in flight per terminal stream. A laptop on a slow link therefore slows the
  office's output, as a slow direct connection does, and a long burst catches up with a fresh screen
  rather than ending the link. A well-behaved office never fills the relay's queue for a channel.
- **Stalled relays:** a relay that stops reading ends the office's connection after 5 s of blocked
  writes, and the office reconnects with backoff. Turning remote access off or quitting the app waits
  at most about 5 s for a stalled relay.
- **Addresses:** a relay address is refused if it has userinfo (`user@`), a backslash, a fragment or
  whitespace. `ws://` is accepted only to `127.0.0.1`, `localhost` or `[::1]`; any other host needs `wss://`.
  The same check guards the address in a pairing link and in `hosts.json`.
- **Timeouts:** the connect to the relay gives up after 10 s per address, and the WebSocket handshake
  (TLS included) after 10 s, so an unreachable relay does not stall the app.
