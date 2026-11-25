# The `csm` command line

`csm` is the command line and MCP server of CodeShellManager. It talks to the running app, so it
can do what the app can: every tool in [tools.md](tools.md) is a subcommand. This page covers what
is not a tool: the global flags, the built-in commands, output and exit codes.

```text
csm [GLOBAL FLAGS] <command> [OPTIONS]
```

## Global flags

Put these before the command name.

| Flag | Effect |
|---|---|
| `--start` | Start CodeShellManager if it is not running, then connect. |
| `--app PATH` | The app `--start` launches. Default: the `CSM_APP` environment variable. |
| `--instance PID\|STATE-PATH` | Talk to one running instance, by its process id or the `state.json` it keeps. Default: the newest instance that is not a `--clean` run. The `CSM_INSTANCE` environment variable does the same; the flag wins. |
| `--in-process` | Run an engine inside `csm` instead of talking to the app. It has none of your sessions. For tests. |

`CSM_RUNTIME_DIR` moves the folder where the app records its running instances.

`--instance` belongs to the root command only, so a tool may have an `instance` parameter of its
own after the command name.

## Built-in commands

| Command | What it does |
|---|---|
| `csm tools [--toolset SET] [--json]` | List the tools (alias: `csm commands`). |
| `csm mcp [--session ID] [--all-tools]` | Serve the tools over MCP on stdio. MCP clients see a core set and turn on more toolsets as they need them; `--all-tools` lists every tool and ignores toolsets. `--session` names the session the server runs inside, so an agent cannot type into its own session. `--auto-allow` (only with `--in-process`) answers every consent request, because there is no app to ask. |
| `csm events` | Print the running app's engine events as JSON lines until interrupted. |
| `csm attach` | Show a session of a paired office in this console. See [remote-access.md](remote-access.md). |
| `csm setup [--client ID] [--dry-run] [--json]` | Add `csm mcp` to Claude Code, Claude Desktop, Codex and Cursor. Keeps comments, makes a backup, verifies. |
| `csm doctor [--json]` | Check the install and print the fix for anything wrong. `--skip-self-test` and `--skip-signature` skip the slower checks. |
| `csm uninstall [--client ID] [--dry-run] [--json]` | Remove only the entries `csm setup` added. |

## Running a tool

Every tool is a subcommand named in kebab-case: the tool `session_new` is `csm session-new`.
Parameters are flags, also in kebab-case (`--folder`, `--command`). Strings, numbers and booleans
are typed; arrays and objects take a JSON value. `csm <tool> --help` lists a tool's flags.

```powershell
csm session-list
csm session-new --folder C:\src\app --command claude
csm session-new --json '{"folder": "C:\\src\\app"}'     # all parameters as JSON
csm session-new --json @params.json                      # or from a file
```

Tools that return text (such as reading a screen) print the text. Other tools print one line. Add
`--json` on its own to print `{"message": ..., "data": ...}`; errors are then printed as JSON too.

A tool that types into a session, reads its output, starts a process, writes files or may discard
data raises a consent card in the app for `csm` as it does for an MCP client. Answer it in the app.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Done. |
| 1 | The tool returned an error. |
| 2 | Usage error: an unknown command or a bad flag value. |
| 3 | The app is not reachable. Start it, or use `--start`. |

`csm attach` uses 0 when you detach or the session ends, 2 for a usage problem and 3 when the
office cannot be reached or the link drops.
