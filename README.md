# CodeShellManager

[![Build](https://github.com/umage-ai/CodeShellManager/actions/workflows/build.yml/badge.svg)](https://github.com/umage-ai/CodeShellManager/actions/workflows/build.yml)
[![Latest Release](https://img.shields.io/github/v/release/umage-ai/CodeShellManager?label=download)](https://github.com/umage-ai/CodeShellManager/releases/latest)
[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)

A Windows desktop app for running **multiple AI coding agents side-by-side** — Claude Code, Codex, GitHub Copilot, or any CLI tool — in a tabbed and grid-layout terminal host.

Built with WPF + [xterm.js](https://xtermjs.org/) + Windows ConPTY for full pseudo-terminal fidelity.

---

## Features

- **Multi-terminal grid** — run up to 18 sessions simultaneously in configurable layouts (1, 2, 3, 4, 6 columns; 2×2, 6×2, 6×3 grids)
- **Full-text search** — all terminal output indexed to SQLite FTS5; instant search across every session, ever
- **Per-project notepad** — collapsible 📝 notes panel on every terminal, auto-saved and searchable
- **Alert detection** — detects when Claude is waiting for input or tool approval; green/orange dot indicators
- **Git status** — shows branch and dirty state in the sidebar per session
- **Session rename** — double-click any session name or click ✏ to rename inline
- **Auto-resume** — automatically resumes the last Claude Code session when restoring on startup (`--resume <id>`)
- **Session history** — clicking a search result from a closed session offers to relaunch it
- **Configurable launch commands** — customise the commands available in the New Session dialog
- **Claude badge** — sessions running `claude` commands get a visual indicator
- **Tray icon** — minimises to system tray; balloon notifications for alerts
- **Settings window** — all options configurable; persisted as JSON

## Requirements

- Windows 10 version 1903+ or Windows 11
- [Microsoft Edge WebView2 Runtime](https://developer.microsoft.com/en-us/microsoft-edge/webview2/) (pre-installed on Windows 11; available as a free download for Windows 10)

> **Note:** The `.msi` installer does not bundle the WebView2 runtime. If you're on Windows 10 and see a blank terminal pane, install the WebView2 runtime from the link above.

## Installation

### Download (recommended)

1. Go to [**Releases**](https://github.com/umage-ai/CodeShellManager/releases/latest)
2. Download either:
   - `CodeShellManager-x.y.z-Setup.msi` — installer (adds Start Menu + Desktop shortcuts, supports uninstall via Apps & Features)
   - `CodeShellManager-x.y.z-win-x64.zip` — portable; extract and run `CodeShellManager.exe`

### Build from source

**Requirements:** [.NET 10 SDK](https://dotnet.microsoft.com/download/dotnet/10.0), Windows 10/11

```bash
git clone https://github.com/umage-ai/CodeShellManager.git
cd CodeShellManager
dotnet run --project src/CodeShellManager/CodeShellManager.csproj
```

## Keyboard Shortcuts

| Key | Action |
|-----|--------|
| `Ctrl+T` | New session |
| `Ctrl+W` | Close active session |
| `Ctrl+F` | Toggle search |
| `Ctrl+Tab` | Cycle sessions |
| `Escape` (in search) | Close search panel |

## Layout Options

Click the layout buttons in the toolbar (right side):

| Button | Layout |
|--------|--------|
| ▣ | Single pane |
| ▥ | 2 columns |
| ▦ | 3 columns |
| ⊞ | 2×2 grid |
| ⇔ | 2 rows |
| 4 | 4 columns |
| 6 | 6 columns |
| 6×2 | 6 columns × 2 rows (12 panes) |
| 6×3 | 6 columns × 3 rows (18 panes) |

## Contributing

Issues and pull requests are welcome. See [CLAUDE.md](CLAUDE.md) for architecture notes and coding conventions.

## License

MIT — see [LICENSE](LICENSE).

---

By [umage.ai](https://umage.ai)
