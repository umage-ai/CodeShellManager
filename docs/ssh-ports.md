# Links and ports on SSH sessions

An SSH session runs on another machine. A link in its terminal to `http://localhost:3000`,
`http://127.0.0.1:3000`, `http://[::1]:3000` or `http://<name>.localhost:3000` points at that
machine's own port 3000, not at this computer's. CodeShellManager does not open such a link in
your browser by accident.

## What happens when you Ctrl+click a loopback link

Click a link in an SSH session's terminal (Cmd+click on macOS).

1. CodeShellManager reads the session's ssh configuration (`ssh -G`, with the session's `-F`, `-p`,
   `-i`, `-J`, `-o`, `-l` and destination; no connection is made). Off the UI thread.
2. If that configuration forwards the port (a `LocalForward` to `localhost` or `127.0.0.1` on the
   remote side, as the NEXUS config does for agent web ports), the link opens on this computer at
   the forwarded port. Nothing is started.
3. Otherwise nothing opens. A card says **"Port 3000 is on <destination>, not on this computer"**
   with two answers:
   - **Forward and open** starts a forward (below) and opens it.
   - **Copy link** copies the link, for example to open it in a browser on the remote host.

Links to other hosts, links in a local or WSL session, and links from a run command open as before.

## Port forwards

A forward is an `ssh -N -L` process that exists only while the session runs. It listens on
`127.0.0.1` on this computer only. It is stopped when its session closes, sleeps or restarts, and
when CodeShellManager quits. Forwards are never saved: after a restart you start them again.

The local port is the remote port when it is free on this computer; otherwise a free port is picked
and the card or the tool tells you which.

The Session tab of an SSH session has a **Ports** list: each address opens on click, and **Stop**
ends the forward.

Tools (see [tools.md](tools.md)):

| Tool | What it does |
|---|---|
| `session_port_forward` | Starts a forward: `session_id`, `remote_port`, optional `local_port`, `open` |
| `session_port_forwards` | Lists the running forwards, optionally for one session |
| `session_port_unforward` | Stops one forward (`local_port`) or all of a session's (`all: true`) |

Starting a forward is a process launch and a network use, so a tool call from an MCP client asks you
first.

## Limits

- Only SSH sessions have a remote host to forward from. The forward uses the session's ssh
  configuration, so anything ssh cannot do without a prompt (a passphrase, a host key you have not
  accepted) fails: the forward is refused and the reason is in the error.
- Only `http` and `https` links are rewritten. The forward carries the whole port, so the link's
  path and query are kept, but the card's **Forward and open** opens the forwarded root address.
- The check reads `LocalForward` lines from `ssh -G`. A forward that only exists inside a running
  `ssh` you started by hand is not seen; start it with `session_port_forward` instead.
