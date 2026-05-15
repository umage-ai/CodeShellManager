# Security Policy

CodeShellManager is a Windows desktop application that hosts terminal CLIs locally via ConPTY and WebView2, indexes terminal output to a local SQLite database, and persists session state to `%AppData%\CodeShellManager\state.json`. The attack surface is primarily local — there is no network listener and no remote control interface. The most security-relevant areas are:

- The PTY hosting layer (`Terminal/PseudoTerminal.cs`, `Terminal/TerminalBridge.cs`)
- The WebView2-hosted xterm.js page and its message bridge (`Assets/terminal.html`, `terminal-init.js`)
- SSH session launching (`ShellSession.BuildSshArgs`)
- Per-session run-command execution (`Services/SessionRunner.cs`)
- State and crash-log files under `%AppData%\CodeShellManager\`

## Supported versions

Only the latest published release is supported. See the [Releases](https://github.com/umage-ai/CodeShellManager/releases) page.

| Version           | Supported |
| ----------------- | --------- |
| Latest release    | Yes       |
| Older releases    | No        |

If you're on an older version, please reproduce on the latest release before reporting.

## Reporting a vulnerability

**Please do not file a public issue for security reports.** Use one of the following private channels:

1. **GitHub Security Advisories** (preferred): https://github.com/umage-ai/CodeShellManager/security/advisories/new
2. **Email**: thraen@gmail.com

Include:

- A clear description of the issue and its impact
- Steps to reproduce (or a proof-of-concept)
- The CSM version (Settings -> About, or the installer file name)
- Your Windows version and any relevant runtime info (`dotnet --info`)
- Whether you'd like credit in the advisory

We aim to acknowledge reports within 5 business days and to ship a fix or mitigation within 30 days for high-severity issues. We will coordinate disclosure with you and credit you in the advisory unless you prefer otherwise.

## Out of scope

The following are not considered vulnerabilities:

- A user with local access to the machine being able to read their own state.json or crash.log (these live in the user's `%AppData%`).
- Behavior of the third-party CLIs hosted in a session (Claude Code, Codex, ssh, etc.).
- Self-XSS or social-engineering scenarios where the user is tricked into pasting attacker-controlled content into a terminal.
- Vulnerabilities in WebView2 / Edge runtime, .NET, or other upstream components — please report those upstream.
