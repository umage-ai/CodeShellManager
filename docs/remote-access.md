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
