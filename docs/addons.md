# Add-ons

Status: phase 1 (declarative packs). Process add-ons are parsed and validated, not run yet.
Spec for the manifest format, the enable rules and the tools. The code is `csm-core::addons`
(parsing, pure), `csm-services::addons` (the folder), `csm-engine::addons` and `tools/addons.rs`
(state and tools), and `csm-ui/src/screens/addons.rs` (Settings > Add-ons).

## Where add-ons live

An add-on is a folder `<config dir>/addons/<id>/` containing `addon.toml`. The config dir is the
folder `state.json` lives in (`--config-dir`, `CSM_CONFIG_DIR`, the portable folder, or the
platform data dir).

- The folder name **must equal the id**. Ids are 1 to 64 characters: `a-z`, `0-9`, `-`.
- Symlinked add-on folders are not followed. Other files in the folder are ignored.
- `addon.toml` is read only if it is 256 KiB or less and valid UTF-8.
- Up to 256 folders are looked at per scan.
- **Nothing is read under `--clean`, and no add-on loads in an isolated run** (no state folder).

## The manifest

```toml
[addon]                         # required
id = "generic-agent"            # required; must equal the folder name
name = "Generic agent"          # required; control characters dropped, 64 characters max
version = "0.1.0"               # required; semantic version
description = "..."             # optional; 1000 characters max
api = 1                         # required; the manifest API. Refused above 1 with "Update CodeShellManager"
kind = "pack"                   # required: "pack" or "process"
homepage = "https://..."        # optional
author = "..."                  # optional

[[profile]]                     # an agent launch profile (see Profiles)
name = "agent"
argv = ["my-agent"]
args = ["--plain"]              # optional, appended after argv
resume = "re-run"               # "none" (default), "re-run", or an argv list with {session_id}
status = [                      # optional status rules, first match wins
  { match = "\\(y/n\\)\\s*$", status = "needs_you_approval" },
]

[[run_template]]                # a run command the person can add to a session's Runs
name = "test"
argv = ["npm", "test"]
cwd = "{folder}"                # optional: {folder} or a relative folder name

[[slash]]                       # a Claude slash preset for the slash menu
command = "/review"
description = "Review the uncommitted changes"

[[command]]                     # a palette command: calls an existing tool with fixed params
id = "list-sessions"
title = "List sessions"
tool = "session_list"
params = { include_asleep = false }

[process]                       # only for kind = "process" (validated, not run yet)
argv = ["my-helper"]
effects = ["launches_process"]  # from: launches_process, sends_input, reads_session_content,
                                #       writes_filesystem, accesses_network
```

Unknown top-level tables are reported and ignored. A `[process]` table in a pack is reported and
ignored.

### Plain tokens

Every argv element (profiles, run templates, `[process]`) is a **plain token**: letters, digits and
`_ - . / \ : = + @ ,`. Nothing else is allowed, so no quote, space, `$`, `%`, `&`, `|`, `;` or
backtick can ever reach a shell. Split a value with spaces into separate elements. A token that is
not plain is reported and its entry is skipped.

Placeholders are not substituted in argv lists, so an argv element with `{session_id}` or `{folder}`
is refused (reported, entry skipped). The two places they mean something are the resume template
(`{session_id}`) and a run template's `cwd` (`{folder}`).

A run template's argv is joined with single spaces when added to a session and then run **in a
shell** (`cmd.exe` or the user's shell, as any Runs command). That is safe only because the tokens
are plain. The consent dialog shows that exact command line.

### Limits

| Limit | Value |
|---|---|
| Entries per kind (profile, run_template, slash, command) | 32 (extra ones reported) |
| Name, title, id | 64 characters |
| Description | 1000 characters |
| Argv elements | 32 per list |
| One argv element | 512 bytes |
| Status rules per profile | 16 |
| Regex source | 512 bytes, compiled with a 1 MiB size limit |
| Palette command params (JSON) | 4 KiB |
| Manifest file | 256 KiB |

### Errors: whole add-on or one entry

- **The whole add-on is an error** (state `error`, its message shown) when the TOML does not parse,
  `[addon]` is missing, a required field is missing or invalid, `api` is above 1, `kind` is not
  `pack` or `process`, or the folder name differs from the id.
- **One entry is skipped** (reported in `problems`, the rest still loads) when it is malformed:
  a bad argv token, a bad regex, an unknown status or effect, a bad resume rule, a command whose tool
  id is not valid. A bad status rule drops its whole profile, since the profile's behaviour would be
  wrong without it.
- **A palette command naming a tool that does not exist** is dropped when the add-on loads (the
  registry is the authority). A command may never call an `addon_*` tool, and may not call a tool
  that writes into a session (`session_send_text` and similar) or enables an extension.

## Profiles

A profile is an agent launch profile. `addon-id.name` is its id.

- `session_new` with `profile = "generic-agent.agent"` starts a session whose command is `argv[0]`
  and whose arguments are `argv[1..]` followed by `args`. Passing `command` or `args` with a profile
  is refused. The session records its profile (`addon_profile`).
- A profile of a disabled add-on cannot be used.
- **Status rules.** While a session from a profile is live and not reporting OSC 7501 status, its
  screen is read about once a second on a worker. The first rule whose regex matches the last 12
  non-empty lines (up to 4 KiB) sets the status: `needs_you_approval`, `needs_you_question`,
  `working`, `idle` or `done` (done shows as "done, unseen"). No match leaves the status to the
  usual rules.
- **Resume.** Restore starts a session again unless its profile says otherwise:
  - `"re-run"` (and the default, when the restore path can start the session) relaunches the same argv.
  - `"none"` brings the session back **asleep**; the person wakes it.
  - An argv template with `{session_id}` is validated but **not applied yet** (no session id is
    passed to the program). It behaves like `"re-run"` until it is applied; see Limits.
  - A session from a profile whose add-on is **not enabled** also comes back asleep. Nothing from an
    add-on runs on restore unless it is enabled and its profile allows it.
  - The add-on folder is read at start on a worker, so profile sessions wait for that read before
    they restore (or stay asleep).

## Run templates

`addon-id.name` in `addon_list` and in the Runs panel. `addon_run_apply` (app only, refused for MCP
clients like `run_edit`) appends the template to a session's run commands with fresh ids. The first
one becomes the default when the list is empty. A template with a `cwd` other than `{folder}` is
refused: run commands have no working-folder field yet.

## Slash presets

Enabled add-ons' presets are added to the slash menu after the person's own. A preset whose command
the person already has is not added. Its description says which add-on it came from:
`Review the uncommitted changes (Generic agent)`.

## Palette commands

`addon-id.id` is the command id. Each one calls its tool through the normal dispatch, with its
fixed params, on behalf of the person in the app (the palette is a UI caller, so no consent card,
as with any button). A command can only name a tool this app has, and never an `addon_*` tool.

## Enabling: the consent point

Add-ons are **off until the person enables them**.

1. Settings > Add-ons lists every folder with its state (enabled, disabled, error), its message
   and its skipped entries.
2. **Enable…** opens a confirm dialog listing what the add-on contributes: profiles, run
   commands, slash presets, palette commands, and for a process add-on the effects it declares.
3. Confirming calls `addon_enable` with the `version` and `digest` shown (both from `addon_list`).
   The engine refuses if the file now differs in either, so what the person approved is exactly the
   text that loads.

The engine stores the **SHA-256 of the `addon.toml` text** that was approved, in
`state.json` settings under `AddonApprovals` (id to digest). An add-on whose file changes after
approval is **disabled** with the message "the file changed since you enabled it", until it is
enabled again. Disabling removes the approval, so enabling again asks again.

`AddonApprovals` cannot be written with `settings_set`, and a state import never changes it.

For MCP clients `addon_enable` declares `ENABLES_EXTENSION` (a new effect, part of the consent rule),
so the client gets a consent card like any other tool that would run something the person has not
seen. Disabling does not ask.

Phase 2 will add signing and a registry. The approval is per digest, so a signature can later sit
beside it without changing what is stored.

## Tools (toolset `addons`)

| id | effects | what |
|---|---|---|
| `addon_list` | read | every add-on, its state, message, contributions and skipped entries; the folder; scan status |
| `addon_reload` | state | read the folder again; answers when the read is done |
| `addon_enable` | state, enables extension (consent for MCP) | `id`, `version` and `digest` (both as shown) |
| `addon_disable` | state | `id`; forgets the approval |
| `addon_open_folder` | launches a process, writes files | creates the folder if missing and opens it |
| `addon_run_apply` | state (refused for MCP) | `session_id`, `template` (`addon-id.name`) |

`session_new` takes `profile`, and `session_send_command` and the slash menu read the merged slash list.

## Process add-ons (phase 2)

`kind = "process"` with a `[process]` table is parsed and validated, and its effects are shown in the
consent dialog. It **runs nothing yet**: it contributes no profiles, commands or presets, and enabling
it adds nothing beyond the approval. Phase 2 will start it out of process over stdio, with a tool
surface and event subscriptions, and each effect checked against what it declared here.

## Example

`docs/addons/examples/generic-agent/addon.toml` is a complete pack: a profile for a generic CLI agent
with status rules, a run template, a slash preset and a palette command. Copy the folder into
`<config dir>/addons/`, then enable it in Settings > Add-ons. `crates/csm-core/tests/example_pack.rs`
parses it with no problems.

## Limits (phase 1)

- Process add-ons do not run.
- Profile resume templates (`{session_id}`) are validated but not applied.
- Run templates with a working folder are refused (run commands have none).
- No registry, signing or update check: add-ons are trusted on enable, with the consent above.
- The quick-open does not list profiles yet (use `session_new` with `profile`, or the palette tool).
- Runs has no "add from add-on" button yet (the `addon_run_apply` tool does it).
- A status rule reads the screen about once a second; a short-lived match can be missed.
- A profile's status rules stop applying the moment its add-on is off; the last status it set is
  cleared on the next tick.
- Run templates run in a shell, as any Runs command does; a `cwd` other than `{folder}` is refused.
- A symlink to a file inside an add-on folder is followed (the file is size-capped, not refused);
  Windows junctions are not detected as links.
