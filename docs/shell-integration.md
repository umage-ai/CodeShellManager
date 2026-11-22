# Shell integration (OSC 9001 and OSC 7501)

Programs running inside a CodeShellManager session can push state up to the app with escape
sequences. Use this to keep a session's accent colour, branch and title in step with what the
program actually represents, for example an SSH overlay or a REPL, where CodeShellManager cannot
look at the state itself.

> **Reporting working / waiting / done?** Use the standard **OSC 7501 Program Status Protocol**
> ([spec](https://www.superlogical.com/rex/docs/build/program-status)) rather than OSC 9001. CSM
> maps reports onto the session's status: `working` (with `progress`) shows as *Working · 40%*;
> `blocked` with `kind=permission` as *Needs you* (approval); any other `blocked` as *Needs you*
> (question); a record entering `done` or `error` as *Done* or *Error* until you look, with a
> notification; `idle` / `clear` clear it. While a session holds any 7501 record, CSM's own screen
> detection stands down for it (*Clear status* in the session's menu hands it back). Status is
> runtime-only and never saved. Claude Code reports through OSC 7501 out of the box.
>
> **Feature query.** CSM answers `OSC 7501 ; ? ST` on its bundled ConPTY (the default on Windows)
> and on macOS and Linux PTYs. On the Windows inbox ConPTY it does not, because conhost answers the
> query that programs send right after it first. The spec makes detection optional: a program
> that reports without asking first works everywhere.

## OSC 9001 wire format

```
ESC ] 9001 ; key=value ; key=value … ST
```

- `ESC` is `\x1b` (27).
- `9001` is the CSM-namespaced OSC identifier.
- `ST` (string terminator) is either `BEL` (`\x07`) or `ESC \` (`\x1b\x5c`). Both are accepted.
- Keys and values are separated by `=`; fields by `;`. Whitespace around keys and values is
  trimmed.
- Unknown keys are ignored, so it is safe to send keys a later version may understand.
- The sequence is consumed by the terminal and never drawn.

## Recognised keys

| Key | Value | Effect |
|---|---|---|
| `color` | `#rgb`, `#rrggbb`, `#rrggbbaa` (alpha last) | The session's accent colour: the sidebar stripe and the pane's accent. Saved with the session. |
| `git-branch` | string | The branch shown on the session. Takes over from CSM's own git polling, which is useful for SSH and other remote sessions. |
| `git-dirty` | `1` / `true`, anything else is clean | The "uncommitted changes" marker next to the branch. |
| `title` | string | Renames the session (as F2 does). Saved with the session. |

Several keys can be sent in one sequence; the session is saved once.

## Examples

All examples below emit `color=#a6e3a1`, `git-branch=feat/foo`, `git-dirty=1`, `title=my-repo` in a single sequence. Adapt to your needs.

### bash / zsh / sh

```bash
printf '\e]9001;color=#a6e3a1;git-branch=feat/foo;git-dirty=1;title=my-repo\e\\'
```

To refresh on every prompt, drop this into your shell init:

```bash
__csm_update() {
  local branch dirty
  branch=$(git symbolic-ref --short HEAD 2>/dev/null) || branch=""
  [ -n "$(git status --porcelain 2>/dev/null)" ] && dirty=1 || dirty=0
  printf '\e]9001;git-branch=%s;git-dirty=%s\e\\' "$branch" "$dirty"
}
PROMPT_COMMAND='__csm_update'    # bash
# precmd_functions+=(__csm_update)  # zsh
```

### PowerShell

```powershell
$esc = [char]27
"$esc]9001;color=#a6e3a1;git-branch=feat/foo;git-dirty=1;title=my-repo$esc\" | Write-Host -NoNewline
```

In a `prompt` function:

```powershell
function prompt {
    $esc = [char]27
    $branch = (git symbolic-ref --short HEAD 2>$null)
    $dirty  = if ((git status --porcelain 2>$null)) { 1 } else { 0 }
    Write-Host -NoNewline "$esc]9001;git-branch=$branch;git-dirty=$dirty$esc\"
    "PS $($executionContext.SessionState.Path.CurrentLocation)> "
}
```

### Python

```python
import sys

def csm_update(**fields):
    payload = ";".join(f"{k}={v}" for k, v in fields.items())
    sys.stdout.write(f"\x1b]9001;{payload}\x1b\\")
    sys.stdout.flush()

csm_update(color="#a6e3a1", **{"git-branch": "feat/foo", "git-dirty": "1"}, title="my-repo")
```

### Node.js

```js
function csmUpdate(fields) {
  const payload = Object.entries(fields).map(([k, v]) => `${k}=${v}`).join(';');
  process.stdout.write(`\x1b]9001;${payload}\x1b\\`);
}

csmUpdate({ color: '#a6e3a1', 'git-branch': 'feat/foo', 'git-dirty': '1', title: 'my-repo' });
```

### Rust

```rust
fn csm_update(fields: &[(&str, &str)]) {
    let payload: String = fields.iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(";");
    print!("\x1b]9001;{payload}\x1b\\");
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

csm_update(&[
    ("color", "#a6e3a1"),
    ("git-branch", "feat/foo"),
    ("git-dirty", "1"),
    ("title", "my-repo"),
]);
```

### Go

```go
package main

import (
    "fmt"
    "strings"
)

func csmUpdate(fields map[string]string) {
    parts := make([]string, 0, len(fields))
    for k, v := range fields {
        parts = append(parts, k+"="+v)
    }
    fmt.Printf("\x1b]9001;%s\x1b\\", strings.Join(parts, ";"))
}
```

## Patterns

**Update on every prompt.** Cheap, predictable, and follows `cd` and branch switches by itself.
Use the shell snippets above.

**Update on relevant events only.** If a prompt hook is too coarse, for example inside a
long-running TUI, send the update whenever your own state changes (another repository, dirty
state changed, branch checked out).

**Colour is sticky.** A `color=` is stored on the session and survives sleep, wake and restarts.
There is no reset on the wire: an empty or invalid value is ignored. A program that runs later in
the same session keeps that colour until it sets its own. *Reset accent colour* in the session's
menu (or the `session_accent_reset` tool) hands it back to the default.

## Limitations

- One way only: CSM never answers an OSC 9001 sequence.
- No acknowledgement that a sequence was parsed.
- Colours must be hex (`#rgb`, `#rrggbb`, `#rrggbbaa`). Named colours and `rgb()` are rejected.
- **Values cannot contain `;`**: it separates fields and there is no escaping. `title=a;b` is read
  as `title=a` plus an unknown key `b`. `=` inside a value is fine (only the first `=` splits).
- **Titles are capped at 80 characters** and branches at 200. Control characters are stripped and
  whitespace trimmed; a title that is empty after that is ignored and the existing name kept.
- Every value is treated as untrusted: anything printing to the terminal (a remote host, `cat` of
  a file) can send these sequences, so CSM validates and caps them and never acts on them beyond
  the table above.

## For contributors

The sequence is read by the OSC tap in `crates/csm-term` (a `vte` parser in front of the
emulator), validated in `crates/csm-core/src/shell_integration.rs`, and applied to the session by
the engine (`crates/csm-engine/src/lifecycle.rs`). OSC 7501 parsing and the probe reply are in
`crates/csm-term`.
