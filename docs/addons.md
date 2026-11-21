# Add-ons

Status: phase 2 (declarative packs, and process add-ons over stdio). Signing and a registry are not built yet.
Spec for the manifest format, the enable rules and the tools. The code is `csm-core::addons`
(parsing, pure), `csm-services::addons` (the folder), `csm-engine::addons` and `tools/addons.rs`
(state and tools), and `csm-ui/src/screens/addons.rs` (Settings > Add-ons).

## Where add-ons live

An add-on is a folder `<config dir>/addons/<id>/` containing `addon.toml`. The config dir is the
folder `state.json` lives in (`--config-dir`, `CSM_CONFIG_DIR`, the portable folder, or the
platform data dir).

- The folder name **must equal the id**. Ids are 1 to 64 characters: `a-z`, `0-9`, `-`.
- Symlinked add-on folders are not followed, and a `addon.toml` that is a link is refused. Other files in the folder are ignored.
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

[process]                       # only for kind = "process": the program CSM starts (see Process add-ons)
argv = ["my-helper"]
effects = ["launches_process"]  # from: launches_process, sends_input, reads_session_content,
                                #       writes_filesystem, accesses_network, stores_secrets, stores_data
calls = ["session_list"]        # core tools it may run through csm/call without a card (optional)
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
fixed params, on behalf of the person in the app. The palette is a UI caller, so a click runs it
with no second prompt, as with any palette tool. That is a deliberate choice: the enable dialog
lists each command with its tool and its exact params, and the person approved those. A command
can only name a tool this app has. It can never name an `addon_*` tool, a tool that writes into a
session (`SENDS_INPUT`, so a pack's text never reaches a PTY through one click), or a tool that
turns on another extension (`ENABLES_EXTENSION`). Such a command is dropped when the add-on loads,
with a note.

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
| `addon_install_samples` | state, writes files | copies the bundled sample add-ons that are not in the folder yet (never overwrites); they arrive disabled |
| `addon_run_apply` | state (refused for MCP) | `session_id`, `template` (`addon-id.name`) |

`session_new` takes `profile`, and `session_send_command` and the slash menu read the merged slash list.

## Process add-ons

`kind = "process"` with a `[process]` table makes the add-on a **program that CSM starts**. It
contributes tools, not profiles, run commands, presets or palette commands. Those belong to packs:
a process add-on that declares them has them ignored, and the problem is listed.

```toml
[process]
argv = ["my-helper", "--stdio"]     # plain tokens; the program and its arguments, no shell
effects = ["writes_filesystem"]     # the most the add-on's tools may use (see below)
calls = ["session_list"]            # core tools it may call without a consent card (see below)
```

### Start and stop

- Enabling it (the same consent as a pack) starts the program. Disabling it, a reload that finds a
  changed `addon.toml`, or quitting the app stops it. Quitting waits at most 3 seconds for it.
- It runs with **argv only** (no shell), its **working folder is the add-on folder**, its stdin and
  stdout are pipes, and on Windows it opens **no console window**. It inherits the app's environment.
- Its stderr is captured into a 2 KiB tail, shown by `plugin_status`. Nothing else is read from it.

### Messages

Newline-delimited **JSON-RPC 2.0**, one message per line, each **at most 1 MiB**. The first message
CSM sends is the handshake:

```
-> {"jsonrpc":"2.0","id":1,"method":"initialize","params":{"api":1,"csm_version":"0.0.1",
    "capabilities":{"caller":1,"data_dir":1,"standing_calls":1,"screens":1},"data_dir":null}}
<- {"jsonrpc":"2.0","id":1,"result":{"api":1,"name":"Echo test","tools":[
      {"id":"echo-test.echo","title":"Echo","summary":"Returns its arguments",
       "input_schema":{"type":"object"},"effects":[]},
      {"id":"echo-test.write","title":"Write","summary":"Writes a file","effects":["writes_filesystem"]}],
    "subscriptions":["session_changed","inbox_changed"]}}
```

- `api` must be 1 (`PROTOCOL_API`). Another value refuses the plugin.
- `capabilities` is an object of feature name to version, so a plugin can tell what this CSM does:
  `caller` (`tools/call` carries `caller`), `data_dir` (the data folder below), `standing_calls`
  (`[process] calls`), `screens` (the add-on may declare screens; see Screens below). Names are only ever added; ignore the ones you do not know. A CSM that
  predates one leaves it out, so check before relying on it.
- `data_dir` is the add-on's own folder, `<state folder>/addon-data/<id>/`, which CSM creates when
  the process starts. It is a string only for an add-on that declared `stores_data` and only when
  the run has a state folder; it is **`null`** otherwise, and always under `--clean` and in an
  isolated run (nothing is written there). A plugin given `null` keeps its data in memory.
- Each tool id must be **`<add-on id>.<name>`**, with `<name>` lowercase letters, digits and `_`
  (up to 48 bytes). Any other prefix is refused.
- A tool's `effects` may only name effects the `[process]` table declares. A tool that names an
  undeclared one is **refused** (the other tools still load), and the problem is shown.
- `input_schema` is an object schema, at most 16 KiB. Omitted, it accepts no fields.
- `subscriptions` may name `session_changed`, `session_removed`, `inbox_changed`, and, for sessions the
  add-on owns, `session_asleep` and `session_awake`. Each matching
  event is sent to the plugin as a notification:
  `{"jsonrpc":"2.0","method":"event","params":{"event":"session_changed","session_id":"..."}}`.
  Events carry only what the engine event carries (ids, not text).
- Limits: 32 tools, 8 subscriptions, title 64 characters, summary 500 characters.

**Calls into the plugin.** A tool call from the app, the CLI or MCP becomes:

```
-> {"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"echo-test.echo","arguments":{},
    "caller":{"kind":"external","client":"claude-code","session_id":"...","session_name":"...","project":"..."}}}
<- {"jsonrpc":"2.0","id":7,"result":{"message":"echoed","data":{}}}
```

**Who is calling (`caller`).** CSM fills `caller` on every `tools/call` from its own record of who
asked; it is never taken from the plugin or the arguments, and an argument named `caller` stays an
argument. `kind` is one of: `ui` (the person, in the app), `cli` (`csm <tool>`), `external` (an MCP
client: `client` is the name it gave, and when it runs inside one of our sessions also
`session_id`, `session_name` and `project`, the project key tasks use), or `addon` (another add-on's
`csm/call`, or this one's own: `addon` is its id). A plugin uses it to refuse what only the person
should do (for example pairing a device from an MCP agent) and to scope an agent to its project. It
is only present on calls that come through CSM, which is all of them.

`message` (500 characters) and `data` are both optional. An error answer
(`{"error":{"code":..,"message":..}}`) becomes a failed call with its message. A call that is not
answered in **30 seconds** times out. At most 16 calls are in flight per add-on, and at most 64 wait
at once across all add-ons; more are refused.

CSM never sends a message over 1 MiB. A call whose arguments are that large fails before it is sent;
a `csm/call` result that would be is answered with an error instead; an event that would be is dropped.

**Calls out of the plugin (`csm/call`).** A plugin may run any CSM tool:

```
-> {"jsonrpc":"2.0","id":9,"method":"csm/call","params":{"tool":"session_list","params":{}}}
<- {"jsonrpc":"2.0","id":9,"result":{"message":"...","data":{}}}
```

The call runs as the **add-on's own caller** (`add-on <id>`, not the person). It is consent-checked
like an MCP call, so a tool whose effects need consent asks the person first, and the consent card
names the add-on. A failed call answers with `error.code = -32000` and `error.data` holding the tool
error (`{"code":"refused","message":"..."}`). At most 8 `csm/call`s run at once per add-on.
`csm/log` with `{"message": "..."}` is written to the app log. Any other method is answered with
`-32601`.

**Standing consent (`calls`).** `[process] calls` lists core tools that this add-on may run through
`csm/call` **without a consent card**, for exactly that list:

```toml
[process]
argv = ["my-helper"]
calls = ["session_list", "session_wake"]
```

- The enable dialog lists each tool with its effects, so the person approves the list together with
  the program. It is tied to the manifest digest like everything else in `addon.toml`: if the file
  changes, the add-on is off and the list is not in force until it is approved again. Disabling it
  ends the list too.
- Only the add-on's own `csm/call`s use it. An MCP client, the CLI or another add-on calling the
  same tool is treated as before. A tool **not** in the list asks as before.
- A name that is not a tool, an add-on's tool, a tool that turns on extensions (`addon_enable`),
  `addon_*`/`plugin_*` management, `consent_*`, `settings_set`, `state_import`, `run_edit`,
  `open_with_set` or `app_quit` is reported as a problem and **not** granted. Listing a tool never
  lifts a tool's own refusals: a tool that refuses add-on callers still does. At most 64 tools.
- `addon_list` shows the tools in force as `process_calls` (`tool`, `title`, `effects`, `asks`).

### Remotes, session notes, sleep and wake, and secrets

Four hooks let a process add-on connect CSM to a place CSM does not know about (a build farm, a
container host, anything reached over ssh). They are generic: the add-on supplies the knowledge, the
app supplies the session.

**Remotes (`csm/remotes_changed`).** The plugin tells CSM what it can open a session on. The message
is a notification carrying the *complete* current list (an empty list withdraws everything), at most
200 entries; send it again whenever the list changes. The entries show up in the palette (type `>`
or part of the name) as `Remote… › <add-on name> › <label>` and as chips in the New session dialog's
Remote… row. They go away when the add-on stops or is disabled.

```
-> (plugin) {"jsonrpc":"2.0","method":"csm/remotes_changed","params":{"remotes":[
     {"id":"agent-1","label":"Agent 1","detail":"idle, image current",
      "session":{"name":"Agent 1","resume":"rerun",
                 "ssh":{"host":"agent-1","config_file":"/path/to/ssh_config",
                        "remote_argv":["tmux","new-session","-A","-s","main"]}}},
     {"id":"new","label":"New agent…","tool":"my-addon.open_agent","params":{"template":"default"}}]}}
```

- `id`: letters, digits and `. _ : -`, up to 64 bytes, unique in the list. `label` (80 characters)
  is required; `detail` (160) is optional.
- Exactly one of `session` and `tool`. `session` is the parameter object of `session_new` (an ssh
  session with a config file and alias, usually) and is opened as given. `tool` names one of **this
  add-on's own** tools (`<add-on id>.<name>`), called with `params`; its answer must carry
  `data.session`, the same object, which is then opened. Each is at most 8 KiB. A bad entry is
  dropped and logged; the rest still load.
- Opening goes through the tool **`remote_open`** (`addon`, `remote_id`, optional `focus`): effects
  state, launch and network, so an MCP client is asked first. `remote_list` (read) lists the
  entries. Both work from the palette, the CLI (`csm remote-open`) and MCP.
- The session is **owned** by the add-on: it is stored as `OwnerAddon` in `state.json` (an id, not a
  secret) and survives a restart of the app and of the plugin. A session the plugin opens itself with
  `csm/call` `session_new` is owned the same way.

**SSH sessions from a config file.** `session_new` and `session_edit` take, under `ssh`:
`config_file` (`ssh -F <file>`; `host` may then be an alias defined in it, and `user` and `port`
can stay empty) and `remote_argv` (the command to run on the remote host as an argv list, at most
64 elements of 4096 bytes; for example `["tmux","new-session","-A","-s","main"]`). The remote
command is built as argv: each element is a POSIX word, left bare when it only has safe characters
and single-quoted otherwise, joined into one remote command line, and that whole line is one
argument to `ssh` (the quoting rules of `docs/wpf/lessons.md`, "Two escaping layers"). A value can
never become another `ssh` option. Control characters are refused.

**Resume by re-running (`resume`).** `session_new` and `session_edit` take `resume: "rerun"`. A
session marked this way is started again with exactly the same command line on restore, wake and
restart: no Claude `--resume` or `--session-id` is added, and it comes back even when its add-on
profile says `resume = "none"`. It is for programs that reattach by themselves, such as
`tmux new-session -A`. A pack profile's `resume = "re-run"` means the same for sessions started
from that profile.

**Status and note (`session_annotate`).** Through `csm/call`, a plugin may set a status override
and a short note on **sessions it owns** (and only those; other callers, the person and MCP clients
are refused):

```
csm/call {"tool":"session_annotate","params":{"session_id":"...","status":"working","annotation":"running remotely"}}
```

`status` is `working`, `needs_you_approval`, `needs_you_question`, `done_unseen`, `error`, `idle`, or
`""` to clear; `annotation` is up to 80 characters or `""`. Leave a field out to keep it. The status
outranks what the screen suggests, except that a process that has ended stays `exited` (and a
sleeping session stays `asleep`). The note appears after the session's subtitle in the sidebar.
Both are runtime only and are cleared when the plugin's process stops, crashes or is disabled; set
them again after the handshake.

**Sleep and wake.** A plugin that subscribes to `session_asleep` and `session_awake` is sent
`{"method":"event","params":{"event":"session_asleep","session_id":"..."}}` when a session it owns
goes to sleep or is woken, and for no other session. A session that sleeps closes its local process
(for an ssh session, the connection); what runs on the far side is the plugin's to track.

**Secrets (`addon_secret_set`, `addon_secret_get`, `addon_secret_delete`).** An add-on that declares
the effect `stores_secrets` may keep small secrets in the OS keychain (Windows Credential Manager,
macOS Keychain, the Secret Service on Linux), through `csm/call`:

```
csm/call {"tool":"addon_secret_set","params":{"key":"ssh-key.path","value":"..."}}
csm/call {"tool":"addon_secret_get","params":{"key":"ssh-key.path"}}   -> data: {"key":..,"value":".."|null}
csm/call {"tool":"addon_secret_delete","params":{"key":"ssh-key.path"}} -> data: {"removed": true|false}
```

- `key`: lowercase letters, digits and `. _ -`, up to 64 bytes; `value` up to 2048 bytes.
- The keychain service is `codeshell-addon:<add-on id>`, taken from **who is calling**, never from a
  parameter: an add-on cannot name another's secrets. The person, the CLI and MCP clients are refused
  (a secret goes from the keychain to the add-on that stored it and nowhere else). Nothing is written
  to `state.json`, and the value is in the result's `data`, never in its message.
- The effect `stores_secrets` is shown in the consent dialog when the add-on is enabled; that is
  the consent, so these calls do not ask again. The calls run off the engine thread (a locked
  keychain can wait), and fail with the keychain's reason if there is none.
- Secrets stay in the keychain when the add-on is disabled or removed; delete them first if that
  matters.

### Effects and consent

Each tool's effects map onto the app's effects, and **consent for MCP callers is derived from
them**: a tool with `writes_filesystem` asks the person before an MCP client runs it, like any
built-in tool with that effect. A tool that declares `"effects": []` uses none and runs without
asking.

| add-on effect | tool effect | MCP consent |
|---|---|---|
| `launches_process` | launches a process | yes |
| `sends_input` | writes into sessions | yes |
| `reads_session_content` | returns session content | yes |
| `writes_filesystem` | writes files | yes |
| `accesses_network` | talks to the network | no (open-world hint only) |
| `stores_secrets` | keeps secrets in the OS keychain | no: only the add-on itself can call them |
| `stores_data` | writes only inside its own `data_dir` | no: counted as the app's own state, like `task_new` |

`stores_data` means the add-on writes only inside its own data folder. It is shown in the enable
dialog ("Stores its own data"), asks no one, and is only as honest as the plugin, like every effect.

A tool that **omits** `effects` is given all of the add-on's declared effects, so it asks before an
MCP client runs it whenever the add-on declares any. Declaring a narrower list (or `[]`) is the
plugin's choice, and it is only as honest as the plugin. The add-on as a whole is approved by the
person, with its program and its declared effects shown in the consent dialog.

A `csm/call` from a plugin runs as `add-on <id>`. An MCP client may not claim a name starting with
`add-on ` (refused at the bridge), so only the add-on's own calls carry its grants.

### Failures and restarts

- A process that **exits**, writes a line that is **not JSON** or not a JSON-RPC message, writes a
  line **over 1 MiB**, or does **not answer `initialize` within 10 seconds** is stopped. Its tools
  are removed, and any call waiting on it fails with "stopped before it answered".
- A stopped process starts again after a backoff of 1, 2, 4, 8 and 16 seconds (capped at 30). After
  **5 failures in a row** it is left alone (state `gave_up`) until `plugin_restart`. A run that lasted
  60 seconds before it failed starts the count again.
- `plugin_status` shows, per enabled process add-on: `state` (`starting`, `running`, `crashed`,
  `gave_up`, `stopped`), `pid`, `restarts`, `last_error`, its `tools`, the handshake `problems`, and
  the stderr tail. Settings > Add-ons shows the state, the last error and a **Restart** button.

### Tools

| id | effects | what |
|---|---|---|
| `plugin_status` | reads session content: asks for MCP clients, since it returns the stderr tail | the table above |
| `plugin_restart` | launches a process, may discard data | `id`: stop and start an enabled process add-on |
| `remote_list` | read | the remotes enabled process add-ons offer |
| `remote_open` | state, launch, network (consent for MCP) | `addon`, `remote_id`, optional `focus` |
| `session_annotate` | ui | add-on callers only: `session_id`, `status`, `annotation` on a session the add-on owns |
| `addon_secret_set` / `_get` / `_delete` | secrets | add-on callers only: `key` (and `value`) in the add-on's own keychain namespace |
| `screen_list`, `screen_open`, `screen_close`, `screen_pop_out`, `screen_dock` | read / ui | the add-on screens (see Screens below) |

While its process runs, the add-on's own tools (`echo-test.echo` and so on) appear in `tool_list`,
the palette, the CLI (`csm echo-test.echo`) and MCP, in the `addons` toolset. They cannot be
palette commands: a command names a built-in tool.

### Test plugin

`crates/csm-engine/tests/support/echo_plugin.rs` (built as `csm-echo-plugin`, not shipped) is a
complete example in Rust. `crates/csm-engine/tests/plugins.rs` runs it through the engine: the
handshake, a call, consent, `csm/call`, events, a crash and restart, disabling, the three
misbehaviour caps, and shutdown. `crates/csm-engine/tests/screens.rs` drives its screen (open, render,
event, patch, refresh, an over-cap tree, a stop and a restart, close) through the engine.

### Sample plugin: Session summary

`apps/csm-session-summary` is a small shipped process add-on in Rust (no runtime to install). It
does the handshake, declares `session-summary.summarise` (input `include_asleep`, default false),
and on a call runs `session_list` through `csm/call` and answers with a one-paragraph `message`
(counts by status, the sessions that need you, repos in play) and a `data` object with the full
detail (`total`, `by_status`, `needs_you`, `by_repo`). `session_list` is read-only and returns no
terminal text, so the add-on and its tool both declare `effects = []`: an MCP client runs it with
no consent card. Its `argv` is the bare name `csm-session-summary`; the plugin host finds it beside
`codeshell.exe` (and on PATH, where the installer puts the app's folder). Test:
`apps/csm-session-summary/tests/summary.rs` runs it through the real host.

### Not in this phase

The code and diff widgets, screens as panes in the layouts, signed add-ons, a registry, a version check or update for a process add-on,
and a process add-on's own profiles or palette commands. Protocol `api` 1 is the only version.


## Screens

A process add-on can show **its own screens** inside CSM. It declares them in its `initialize`
answer; when the person opens one, CSM asks the plugin for a **view tree** (JSON), validates it off
the UI thread, and draws it with CSM's own widgets and theme tokens inside a frame CSM draws. The
plugin never draws pixels, never sends escape sequences, never picks colours, sizes or fonts, and
never opens windows. Check `capabilities.screens` (version 1) in `initialize` before declaring one.

This is the first part of the screens work: the protocol, the tools and the basic widgets. The code
and diff widgets, syntax highlighting, comment threads and screens as panes inside the layouts are
later work; a plugin that sends `code` or `diff` today gets a small "unsupported" box.

### Declaring and opening

```json
<- {"jsonrpc":"2.0","id":1,"result":{"api":1,"name":"Review","tools":[...],
    "screens":[{"id":"review.changes","title":"Review","icon":"layers","scope":"project",
                "summary":"Changed files and diffs"}]}}
```

- `id` is `<add-on id>.<name>` (same rules as a tool id). `title` is required (64 characters).
  `icon` names an icon from CSM's own set (an unknown one draws none). `scope` is `project`,
  `session` or `global` (the default): it decides which context the screen needs when it is opened.
- At most 4 screens. A bad entry is dropped and listed in `problems` (`plugin_status`).
- The screens are offered while the process runs, in the palette ("Open <title>") and in
  `screen_list`. They go away when it stops.

CSM sends `screen/open` as a **request**; the plugin has 10 seconds to answer with
`{"rev": 1, "root": {...}}`. Until then the tab shows a spinner; after that, "<add-on> did not
answer" with **Retry**.

```
-> {"jsonrpc":"2.0","id":12,"method":"screen/open",
    "params":{"screen":"review.changes","instance":"i7",
              "context":{"session_id":"s3","project":"repo:C:/x","folder":"C:/x","path":null,"args":null},
              "theme":"dark"}}
<- {"jsonrpc":"2.0","id":12,"result":{"rev":1,"root":{"type":"column","children":[...]}}}
```

`instance` names this open screen in every later message. When it is closed CSM sends the
notification `screen/closed {instance}`.

### The tools (toolset `addons`)

| id | effects | what |
|---|---|---|
| `screen_list` | read | the screens running add-ons offer, and the open instances (phase, rev, error, own window) |
| `screen_open` | ui | `screen`, optional `context` (`session_id`, `project`, `path`, `args` up to 4 KiB), `focus`, `theme`; returns `instance`. The same screen on the same context is focused, not opened twice. A screen that did not answer is asked again. At most 8 instances per add-on |
| `screen_close` | ui | `instance`; the plugin gets `screen/closed` |
| `screen_pop_out`, `screen_dock` | ui | move an instance into its own window, or back into the tab strip |

They change only what the app shows, so an MCP agent may call them without a consent card ("I
opened the review for you"), like `session_focus`. A `session_id` gives the screen the session's
project (the key tasks use) and working folder. Project screens need a project; session screens need
a session.

### Updates: render, patch, refresh, error

The plugin updates a screen with notifications. `rev` increases strictly (a plugin restart starts
again at 1):

```
<- {"jsonrpc":"2.0","method":"screen/render","params":{"instance":"i7","rev":5,"root":{...}}}
<- {"jsonrpc":"2.0","method":"screen/patch","params":{"instance":"i7","rev":6,"base":5,
     "ops":[{"op":"replace","key":"status","node":{...}},
            {"op":"remove","key":"draft-3"},
            {"op":"append","key":"comments","node":{...}}]}}
```

- A render or patch whose `rev` is not newer than what is shown is dropped.
- A patch applies only if `base` is the revision shown. Otherwise it is dropped and CSM sends the
  notification `screen/refresh {instance}`; the plugin answers with a full `screen/render`. So the
  protocol heals itself without CSM tracking diffs.
- `replace` swaps the node with that `key`; `remove` takes a child out of a `column`, `row`,
  `toolbar` or `section` (never one of a `split`'s two, or a `scroll`'s or `tabs`' child: replace
  those); `append` adds a child to one of those. At most 256 ops. `set` (prop-level patches) is not
  built yet.
- A tree or patch that breaks a cap or is malformed is **refused as a whole**: the previous tree
  stays on screen, the frame shows "<add-on> sent a view CSM could not show: <reason>", and the plugin
  gets the notification `screen/error {instance, message}`. Nothing panics.
- Renders are published to the window at most 30 times a second per instance, latest wins; every
  render is still applied to the latest tree.
- A text input keeps what the person is typing in CSM, keyed by `(instance, key)`. A re-render does
  not overwrite it, unless the node carries `reset: true` (send it after a successful submit).

### Events and buttons

Widgets send events to the plugin as notifications; the plugin answers with a render or patch.

```
-> {"jsonrpc":"2.0","method":"screen/event",
    "params":{"instance":"i7","rev":6,"key":"files","kind":"select","value":{"path":"src/lib.rs"}}}
```

`rev` is the revision the person saw. The kinds and their `value`:

| kind | from | value |
|---|---|---|
| `click` | a `button` with `"event": true` | null |
| `select` | `list`, `file_list`, `select`, `segmented`, `tabs` | `{"key"}` (list, tabs), `{"path"}` (file_list), `{"value"}` (select, segmented) |
| `toggle` | `checkbox` | `{"checked": bool}` |
| `submit` | `text_input` (Enter, or its `submit_label` button) | `{"value": text}` |
| `change` | `text_input` with `on_change`, 300 ms after the last keystroke | `{"value": text}` |
| `link` | a `csm:` link in Markdown text | `{"href": "csm:..."}` |

(`range_action`, `thread_toggle` and `visible` are reserved for the code and diff widgets.)
Events are queued without waiting: a full queue (64 messages per add-on) keeps the latest `change`
and refuses others with a toast ("<add-on> is busy").

**Buttons bound to tools.** A `button` with `"action": {"tool": ..., "params": {...}}` runs that tool
through the normal dispatch as a click of the person, so a button does nothing an agent could not do
with the same tool. Which tools is decided when the tree is parsed, by the tool id:

- The add-on's own tools (`<its id>.*`) run directly.
- `session_focus`, `screen_open`, `screen_close` and `inspector_show` run directly.
- `session_send_text`, `open_url` and `open_with` run only after **a confirm CSM draws**: it names
  the add-on, the tool, the exact text or address and the target session (invisible characters shown
  as markers). The button's label is the plugin's, so the confirm is where the truth is shown.
- Everything else (`addon_*`, `plugin_*`, `consent_*`, `settings_set`, anything that enables an
  extension or may discard data, another add-on's tools) is refused: the button draws disabled, with
  the reason as its tooltip.

### The widgets

A node is `{"type": ..., "key"?: string, ...props}`. A `key` (1 to 64 characters, no control
characters, unique in the tree) is required on every node that sends events. An unknown `type` draws
as a small "unsupported: <type>" box (its children are ignored), so a newer plugin degrades on an
older CSM; unknown props are ignored. A prop of the wrong type falls back to its default; a missing
required child, a duplicate key or a cap breach refuses the tree. `tone` is
`default | muted | info | success | warning | danger | accent`; `gap` is `none | xs | sm | md | lg`.

| type | props |
|---|---|
| `column`, `row` | `children`, `gap`, `align` (`start`, `center`, `end`, `stretch`) |
| `split` | `children` (exactly two), `ratio` (0.15 to 0.85), `min_px`: a draggable divider; CSM remembers the ratio per instance (needs a `key`) |
| `scroll` | `child` |
| `tabs` | `key`, `tabs: [{key, label, badge?}]`, `selected`, `child` (the plugin renders the selected tab's content) |
| `section` | `title`, `badge?`, `collapsible?` (needs a `key`), `collapsed?`, `children` |
| `divider`, `spacer`, `toolbar` | `toolbar`: `children`, a right-aligned strip |
| `text` | `text`, `style` (`body`, `caption`, `title`, `mono`), `tone`, `wrap` |
| `markdown` | `source` (up to 64 KiB): paragraphs, headings, emphasis, code spans and fenced code, lists, quotes, rules, links. No images (the alt text shows), no HTML (shown as text), no tables |
| `badge` | `text`, `tone` |
| `icon` | `name` (CSM's set), `tone` |
| `kv` | `rows: [[key, value]]` (up to 500) |
| `empty` | `title`, `body?`, `icon?` |
| `progress` | `value?` (0 to 1; absent is a spinner), `label?` |
| `comment` | `author: {name, kind: "person"\|"agent"}`, `time_ms`, `body` (Markdown), `badges?`, `actions?` (buttons). `kind: agent` gets CSM's agent mark |
| `button` | `label`, `kind` (`primary`, `secondary`, `ghost`, `danger`), `icon?`, `disabled?`, and either `action` or `"event": true` |
| `text_input` | `key`, `value`, `placeholder`, `multiline?`, `max_len` (up to 16 KiB), `submit_label?`, `on_change?`, `reset?` |
| `checkbox` | `key`, `label`, `checked` |
| `select`, `segmented` | `key`, `options: [{value, label}]` (up to 200), `selected` |
| `list` | `key`, `items: [{key, title, subtitle?, icon?, badges?}]` (up to 20,000, virtualised), `selected?` |
| `file_list` | `key`, `files: [{path, status?: M A D R C U ?, old_path?, badges?}]` (up to 20,000), `view: "tree"` (default) or `"flat"`, `selected?`: CSM builds the folder tree from the paths; folders fold locally |

**Caps** (checked before anything is kept; a breach refuses the whole tree):

| limit | value |
|---|---|
| a message | 1 MiB (as for every message) |
| a tree, serialized | 768 KiB |
| nodes | 5,000 |
| depth | 32 |
| one plain string | 16 KiB; a label is cut at 200 characters |
| Markdown source | 64 KiB (2,000 blocks, 8 levels of lists and quotes) |
| operations in a patch | 256 |
| screens declared per add-on | 4 |
| open instances per add-on | 8 |

### What the person sees, and what a screen cannot do

- **The tab strip.** With a screen open, a strip of views appears under the group bar: `Sessions`
  and a tab per open screen (its icon, title, an "Add-on" mark and a close x). **Pop out** (in the
  frame's header) moves a screen into its own window, using the same viewport machinery as the
  tear-offs; its tab stays, and clicking it docks it back. Closing the window docks it.
- **The frame.** Every screen sits inside a header CSM draws (the screen's title, an "Add-on" badge
  and the add-on's name and id) and a 1 px border, and is clipped to the area below it, so a view
  can neither cover nor imitate it. A click with no new render after 300 ms shows a small spinner in
  the header.
- **Placeholders.** Opening (spinner), "did not answer" (Retry, Close), "is not running" (Restart,
  Close). When the process stops, the last tree stays on screen, greyed out and disabled, under a
  bar; when it is running again the open instances are asked to open again, on their old context.
  Disabling the add-on closes its screens.
- **Untrusted text.** Labels drop control, bidi and zero-width characters. Text, comments and
  Markdown keep them but draw each one as a visible marker (an ESC symbol, `⟨U+202E⟩`, in a warning
  tint): a review screen shows a Trojan-Source trick instead of hiding or obeying it. A text a person
  types goes only to the add-on that owns the screen.
- **Links.** Only absolute `http` and `https` links are live (the tooltip shows the real address; a
  click runs `open_url` as the person's click; text that names a different URL shows both). `csm:`
  links become `link` events. Any other scheme is plain text.
- **Above everything.** Screens draw in the main panel, below menus, dialogs, toasts and consent
  cards (which name the add-on and are `Foreground` areas). A screen in its own window is a separate
  OS window: a consent card appears in the main window.

## Example

`docs/addons/examples/generic-agent/addon.toml` is a complete pack: a profile for a generic CLI agent
with status rules, a run template, a slash preset and a palette command. Copy the folder into
`<config dir>/addons/`, then enable it in Settings > Add-ons. `crates/csm-core/tests/example_pack.rs`
parses it with no problems.

## Samples that ship with the app

`samples/addons/` holds three real add-ons, staged by the release workflow into
`<install dir>\samples\addons\` (MSI and portable zip alike):

| id | kind | what |
|---|---|---|
| `codex-cli` | pack | profiles `agent` (`codex`) and `resume-last` (`codex resume --last`), status rules, `/review` `/diff` `/compact`, palette commands to start either |
| `gemini-cli` | pack | profiles `agent` (`gemini`) and `resume-latest` (`gemini --resume`), status rules, `/compress` `/stats` `/memory`, palette commands to start either |
| `session-summary` | process | the Session summary plugin above |

The MSI installs per machine and add-ons live per user, so nothing is enabled or copied at install
time. **Settings > Add-ons > Install samples** (`addon_install_samples`) copies each sample folder
that is not already in the add-ons folder; an existing folder (yours, edited, enabled) is never
touched. They appear **disabled**; **Enable...** shows what each adds and asks, as for any add-on.
The profile resume is `re-run`: CSM does not know Codex's or Gemini's session ids, so restore
starts a new conversation and the `resume-*` profiles continue the last one. The status rules match
the TUIs' wording as last seen and may need adjusting after a release of either tool (see the
comments in each file). `crates/csm-core/tests/sample_packs.rs` parses all three with no problems.

## Limits

- Profile resume templates (`{session_id}`) are validated but not applied.
- Run templates with a working folder are refused (run commands have none).
- No registry, signing or update check: add-ons are trusted on enable, with the consent above.
- The quick-open offers an enabled add-on's profiles as "Add-on: name" chips in the Profile row (new sessions only); `session_new` with `profile` does the same for MCP and CLI callers.
- Runs shows "<name> (add-on)" buttons for each enabled add-on's run templates (`addon_run_apply`).
- A status rule reads the screen about once a second; a short-lived match can be missed.
- A profile's status rules stop applying the moment its add-on is off; the last status it set is
  cleared on the next tick.
- Run templates run in a shell, as any Runs command does; a `cwd` other than `{folder}` is refused.
- A symlink to another file inside an add-on folder is not followed for the manifest; other files are ignored. Windows junctions are not detected as links.
- A process add-on's restarts are not rate-limited by hand: `plugin_restart` (and the Restart button) starts it at once. Only crashes back off.
- Distinct tool id, title and summary texts of process add-ons are kept for the life of the app, up to 4096 in all. A tool past that is not registered (logged), so a plugin that changes its text on every start uses up that room and then loses its new tools.
- A process add-on's tool calls are not cancelled when its process stops; they fail with "stopped before it answered".
- No signing, registry or version check for process add-ons; consent is per `addon.toml` digest, as for packs.
