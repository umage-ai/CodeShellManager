# Phase 0 terminal spike: how to run it, what was measured, what a human must check

Branch `feature/phase0-spike`. Gate definition: `docs/plan.md` section 5, "Phase 0".

## Layout

| Path | Survives into the product? | What it is |
|---|---|---|
| `crates/csm-pty` | yes | `Pty` trait. Windows: own ConPTY binding (bundled `conpty.dll` by full path, inbox `kernel32` by full path, per-HPCON API, 3-failure demotion), Job Object tree kill, PATH-only program lookup, MSVCRT argv quoting, early-output buffer, ordered writer thread. Unix: portable-pty + `killpg`. The only `unsafe` in the workspace is `src/conpty.rs`. |
| `crates/csm-term` | yes | `alacritty_terminal` + a `vte` OSC tap (9001, 7501 incl. `?`, 133, 7) that stops right after each sequence it acts on, so replies stay in stream order. Answers XTVERSION. Input encoding (keys, mouse, focus, bracketed paste as a separate `Untrusted` type). No egui. |
| `apps/term-spike` | renderer is the prototype for `csm-ui`; the app is throwaway | egui 0.36 / eframe wgpu. Own renderer: swash shaping (`calt`/`liga`) + rasterization into a shelf-packed RGBA atlas; one textured mesh per pane, rebuilt only when the terminal's generation or the view changes. Box drawing and block elements as rectangles. |
| `apps/term-spike/src/bin/spike-gen.rs` | no | streaming output generator for the stress test |
| `apps/term-spike/src/bin/probe-check.rs` | no | headless: runs a program on csm-pty + csm-term exactly like a pane and prints what the terminal saw |
| `scripts/get-conpty.ps1` | yes | fetches the bundled ConPTY (NuGet, Authenticode-checked) into `.conpty/<arch>/` (gitignored) |

## Running it

```powershell
# once: put conpty.dll + OpenConsole.exe in .conpty\x64\ (never committed)
pwsh scripts/get-conpty.ps1                # from nuget.org, signature-checked
# or copy from the WPF build output:
pwsh scripts/get-conpty.ps1 -From ..\..\CodeShellManager\src\CodeShellManager\bin\Release\net10.0-windows

cargo build --release -p term-spike --bins  # build.rs copies both files beside the exes
.\target\release\term-spike.exe            # 4 pwsh panes + 1 floating + 1 torn-off window
```

`CSM_CONPTY_DIR=<folder>` overrides where `build.rs` copies them from. Without them the spike
runs on the inbox ConPTY and logs `PTY ConPTY: inbox — conpty.dll not found in …`. The log
goes to `term-spike.log` in the current folder (`--log PATH` to change it).

| Command | What it does |
|---|---|
| `term-spike` | 4 grid panes running `pwsh -NoLogo` (cmd if no pwsh), plus a floating and a torn-off pane |
| `term-spike --panes 20 --stress` | 20 grid panes streaming `spike-gen` at 2000 lines/s each |
| `term-spike --panes 20 --stress --rate 0 --no-extra` | unthrottled output |
| `term-spike --bench 30 …` | run 30 s, log a frame summary of the second half plus one line per pane, exit |
| `term-spike -- claude --debug-file C:\tmp\claude.log` | grid panes run `claude` (argv, no shell) |
| `term-spike --inbox` / `--no-ligatures` / `--font PATH` / `--font-size 14` / `--no-vsync` / `--spin` | variants; `--spin` repaints continuously (control experiment) |
| `probe-check --secs 15 --cwd DIR -- claude --debug-file FILE` | headless 7501 check |

`CSM_LOG_REPAINT_CAUSES=1` logs why each frame happened; `CSM_TRACE_BYTES=1` makes
`probe-check` print every raw PTY chunk.

## Gate checklist (measured 2026-11-19, Windows 11 26300, over RDP)

**The machine was reached over RDP with no GPU exposed to the session:** wgpu found no Vulkan
driver, GL 1.1 only, and fell back to DX12 on the WARP software rasterizer ("Microsoft Basic
Render Driver"). Every fps number below is capped by that, as the control experiment shows.

| Gate item | Result | Evidence |
|---|---|---|
| builds; `cargo test` passes | **pass** | `cargo test --workspace`: 52 tests, 0 failures. `cargo check -p csm-pty -p csm-term --target x86_64-unknown-linux-gnu` passes (not run on Linux). |
| clippy | **pass** | `cargo clippy --workspace --all-targets`: 0 warnings. `clippy.toml` allows unwrap/expect/panic in tests only. |
| PTY on bundled ConPTY, logs which and why | **pass** | `PTY ConPTY: bundled (found conpty.dll + OpenConsole.exe in …\target\release; loaded …\target\release\conpty.dll)`; `--inbox` logs `inbox — disabled by setting`. First output 6–11 ms after spawn. Unit tests: relative DLL path refused, both files required, 3-failure demotion. |
| real `claude` runs; 7501 probe | **pass** | Claude 2.1.327, run directly by argv (no pwsh wrapper needed), in a scratch folder, left at the trust dialog and exited with Ctrl+C. Debug log on bundled: `programStatus=yes (probe: OSC 7501 reply)`, and Claude then reported `state=blocked:app=claude-code:kind=permission:msg=…` ("trust this folder"). Same check inside a GUI pane: same result. On inbox: `programStatus=no (probe: no reply to OSC 7501 ; ?)`, matching the WPF app's measurements. |
| (bonus) synchronized output | **pass** | After adding an XTVERSION reply: `synchronizedOutput=yes (probe: DECRPM 2026 status=2)`. Before it Claude never asked. |
| OSC 9001 / 7501 / 133 / 7 parsed | **pass** | `crates/csm-term/tests/osc.rs`: byte fixtures fed whole, at every two-chunk split and byte by byte; BEL and ST; probe reply ordered before a following DA1 and after a preceding one, including inside a DEC 2026 synchronized update; colour queries answered in order; oversized/invalid payloads; a randomised no-panic run. |
| first PTY size = measured grid | **pass** | No placeholder anywhere: the PTY is spawned from the first frame's measured grid. Log: `pane 0 '#0 claude': first measured grid 202x55 → creating PTY at that size`; per-pane bench lines show `first_grid == grid` for grid, floating (84x18) and torn-off (109x31) panes. kittest: `mode con` inside the child prints exactly the first-frame column count, and the post-resize count after a resize. |
| every resize reaches the PTY | **pass (automated for docked panes)** | kittest resize check above. Floating/torn-off resize: manual check 4. |
| 20 panes streaming, frame time | **CPU side pass; ≥60 fps not verifiable here** | 20 × 2000 lines/s (737 MB in 30 s): `ui()` avg 1.38 ms p95 1.81; eframe full-frame CPU (ui + tessellation + render submission, no vsync wait) avg 2.7 ms p95 3.3; 19.4 mesh rebuilds/frame. Unthrottled (1.74 GB in 30 s, ~58 MB/s): full-frame CPU avg 3.6 ms p95 6.9. Achieved 1.1–1.7 fps, but **control: an empty window repainting continuously gets 2.2 fps (2.8 without vsync)** on this session, so presentation, not the renderer, is the cap. |
| idle panes cost ~nothing | **pass** | 20 idle pwsh panes: 3 frames in 20 s (one is the bench's own wake-up), 0 mesh rebuilds, `ui()` 0.45 ms. No cursor blink, no polling; repaints come only from PTY output. |
| floating pane in an `egui::Window` | **pass (renders, sized, spawns); input manual** | 0.1 ms per frame. |
| torn-off pane (multi-viewport) | **works; input/focus/DPI manual; perf risk** | Opens as a second OS window at its own measured size. Its immediate viewport renders and presents synchronously inside the main frame: 230–540 ms per frame on WARP. See risks. |
| processes die with the app | **pass** | Started 5 PTYs, closed the app: all 5 OpenConsole and 5 pwsh children gone. `kill_takes_the_whole_tree` test checks a grandchild too. |
| true colour, box drawing, wide chars, emoji, ligatures, attributes | **pass offscreen; on-screen manual** | `tests/render_snapshot.rs` renders with the real wgpu renderer to `target/term-spike-render.png`: 24-bit gradient, 256-colour, CJK, colour emoji (Segoe UI Emoji via COLR), Cascadia Code ligatures (`->` `=>` `!=` `<=`), bold/italic/underline/strike/inverse/dim, joined box lines. |
| selection + copy, bracketed paste, mouse reporting | **encoding unit-tested; interaction manual** | `csm-term` tests: SGR/X10/UTF-8 mouse, paste markers and the paste-cannot-break-out case. |
| IME (CJK), ligature look, visual quality | **not verified** | manual checks 5–7 |

## Manual checks for a human (at the console, not over RDP, on a GPU)

Build first (see above), then from the repo root:

1. **Frame rate gate.** `.\target\release\term-spike.exe --panes 20 --stress --no-extra --bench 30`
   then `findstr BENCH term-spike.log`. Pass: `fps` ≥ 60 and `frame interval p95` ≤ ~17 ms.
   Repeat with `--rate 0` (unthrottled) and without `--no-extra` (adds the torn-off window).
   Also run `--panes 0 --no-extra --spin --bench 10` as the control: it should show ~your
   refresh rate.
2. **Idle.** `term-spike --panes 20 --no-extra --bench 40`: the BENCH line should show a handful
   of frames over the last 20 s and `mesh rebuilds/frame 0.00`. Task Manager: CPU ~0% when idle.
3. **Claude.** `term-spike --panes 1 --no-extra --cwd <a scratch folder> -- claude --debug-file C:\tmp\c.log`.
   The trust dialog renders with its box and the `❯` selector; arrow keys move the selector;
   Esc / Ctrl+C exits. `findstr programStatus C:\tmp\c.log` → `programStatus=yes`. In a folder
   you trust, chat once ("say hi", nothing that edits files) and watch the header strip show
   `7501 working` → `7501 done`.
4. **Floating and torn-off windows.** `term-spike`. Click the floating pane, type `dir`, Enter.
   Resize the floating window by its corner: run `$Host.UI.RawUI.WindowSize` (pwsh) after each
   resize, it must match the `NxM` in the pane header. Same in the torn-off OS window, including
   maximize/restore and, if you have two monitors with different scaling, dragging it between
   them (text must stay crisp and the grid size must update). Click between panes and windows:
   only the clicked pane gets keys (accent ring). Close the torn-off window: the pane docks back
   into the grid with its scrollback intact. Use the header buttons `float` / `tear off` / `dock`.
5. **IME (CJK).** Windows Settings → Time & language → add Japanese (Microsoft IME). Focus a
   pane, switch input (Win+Space), type `nihongo`, Space to convert, Enter to commit. Pass: the
   composition text shows at the cursor while composing (underlined), the candidate window sits
   next to the cursor, and `日本語` appears at the prompt after Enter, once. Repeat with Korean
   (2-set) and Chinese (Pinyin). On macOS the equivalent check belongs to the Unix run.
6. **Ligatures and glyph quality.** In a pane: `echo '-> => != === <= >= |> ::'`. Arrows/≠ glyphs
   should form (Cascadia Code). Compare text weight and sharpness with Windows Terminal at the
   same font size side by side; note if ours looks heavier (the coverage curve is uncalibrated,
   see risks). `term-spike --no-ligatures` turns them off. `--font C:\path\FiraCode.ttf` tests
   another ligature font.
7. **Selection, copy, paste.** Drag to select (highlight), Ctrl+C copies (with no selection
   Ctrl+C sends ^C to the program), paste into Notepad. Double-click selects a word, triple a
   line. Copy two lines with an `echo` in them and Ctrl+V into pwsh: nothing runs until you press
   Enter (bracketed paste; PSReadLine shows both lines). Mouse wheel scrolls back; typing jumps
   to the bottom.
8. **Mouse reporting.** In pwsh: ``Write-Host -NoNewline "`e[?1000h`e[?1006h"`` then click in the
   pane: PSReadLine echoes sequences like `^[[<0;12;5M`. Shift+click still selects. Turn off with
   ``Write-Host -NoNewline "`e[?1000l`e[?1006l"``. Better: `claude` → `/config` or any TUI
   with mouse support (e.g. `nvim` if installed, `:set mouse=a`).
9. **Emoji / wide characters.** `echo "日本語 🚀✅🦀"` in pwsh: wide chars take two cells, the
   cursor lands right after them, emoji are in colour.

## Known gaps and risks

1. **Frame rate unverified (environment).** Only CPU-side cost was measured (≤7 ms p95 for 20
   unthrottled panes). GPU cost of ~20 panes × ~2.5k cells of quads is small for any real GPU,
   but it is not measured. Manual check 1 closes it.
2. **Immediate viewports serialize presentation.** `show_viewport_immediate` renders and
   presents the torn-off window inside the main frame. With vsync on real hardware that likely
   means one vsync wait per window per frame (e.g. 2 windows → ~30 fps). The product should use
   `show_viewport_deferred`, which needs the render resources (`FontSet`, atlas) behind
   `Arc<Mutex<…>>`; the terminal state is already shared that way. Measure on hardware first.
   *Update (feature/ui):* `csm-ui` draws torn-off panes and groups with `show_viewport_deferred`;
   the renderer and views live in an `Arc<Mutex<PaneHost>>`. Frame cost on hardware still unmeasured.
3. **Glyph coverage is linear.** Text looks heavier than egui's own text and probably than
   Windows Terminal. Needs a coverage/gamma curve (and maybe subpixel positioning) tuned by eye.
   *Update (feature/ui):* measured in `crates/csm-ui/tests/text_weight.rs` (same font, size and
   colours, egui text vs the terminal, summed luminance distance from the background). With the
   same font the spike's setting (hinted, linear) was 0.81× egui's ink in Mocha and 1.0× in
   Latte, i.e. lighter, not heavier; the heaviness seen was probably Cascadia Code next to
   egui's proportional font. The terminal now renders unhinted with egui's curve per theme
   (2c − c² on dark, linear on light): 1.01× in Mocha, 0.99× in Latte, at 1× and 1.5×. Whether
   that matches Windows Terminal is still a by-eye check (manual check 6).
4. **Font fallback is a fixed Windows list** (Segoe UI Emoji, Segoe UI Symbol, Microsoft YaHei,
   Yu Gothic, Malgun Gothic). No system font discovery, no per-OS lists, no italic face (italic is
   a synthetic skew). Emoji in a *single* cell are centred and may overflow it.
5. **Rounded corners and dashed lines** are drawn as square/solid; diagonals and Powerline
   glyphs come from the font. Double-line junctions are approximate.
6. **IME untested**, as are dead keys and AltGr on non-US layouts (`Ctrl+Alt` is treated as
   AltGr and left to text input).
7. **egui swallows Ctrl+C/X/V** into Copy/Cut/Paste events; the widget maps them back (Copy →
   copy or ^C, Cut → ^X, Paste → bracketed paste), so a program that wants a raw ^V never gets
   it. Kitty keyboard protocol is off.
8. **Inbox ConPTY: Ctrl+C did not exit Claude** in `probe-check --inbox` (it did on bundled).
   Not investigated; inbox is the fallback path only.
9. **Lock contention under unthrottled output**: the UI never waits (try_lock), so a pane may
   show a frame-old mesh (35 misses in 25 frames at ~58 MB/s). Acceptable; worth watching.
10. **Unix path compiled, never run** (no macOS/Linux machine used). PTY via portable-pty; tree
    kill via `killpg(SIGKILL)` on the session's group.
11. **Not built:** link detection, cursor blink, URL hover, search, a real theme system,
    accessibility of the terminal contents.
12. Shell startup in this profile takes ~5 s (conda init in the pwsh profile); not ours.

## Recommendation

**Go, conditional on manual checks 1 (frame rate on a GPU) and 5 (IME).** Everything that could
be measured here passed: our ConPTY binding reproduces the WPF app's hard-won behaviour
(bundled by full path, early output kept, DA1/7501 order), Claude reports
`programStatus=yes` and now also `synchronizedOutput=yes`, the first PTY size is the measured
grid, idle panes cost nothing, and 20 unthrottled panes cost under 7 ms of CPU per frame. The
two items this environment could not answer are exactly the plan's Tauri-fallback triggers
(performance, IME); if either fails at the console, the fallback in the plan applies.
