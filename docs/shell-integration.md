# CodeShellManager Shell Integration

Programs running inside a CodeShellManager terminal can push session state up to the host UI by emitting a custom OSC (Operating System Command) escape sequence. This is the recommended way for tools like SSH overlays, REPLs, and TUI apps to keep CSM's accent color, git status, and tab title in sync with whatever the program actually represents — even when CSM cannot inspect that state locally.

## Wire format

```
ESC ] 9001 ; key=value ; key=value … ST
```

- `ESC` is `\x1b` (`0o33`, `27`).
- `9001` is the CSM-namespaced OSC identifier.
- `ST` ("string terminator") is either `BEL` (`\x07`) or `ESC \` (`\x1b\x5c`). Both are accepted.
- Keys and values are separated by `=`. Multiple fields are separated by `;`.
- Whitespace around keys/values is trimmed.
- Unknown keys are silently ignored — safe to emit forward-compatibly.
- The whole sequence is consumed by xterm and never rendered.

## Recognised keys

| Key          | Value format            | Effect |
|--------------|-------------------------|--------|
| `color`      | `#rgb`, `#rrggbb`, `#rrggbbaa` | Override the session accent. Repaints the sidebar stripe and the active-pane ring immediately. |
| `git-branch` | string                  | Set the branch label shown in the sidebar. Bypasses CSM's local `git` polling — useful for SSH/remote sessions. |
| `git-dirty`  | `0`/`1` (or `false`/`true`) | Toggle the dirty marker (`*`) shown next to the branch. |
| `title`      | string                  | Rename the session (same as double-clicking the sidebar entry). Persisted to `state.json`. |

Multiple keys can be sent in a single sequence; CSM applies them atomically and saves state once.

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

**Update on every prompt.** Cheap, predictable, and handles `cd` / branch switches automatically. Use the shell snippets above.

**Update on relevant events only.** If a prompt-hook is too coarse — e.g. inside a long-running TUI like `nexus` — call your update function whenever your internal state changes (new repo selected, dirty state changes, branch checked out, etc.).

**Reset on exit.** If your program owns the session's accent for its lifetime, restore the default before exiting:

```bash
# Clearing color sends the empty string, which CSM treats as "use the default hash"
# (only true if you've also chosen to clear ColorOverride; currently CSM keeps the
# last value. To restore the original hash, leave the color key out entirely.)
```

In the current build, an emitted `color=` is sticky and persists in `state.json` across restarts. If you want it to revert when your program exits, emit nothing extra — but if a different program later runs in the same session, it will inherit your color until it sets its own.

## Limitations

- The protocol is one-way: CSM does not respond to OSC 9001 sequences with any data.
- There's no acknowledgement that a sequence was parsed. Validate your output with the inspector if you want to be sure (DevTools is enabled in WebView2; press `F12` inside a terminal pane).
- Color values must be valid CSS hex (`#rgb` / `#rrggbb` / `#rrggbbaa`). Named colors and `rgb()` syntax are rejected.
- The terminating byte should be `BEL` or `ESC \`. xterm.js will eventually time out an unterminated OSC, but until then your text appears swallowed.

## Pipeline (for CSM contributors)

`terminal-init.js` registers the OSC handler via `term.parser.registerOscHandler(9001, …)`. The handler parses the payload, posts `{type: "shellIntegration", fields: {…}}` over the WebView2 message channel, and returns `true` so xterm consumes the sequence. `TerminalBridge.OnWebMessageReceived` raises `ShellIntegrationReceived`. `MainWindow.LaunchSessionAsync` subscribes and dispatches to `SessionViewModel.ApplyShellIntegration(fields)`, then triggers `SaveStateAsync`. Color/title changes propagate through `INotifyPropertyChanged` to repaint the sidebar stripe and active ring; git fields update `GitBranch` / `GitIsDirty`.
