# Security Policy

CodeShellManager is a desktop application that hosts terminal CLIs (Claude Code, Codex, shells,
ssh…) in local pseudo-terminals, indexes their output to a local SQLite database, and keeps its
state in the user's app-data folder (`%AppData%\CodeShellManager2` and
`%LocalAppData%\CodeShellManager2` on Windows). It has no account and sends no telemetry.

## What uses the network

- **Local control channel (always).** The `csm` command line and the `csm mcp` server talk to the
  running app over a loopback connection, authenticated with a token written per launch to the
  user's runtime folder. By default nothing listens on other interfaces.
- **Remote access (off until the person turns it on).** Only the person at the app can turn it on
  or pair a device; no agent, MCP client, `csm` command or paired device can. When it is on, the
  app listens on the port and address the person chose (port 47300 by default, one address, or
  every interface only as an explicit choice). A saved "every interface" choice is honoured after a
  restart only when it carries the app's own signature; otherwise the app listens on loopback
  until the person confirms it again. Connections use Noise with per-device keys after a pairing
  with a check code, and each paired device has a level (Full, Operator, Viewer) and a trusted flag
  that only the office's own window can change. An untrusted device needs the person's OK on the
  office for anything that types into a session or starts a process. Every enable, pairing,
  connection and revocation is logged. With a relay configured, the app also connects outbound to
  that relay, which carries only encrypted frames. See `docs/remote-access.md`.
- **Update check (on by default).** Once a day the app makes one HTTPS request to GitHub's API to
  ask whether a newer release exists. Nothing is downloaded and the only identifying header is a
  generic `CodeShellManager/<version>` user agent. Turn it off in Settings → General → Updates.
- **What the person starts.** Git fetch, ssh and the programs run in sessions go where the person
  points them.

## The security model in brief

- **Consent for MCP clients.** Every action is a tool with declared effects. When an MCP client
  calls a tool that sends input to a session, reads session content, starts a process, writes
  files or may discard data, the call waits until the person answers a consent card in the app
  (*Allow once*, *Always for this client*, *Don't allow*); unanswered requests time out after
  120 seconds. Grants are revocable in Settings → MCP. The tool that answers consent is refused for
  every caller but the app's own UI, so a client can never approve itself.
- **Extra rules regardless of grants.** An agent cannot send input to its own session; input into
  a session that is mid-turn is refused unless the caller passes `force`; some tools (changing the
  Open with list, importing a setup, accepting tasks) are refused for MCP clients outright.
- **Untrusted input.** PTY output, OSC payloads, state files, imports, git output and add-on
  manifests are treated as hostile: validated, capped, and never allowed to crash the app.
  Untrusted text sent into a terminal is pasted with bracketed-paste markers, never typed raw.
- **No shell interpolation.** Commands are built as argv lists and executed directly; a shell is
  used only when the user's own command is the payload. "Open with" programs are resolved and
  checked before they are started.
- **Native code.** `unsafe` Rust is confined to the Windows ConPTY binding. `conpty.dll` is loaded
  by full path from beside the executable (Microsoft-signed), never by bare name.
- **Add-ons** run nothing until the person enables them after seeing what they add; a process
  add-on's tools go through the same consent rules.

## Supported versions

Only the latest published release is supported. See the
[Releases](https://github.com/umage-ai/CodeShellManager/releases) page.

| Version | Supported |
| --- | --- |
| Latest release | Yes |
| Older releases | No |

If you're on an older version, please reproduce on the latest release before reporting.

## Reporting a vulnerability

**Please do not file a public issue for security reports.** Use one of the following private
channels:

1. **GitHub Security Advisories** (preferred):
   https://github.com/umage-ai/CodeShellManager/security/advisories/new
2. **Email**: thraen@gmail.com

Include:

- A clear description of the issue and its impact
- Steps to reproduce (or a proof of concept)
- The CodeShellManager version (Settings → About, or the installer file name)
- Your OS and version
- Whether you'd like credit in the advisory

We aim to acknowledge reports within 5 business days and to ship a fix or mitigation within 30
days for high-severity issues. We will coordinate disclosure with you and credit you in the
advisory unless you prefer otherwise.

Of particular interest: a way for an MCP client or add-on to act without the consent card the
rules above require, a way for terminal output or a file on disk to make the app run a command or
crash, and anything that reaches the local control channel without its token.

## Out of scope

- A user with local access to the machine reading their own state files or search index.
- Behaviour of the third-party CLIs hosted in a session (Claude Code, Codex, ssh, etc.).
- What a person allows on a consent card ("Always for this client" means what it says).
- Self-XSS or social-engineering scenarios where the user is tricked into pasting
  attacker-controlled content into a terminal.
- Vulnerabilities in upstream components (Windows ConPTY, GPU drivers, Rust crates): please report
  those upstream; tell us too if CodeShellManager needs to update.
