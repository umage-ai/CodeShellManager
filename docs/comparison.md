# How CodeShellManager compares

*Competitor columns last reviewed: October 2026. The CodeShellManager column describes 2.0.* This
space moves fast. If something here is out of date, please
[open an issue](https://github.com/umage-ai/CodeShellManager/issues) and we'll correct it.

This compares CodeShellManager (CSM) with tools for running several AI coding agents at once **on Windows**. Competitor details come from each tool's own documentation and from published reviews (linked under [Sources](#sources)), not from hands-on benchmarking. A **?** means we couldn't confirm it either way.

Popular tools without a Windows build (Conductor, Superset, cmux) are left out.

## The tools

| Tool | What it is | Windows | Licence |
|---|---|---|---|
| **CodeShellManager** | Native terminal host for many real CLI agent sessions, with a Needs you inbox, search, groups, sleep, a task Board, and CLI/MCP control | Native (macOS and Linux coming) | MIT |
| **Claude Code Desktop** | Anthropic's official app: chat-style UI for Claude, with panes, diffs and previews | Native | Proprietary |
| **Nimbalyst** | Desktop workspace with Claude Code and Codex sessions on a kanban board, plus editors | Native | MIT |
| **Orca** | Sends one prompt to several agents, each in its own git worktree, and compares results | Native | MIT |
| **Herdr** | Rust terminal multiplexer that tracks whether each agent is working, blocked or idle | Beta | Apache-2.0 |
| **Claude Squad** | Go terminal UI on top of tmux and git worktrees | WSL only | AGPL-3.0 |
| **GridVibe** | Python grid of agent, SSH and WSL panes, with offline voice input | Native (needs Python) | ? |

## Feature matrix

✅ has it · ◐ partial · ❌ doesn't · ? unconfirmed

| | **CSM** | Claude Code Desktop | Nimbalyst | Orca | Herdr | Claude Squad | GridVibe |
|---|---|---|---|---|---|---|---|
| **Agents and terminals** | | | | | | | |
| Real terminal per session (unmodified CLI) | ✅ | ❌ chat UI | ❌ chat UI | ◐ | ✅ | ✅ | ✅ |
| Any CLI agent or shell | ✅ | ❌ Claude only | ◐ 2–4 agents | ◐ 4 agents | ✅ | ◐ | ◐ 6 agents |
| Sessions visible at once | ✅ every live one (Overview); grids up to 6×6 | ◐ 2 (split) | ? | ? | ✅ tiling panes | ◐ | ◐ 8 |
| **Search and memory** | | | | | | | |
| Full-text search across all output, incl. closed sessions | ✅ | ❌ | ◐ task search | ❌ | ❌ | ❌ | ❌ |
| Notes per project | ✅ searchable | ❌ | ✅ docs | ❌ | ❌ | ❌ | ❌ |
| **Attention and scale** | | | | | | | |
| Waiting / needs-approval detection | ✅ Needs you inbox + tray | ◐ on finish only | ✅ iOS push | ? | ✅ | ◐ | ✅ |
| Working / idle state per agent | ✅ working · needs you · done · idle · asleep · error | ? | ? | ? | ✅ working · blocked · done · idle | ? | ? |
| Sleep a session (free resources, keep its slot) | ✅ | ◐ archive | ❌ | ❌ | ❌ | ◐ pause | ❌ |
| Restore and resume Claude on startup | ✅ auto `--resume` | ✅ | ✅ | ? | ✅ detach/attach | ◐ | ✅ |
| Task board across agents | ✅ Board, with subagents | ? | ✅ kanban | ? | ❌ | ❌ | ❌ |
| **Remote and environments** | | | | | | | |
| SSH sessions | ✅ | ✅ | ? | ✅ | ✅ | ❌ | ✅ |
| WSL sessions | ✅ with git status | ◐ fewer features | ◐ path translation | ? | ❌ | ◐ runs inside WSL | ✅ |
| Agents keep running with the window closed | ◐ in the tray, while the app runs | ✅ cloud | ? | ? | ✅ | ✅ tmux | ❌ |
| Mobile or remote control | ❌ | ✅ Dispatch | ✅ iOS | ✅ | ✅ SSH attach | ❌ | ❌ |
| Agents and scripts drive the app (CLI / MCP) | ✅ `csm`, `csm mcp`, with consent cards | ? | ? | ? | ? | ? | ? |
| **Review and workflow** | | | | | | | |
| Git worktree creation | ◐ one click, not automatic | ✅ per session | ✅ | ✅ core feature | ◐ via plugins | ✅ | ? |
| Diff / review view | ◐ Review add-on (ships as a sample) | ✅ | ✅ | ✅ | ◐ via plugin | ✅ | ◐ git sidebar |
| Browser / app preview | ❌ opens the URL in your browser | ✅ | ✅ | ? | ❌ | ❌ | ✅ |
| Per-session run commands (F5, background output) | ✅ incl. `package.json` scripts | ◐ preview servers | ❌ | ❌ | ❌ | ❌ | ❌ |
| **Install** | | | | | | | |
| How you install it | winget · choco · MSI | installer | installer | installer | PowerShell script | WSL + tmux + gh | Python 3.10+ |

## Where CodeShellManager stands out

1. **It runs the real CLI, so you lose nothing.** Chat-style apps wrap the agent in their own UI, and features fall off at the edges. For example, Claude Code Desktop's docs note that plugins aren't available in WSL sessions and that its terminal pane is local-only. CSM runs `claude` exactly as Windows Terminal would, through your PowerShell 7 profile, so hooks, plugins, the status line and profile functions all work. It's agent-neutral too: Copilot CLI, Codex, Gemini, aider or a plain `pwsh` run the same way.

2. **One place that says who needs you.** Every session has one status, shown the same way everywhere, and the Needs you inbox lists the sessions waiting on you, oldest first. Programs that report their state (OSC 7501, as Claude Code does) drive it exactly; for the rest, CSM reads the screen.

3. **Full-text search over everything any session printed.** All terminal output is indexed to SQLite FTS5. With many agents running, the question that matters is "which session printed that error, URL or decision?". CSM answers it even for closed sessions, and can reopen a closed session from the search result.

4. **Built for large fleets.** Groups with their own layouts and windows, an Overview of every live session, a Board of agents and tasks with each Claude session's subagents, bulk actions on a multi-selection, sleep and wake, and staggered Claude launches that avoid corrupting Claude's shared config file.

5. **Scriptable, and open to agents.** Every action is a tool: `csm <tool>` on the command line, or `csm mcp` for any MCP client. Tools that send input, read output, start processes or write files ask you first on a consent card.

6. **A native app.** A GPU-rendered terminal over ConPTY, Windows' own pseudo-terminal API, with no Electron, WebView, Python, daemon, tmux or WSL prerequisite. Installs from winget or Chocolatey and needs no account.

7. **Run commands that don't touch the agent.** F5 runs tests, a build or a dev server in a separate background process with its output in the inspector, ready to copy to the agent.

8. **Smaller touches.** Programs in a session can push their colour, git branch and title via [OSC 9001](shell-integration.md), so SSH tools can report state. Each project has a searchable notepad, and its `CLAUDE.md` / `AGENTS.md` / `README.md` one tab away. Claude usage and cost per session. Ctrl+Shift+T reopens a closed session. Windows Terminal profiles import with their colours and font size.

## Where it lags

- **Closing the app ends the sessions.** Close-to-tray keeps them running while the app runs, but nothing survives quitting.
- **Review is an add-on**, not built into the main window, and there is no commit or push: git in CSM is read-mostly on purpose.
- **Worktrees are one click, not automatic.** CSM creates, lists, jumps between, prunes and removes worktrees, but Orca, Claude Code Desktop and Nimbalyst make one worktree per task the default.
- **No mobile or remote companion** yet.
- **Windows only for now.** macOS and Linux builds are coming.

## Sources

- [Claude Code Desktop documentation](https://code.claude.com/docs/en/desktop)
- [Nimbalyst](https://nimbalyst.com/) and its [session-manager comparison](https://nimbalyst.com/blog/best-session-managers-for-claude-code-and-codex/)
- [Orca (stablyai/orca)](https://github.com/stablyai/orca)
- [Herdr (herdrdev/herdr)](https://github.com/herdrdev/herdr), its [agent-state docs](https://herdr.dev/docs/agents/), and the plugins [herdr-reviewr](https://github.com/persiyanov/herdr-reviewr) and [herdr-projects](https://github.com/eliasstravik/herdr-projects)
- [Herdr review (bitdoze)](https://www.bitdoze.com/herdr-agent-multiplexer/) and [guide (Better Stack)](https://betterstack.com/community/guides/ai/herdr-ai-agent/)
- [Claude Squad](https://github.com/smtg-ai/claude-squad)
- [GridVibe](https://gitblind.noratr.app/JSstudent/gridvibe)
- [awesome-agent-orchestrators](https://github.com/andyrewlee/awesome-agent-orchestrators)
