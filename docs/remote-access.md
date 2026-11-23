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
3. Both screens show the same four-digit **check code**. Compare them, then click **Allow** on the
   office. (**Deny** on the office refuses the pairing; a wrong code is refused too.)
4. The laptop lists the office under **Paired offices**. **Connect** opens a remote window for it;
   **Forget** removes it from this laptop, with the office's pinned key. This laptop's own device key stays,
   because other offices may know it.

Pairing is started only from the app's own windows, on either side. It is not possible from the
command line, from an agent or from another paired device.

## Using a remote window

- The window's title starts with the office's name, and a strip under the title bar says
  **Connected to OFFICE · address**. It also shows **Connecting to…**, **OFFICE offline · reconnecting**
  (the link retries after 1, 2, 5, 10 and then every 30 seconds) or **OFFICE refused this laptop**
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
  laptop's machine: opening a folder or a file in an editor, the native folder picker, backup export
  and import, dropping files onto a pane (you get a toast) and pasting an image (you get a toast).
  Paste text instead, or copy the file to the office first.
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
sessions, with the same level this laptop has there (today: Full). The office cannot tell such a
program from you at the keyboard. Only pair laptops you trust, and forget an office you no longer use.

## Troubleshooting

| What you see | What to check |
|---|---|
| The laptop says it cannot connect and the office shows nothing | The office is on (Settings › Remote access), the laptop reaches the office address (`ping` it over the VPN), and the firewall rule exists for the port. |
| "The office does not accept this device" | The device was revoked, or the office forgot the pairing. Pair the laptop again. |
| "The two CodeShellManager versions…" | Update both to the same version. The protocol and the terminal emulator must match. |
| "This office is not paired with this laptop any more" | Forget and pair again. |
| The office's address is missing (an adapter is down) | The office keeps retrying in the background and listens nowhere else. Bring the adapter up. |

## Limits (for now)

- Pasting an image into a remote session is refused, and file drops are refused. Uploads are planned.
- A remote session's links and run URLs open in the laptop's browser; port forwarding is planned.
- There is one office per window; several offices are shown as several windows.
- Only the "Full" level exists: a paired laptop can do everything the office's own window can do.
- Anything that can run `csm` on this laptop can type into the offices it is paired with (see the trust
  note under `csm attach`).

## Through a relay (no VPN)

A direct address needs the laptop to reach the office: the same LAN, or a VPN such as WireGuard. A
relay removes that need. Both machines connect **outbound** to a relay (`relay/`, `csm-relay`), which
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
