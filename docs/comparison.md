# How CodeShellManager compares

*Last reviewed: October 2026.* This space moves fast. If something here is out of date, please [open an issue](https://github.com/umage-ai/CodeShellManager/issues) and we'll correct it.

This compares CodeShellManager (CSM) with tools for running several AI coding agents at once **on Windows**. Competitor details come from each tool's own documentation and from published reviews (linked under [Sources](#sources)), not from hands-on benchmarking. A **?** means we couldn't confirm it either way.

Popular tools without a Windows build (Conductor, Superset, cmux) are left out.

## The tools

| Tool | What it is | Windows | Licence |
|---|---|---|---|
| **CodeShellManager** | WPF terminal host for many real CLI agent sessions, with search, groups and sleep | Native | MIT |
| **Claude Code Desktop** | Anthropic's official app: chat-style UI for Claude, with panes, diffs and previews | Native | Proprietary |
| **Nimbalyst** | Desktop workspace with Claude Code and Codex sessions on a kanban board, plus editors | Native | MIT |
| **Orca** | Sends one prompt to several agents, each in its own git worktree, and compares results | Native | MIT |
| **Herdr** | Rust terminal multiplexer that tracks whether each agent is working, blocked or idle | Beta | AGPL-3.0 |
| **Claude Squad** | Go terminal UI on top of tmux and git worktrees | WSL only | AGPL-3.0 |
| **GridVibe** | Python grid of agent, SSH and WSL panes, with offline voice input | Native (needs Python) | ? |

## Feature matrix

✅ has it · ◐ partial · ❌ doesn't · ? unconfirmed

| | **CSM** | Claude Code Desktop | Nimbalyst | Orca | Herdr | Claude Squad | GridVibe |
|---|---|---|---|---|---|---|---|
| **Agents and terminals** | | | | | | | |
| Real terminal per session (unmodified CLI) | ✅ | ❌ chat UI | ❌ chat UI | ◐ | ✅ | ✅ | ✅ |
| Any CLI agent or shell | ✅ | ❌ Claude only | ◐ 2–4 agents | ◐ 4 agents | ✅ | ◐ | ◐ 6 agents |
| Sessions visible at once | ✅ 18 | ◐ 2 (split) | ? | ? | ✅ tiling panes | ◐ | ◐ 8 |
| **Search and memory** | | | | | | | |
| Full-text search across all output, incl. closed sessions | ✅ | ❌ | ◐ task search | ❌ | ❌ | ❌ | ❌ |
| Notes per project | ✅ searchable | ❌ | ✅ docs | ❌ | ❌ | ❌ | ❌ |
| **Attention and scale** | | | | | | | |
| Waiting / needs-approval detection | ✅ dots + tray | ◐ on finish only | ✅ iOS push | ? | ✅ | ◐ | ✅ |
| Sleep a session (free resources, keep its slot) | ✅ | ◐ archive | ❌ | ❌ | ❌ | ◐ pause | ❌ |
| Restore and resume Claude on startup | ✅ auto `--resume` | ✅ | ✅ | ? | ✅ detach/attach | ◐ | ✅ |
| **Remote and environments** | | | | | | | |
| SSH sessions | ✅ | ✅ | ? | ✅ | ✅ | ❌ | ✅ |
| WSL sessions | ✅ with git status | ◐ fewer features | ◐ path translation | ? | ❌ | ◐ runs inside WSL | ✅ |
| Agents keep running with the window closed | ❌ ([#149](https://github.com/umage-ai/CodeShellManager/issues/149)) | ✅ cloud | ? | ? | ✅ | ✅ tmux | ❌ |
| Mobile or remote control | ❌ | ✅ Dispatch | ✅ iOS | ✅ | ✅ SSH attach | ❌ | ❌ |
| **Review and workflow** | | | | | | | |
| Git worktree creation | ◐ manual | ✅ per session | ✅ | ✅ core feature | ❌ | ✅ | ? |
| Diff / review view | ❌ ([#148](https://github.com/umage-ai/CodeShellManager/issues/148)) | ✅ | ✅ | ✅ | ❌ | ✅ | ◐ git sidebar |
| Browser / app preview | ❌ post-run URL only | ✅ | ✅ | ? | ❌ | ❌ | ✅ |
| Per-session run commands (F5, background output) | ✅ | ◐ preview servers | ❌ | ❌ | ❌ | ❌ | ❌ |
| **Install** | | | | | | | |
| How you install it | winget · choco · MSI | installer | installer | installer | PowerShell script | WSL + tmux + gh | Python 3.10+ |

## Where CodeShellManager stands out

1. **It runs the real CLI, so you lose nothing.** Chat-style apps wrap the agent in their own UI, and features fall off at the edges. For example, Claude Code Desktop's docs note that plugins aren't available in WSL sessions and that its terminal pane is local-only. CSM runs `claude` exactly as Windows Terminal would, through your PowerShell 7 profile, so hooks, plugins, the status line and profile functions all work. It's agent-neutral too: Copilot CLI, Codex, Gemini, aider or a plain `pwsh` run the same way.

2. **Full-text search over everything any session printed.** All terminal output is indexed to SQLite FTS5. With many agents running, the question that matters is "which session printed that error, URL or decision?". CSM answers it even for closed sessions, and can relaunch a closed session from the search result.

3. **Built for large fleets.** Up to 18 panes on screen, groups with bulk actions, sleep and wake, restart-all, and staggered Claude launches that avoid corrupting Claude's shared config file. CSM is tuned and measured at 50+ sessions, and sleep keeps dozens of projects one click away without dozens of live processes.

4. **A native Windows app.** Built on ConPTY, Windows' own pseudo-terminal API, with no Electron, Python, daemon, tmux or WSL prerequisite. Installs from winget or Chocolatey and needs no account.

5. **Run commands that don't touch the agent.** F5 runs tests or a build in a separate background process with its output in a drawer, then pastes that output to the agent in one click.

6. **Smaller touches.** Programs in a session can push their colour, git branch and title to CSM via [OSC 9001](shell-integration.md), so SSH tools can report state. Each project has a searchable notepad. Ctrl+Shift+T reopens a closed session. Windows Terminal profiles import with their fonts and colour schemes.

## Where it lags

- **No diff or review view** yet: [#148](https://github.com/umage-ai/CodeShellManager/issues/148).
- **Closing the window ends the sessions.** Close-to-tray with live status is planned: [#149](https://github.com/umage-ai/CodeShellManager/issues/149).
- **Worktrees are manual.** Orca, Claude Code Desktop and Nimbalyst make one worktree per task the default.
- **No mobile or remote companion.**
- **Windows-only, by design.**

## Sources

- [Claude Code Desktop documentation](https://code.claude.com/docs/en/desktop)
- [Nimbalyst](https://nimbalyst.com/) and its [session-manager comparison](https://nimbalyst.com/blog/best-session-managers-for-claude-code-and-codex/)
- [Orca (stablyai/orca)](https://github.com/stablyai/orca)
- [Herdr review (bitdoze)](https://www.bitdoze.com/herdr-agent-multiplexer/) and [guide (Better Stack)](https://betterstack.com/community/guides/ai/herdr-ai-agent/)
- [Claude Squad](https://github.com/smtg-ai/claude-squad)
- [GridVibe](https://gitblind.noratr.app/JSstudent/gridvibe)
- [awesome-agent-orchestrators](https://github.com/andyrewlee/awesome-agent-orchestrators)
