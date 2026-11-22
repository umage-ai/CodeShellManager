# Distributing CodeShellManager 2 on macOS and Linux

Status: plan, November 2026. Windows ships through `release-v2.yml` (signed MSI and zip, GitHub
pre-releases; winget and Chocolatey at 2.0 final). This document covers the other two platforms:
what the build produces today, where to publish it, in what order, and what is still broken at
run time.

## 1. What the build produces

`.github/workflows/release-v2-unix.yml` runs on the same triggers as `release-v2.yml` (a `v2.*`
tag, or `workflow_dispatch` for a dry run). It uploads **workflow artifacts only**. Nothing is
attached to a release and nothing is published.

| Artifact | Contents |
|---|---|
| `codeshellmanager-<ver>-linux-x86_64` | `CodeShellManager-<ver>-linux-x86_64.tar.gz`, `CodeShellManager-<ver>-x86_64.AppImage`, `codeshellmanager_<debver>_amd64.deb`, SHA256SUMS |
| `codeshellmanager-<ver>-linux-aarch64` | the same for aarch64 / arm64 |
| `codeshellmanager-<ver>-macos` | `CodeShellManager-<ver>-macos-universal.dmg` (arm64 + x86_64), SHA256SUMS |

- `<debver>` turns `2.0.0-beta.3` into `2.0.0~beta.3`, so that Debian sorts the beta before 2.0.0.
- **Linux:** built on `ubuntu-22.04` (glibc 2.35), so the binaries also run on Debian 12, Ubuntu
  22.04+ and Fedora/RHEL of the same age.
  - The .deb installs into `/usr/lib/codeshellmanager/`, with symlinks `/usr/bin/codeshell` and
    `/usr/bin/csm`, a `.desktop` file and a hicolor icon. Its `Depends` line comes from
    `dpkg-shlibdeps` plus a fixed list of the libraries that winit and wgpu load with `dlopen`.
  - The AppImage bundles no libraries.
  - `./CodeShellManager.AppImage csm mcp` runs the CLI, as does a symlink to the AppImage named `csm`.
- **macOS:** `CodeShellManager.app` with bundle id `ai.umage.CodeShellManager`.
  - `Contents/MacOS` holds `codeshell`, `csm`, `csm-review` and `csm-session-summary`.
  - `Contents/Resources` holds `AppIcon.icns` and `samples/addons`.
  - Minimum macOS version: 11.
  - On a tag, when every `APPLE_*` secret exists, the app is signed with Developer ID and hardened
    runtime, then the dmg is signed, notarized and stapled. Otherwise the bundle is ad-hoc signed.
- **Icons:** generated in CI from `apps/codeshell/assets/codeshell.ico` (which a test keeps in sync
  with the drawn icon) by `scripts/ico-to-png.py`.

**Not verified.** The workflow has never run: it can only run on GitHub. Locally, it parses as
YAML and its shell and packaging steps were reviewed by hand; actionlint was not available. Check
these on the first dry run:

- **arm64 Linux runner.** `ubuntu-22.04-arm` is free for public repositories. If the repository is
  private, the aarch64 job needs a paid larger runner, or should be dropped.
- **appimagetool `1.9.0`.** The download URL is pinned to that tag and has not been checked.
- **Unsigned packages.** The .deb's `Maintainer` field has no email address (see Decisions).
  `dpkg-deb` accepts this; lintian would not.
- **Signing steps.** Developer ID signing and notarization run only with real secrets on a tag.
- **Dispatch from main.** Like `release-v2.yml`, a copy of the workflow file must exist on `main`
  before `gh workflow run release-v2-unix.yml --ref v2` can dispatch it.

## 2. Recommendation and order

1. **GitHub Releases, every platform, from the next beta tag.** Attach the tar.gz, AppImage, .deb
   and dmg to the same pre-release as the MSI. Either give this workflow `contents: write` and
   upload with `softprops/action-gh-release` to the tag's release, or fold it into `release-v2.yml`.
   Coordinate with whoever owns that file. This costs nothing, and every later channel downloads
   from these URLs.
2. **macOS: Homebrew cask in our own tap** (`umage-ai/homebrew-tap`). Users run
   `brew install --cask umage-ai/tap/codeshellmanager`. It is cheap, under our control, and needs no
   notability review. Do this as soon as notarization works (step 3), because unsigned casks are
   painful for users and Homebrew is retiring casks that fail Gatekeeper.
3. **macOS: Apple Developer ID and notarization.** This is a prerequisite for a pleasant macOS
   install by any route, so start the enrolment now: it can take days.
4. **Linux: AUR `codeshellmanager-bin`.** Arch users expect it. It repackages the release tarball,
   takes about an hour to set up and is a few minutes per release (or automated).
5. **Linux: an apt repository** (then rpm), once there are Linux users who want automatic updates.
6. **homebrew/cask (official)**, once the project meets Homebrew's notability bar.
7. **Nix**: a flake in the repo is cheap. nixpkgs can wait for a community packager.
8. **Not now: Flathub, Snap, Mac App Store.** Their sandboxes break the app's core job (section 4).

## 3. macOS

### Signing and notarization (required in practice)

- **Apple Developer Program:** 99 USD a year. Enrol as the organisation (umage), not a person, so
  the certificate reads "Developer ID Application: umage …". An organisation needs a D-U-N-S
  number. Allow 1–2 weeks.
- **Certificate:** a *Developer ID Application* certificate, exported with its private key as a
  `.p12`. Only the Account Holder can create it.
- **Notarization:** an App Store Connect API key (Users and Access → Integrations → Keys, role
  "Developer"). `notarytool` uses the key id, the issuer id and the `.p8` file. This is better
  than an Apple ID and app-specific password: it is not tied to a person and has no 2FA prompts.
- **What notarization requires:**
  - every Mach-O is signed with Developer ID, with hardened runtime (`--options runtime`) and a
    secure timestamp;
  - there are no data files in `Contents/MacOS`, which is why `samples/addons` moved to
    `Resources`.

  The workflow does all of this. The app needs no entitlements: the shells and CLIs it starts are
  separate signed binaries, and hardened runtime applies only to our own process.
- **Without notarization:**
  - On macOS 15, a downloaded app cannot be opened by right-click → Open any more. The user has
    to go to System Settings → Privacy & Security → Open Anyway, or run
    `xattr -dr com.apple.quarantine`.
  - That is acceptable for testers. It is not acceptable for a public release.
- **Workflow secrets:** `APPLE_CERTIFICATE_P12` (base64), `APPLE_CERTIFICATE_PASSWORD`,
  `APPLE_SIGNING_IDENTITY`, `APPLE_API_KEY_ID`, `APPLE_API_ISSUER_ID`, `APPLE_API_KEY_P8` (base64).

### Homebrew

- **Own tap:** create the repository `umage-ai/homebrew-tap` and add `Casks/codeshellmanager.rb`.
  - It points at the release dmg and its sha256.
  - `app "CodeShellManager.app"` installs the app.
  - `binary "#{appdir}/CodeShellManager.app/Contents/MacOS/csm"` puts `csm` on PATH, which agents
    need for `csm mcp`.
  - `zap trash:` lists the config directory.
  - A release job can bump the cask with a token that may push to the tap. That is a separate
    secret, e.g. `HOMEBREW_TAP_TOKEN`, a fine-grained PAT limited to that one repository.
- **homebrew/cask (official):**
  - The project must meet Homebrew's notability bar: GitHub stars, forks and watchers, with
    higher thresholds when the author submits the cask.
  - The app must be signed and notarized.
  - Updates arrive through `brew upgrade`. Do not mark the cask `auto_updates true` unless the
    app gets its own updater.
- **A Homebrew formula (built from source)** is possible, but it compiles the whole wgpu stack on
  the user's machine. The cask is the right shape for a GUI app.

### Mac App Store: not suitable

- An App Store app must run in the App Sandbox, and every process it starts inherits that sandbox.
  CodeShellManager's purpose is to start the user's own shells and CLIs (`claude`, `git`, `ssh`,
  anything on their PATH) with full access to their files and network. Inside the sandbox those
  CLIs cannot read the user's repositories, cannot run binaries that Homebrew installed, and
  cannot write `~/.claude`.
- This is why iTerm2, WezTerm, Ghostty, Alacritty and Warp all ship outside the store. There is no
  entitlement that fixes it.

### Updates on macOS

- `brew upgrade` covers Homebrew users.
- For dmg users, add later a "new version available" check against the GitHub Releases API: a
  tool, `app_check_update`, with a notice in the UI. A full in-place updater (Sparkle, or a Rust
  equivalent) adds an EdDSA key and an appcast to maintain. Postpone it until there is demand.

## 4. Linux

| Channel | Effort | Reach | Updates | Notes |
|---|---|---|---|---|
| GitHub Releases: tar.gz, AppImage, .deb | done (attach to release) | everyone who finds the repo | manual; AppImage can self-update (below) | the base for everything else |
| AUR `codeshellmanager-bin` | low: a PKGBUILD that repackages the tarball | Arch, Manjaro, EndeavourOS: many developers | `yay`/`paru` | needs an AUR account and an SSH key; bump per release (can be automated) |
| apt repository | medium | Debian, Ubuntu, Mint, Pop!_OS | `apt upgrade` | needs a GPG signing key and hosting: GitHub Pages with `reprepro`/`aptly`, or Cloudsmith (free OSS tier). Users add a source and a key once |
| rpm + dnf repository | medium (add `rpmbuild` or `cargo-generate-rpm` to the workflow) | Fedora, RHEL, openSUSE | `dnf upgrade` | same signing and hosting story; Fedora COPR is an alternative |
| Nix | low (flake in repo) / medium (nixpkgs) | NixOS users | `nix flake update` | nixpkgs needs a maintainer and builds from source (fetches git dependencies, wgpu) |
| Flathub | high, plus an ongoing sandbox fight | the widest desktop reach, software-centre visible | automatic | see below: the sandbox conflicts with the app's purpose |
| Snap | medium, plus a manual review | Ubuntu | automatic | needs `classic` confinement, granted only after manual review |

### Why Flatpak and Snap are a real problem for this app

CodeShellManager spawns the user's login shell in a PTY, runs `git`, `ssh`, `claude` and so on
from the user's PATH, reads `~/.claude`, and serves the `csm` CLI over a socket in
`$XDG_RUNTIME_DIR`.

- **Flatpak.** The app runs in a sandbox with its own runtime and `/usr`.
  - The user's shell and tools are not inside it. To run them it must use
    `flatpak-spawn --host`, which needs the `--talk-name=org.freedesktop.Flatpak` permission. That
    is a full sandbox escape, and Flathub accepts it only with a justification (terminals such as
    Ptyxis and BlackBox do this).
  - Each PTY then has to be created through the host-spawn D-Bus API instead of `openpty` in our
    own process. That is new code in `csm-pty`.
  - `$XDG_RUNTIME_DIR` inside the sandbox is `/run/user/<uid>/app/ai.umage.CodeShellManager`. A
    `csm` installed on the host would not find the socket, and an MCP client cannot easily
    start `flatpak run --command=csm`.
  - Filesystem access needs `--filesystem=home` or `host`.
  - The result is a sandbox in name only, plus real engineering work. Do it only when there is
    clear demand for software-centre discovery.
- **Snap.** Strict confinement cannot run arbitrary host binaries at all. `classic` confinement
  (no sandbox) is what terminals and IDEs use, and the Snap Store team must approve it by hand
  for each snap, which they do for terminals. That makes it feasible but bureaucratic. Ubuntu
  users are equally well served by the .deb or an apt repository.

### AppImage notes

- Self-update: build with `appimagetool -u "gh-releases-zsync|umage-ai|CodeShellManager|latest|CodeShellManager-*-x86_64.AppImage.zsync"`
  and upload the `.zsync` file next to the AppImage. AppImageUpdate and AppImageLauncher can
  then update it. Add this once the files are attached to releases.
- The AppImage sets no `LD_LIBRARY_PATH`, on purpose: the shells the app starts must not inherit
  one.
- `csm setup` writes the absolute path of `csm` into MCP client configs. In an AppImage that path
  is a temporary mount (see runtime gap 7). Until that is fixed, the AppImage is less suited to
  MCP use than the .deb or the tarball.

## 5. Names and IDs to reserve

| What | Value | Where / how |
|---|---|---|
| App / bundle / Flatpak id | `ai.umage.CodeShellManager` | already used for the macOS bundle id and the Linux `.desktop` file. Flathub verifies ownership of `umage.ai` with a token under `https://umage.ai/.well-known/` |
| Homebrew tap | `umage-ai/homebrew-tap` (→ `umage-ai/tap`) | create the GitHub repository |
| Cask token | `codeshellmanager` | in the tap now; check it is free in homebrew/cask |
| Debian/rpm package | `codeshellmanager` | the binaries are `codeshell` and `csm`; check that `csm` does not clash in the target distributions |
| AUR | `codeshellmanager-bin` (and later `codeshellmanager` built from source) | AUR account |
| Snap name | `codeshellmanager` | register it on snapcraft.io now (free), even if nothing ships |
| Apple Team / Developer ID | umage organisation | Apple Developer Program |
| apt/rpm repository host | e.g. `https://umage-ai.github.io/CodeShellManager/apt` or Cloudsmith `umage-ai/codeshellmanager` | decide (below) |

## 6. Accounts and secrets Allan needs

- **Apple Developer Program** (organisation, 99 USD a year, D-U-N-S number), then the six `APPLE_*`
  repository secrets listed in section 3.
- **Homebrew tap:** the repository `umage-ai/homebrew-tap`, and `HOMEBREW_TAP_TOKEN` if releases
  bump it automatically.
- **AUR:** an account with an SSH key (`AUR_SSH_PRIVATE_KEY` if CI pushes the PKGBUILD).
- **apt/rpm repository:** a GPG signing key (`LINUX_REPO_GPG_KEY`, `LINUX_REPO_GPG_PASSPHRASE`)
  and the hosting choice.
- **Optional now:** a snapcraft.io account to reserve the name, and a Flathub domain verification
  for `umage.ai`.
- **The .deb maintainer:** an email address for the `Maintainer` field (and AUR/Homebrew
  metadata).

## 7. Runtime gaps on Linux and macOS

The packages build, and sessions start (`$SHELL -l -i -c`, process-group kill, `xdg-open`/`open`,
editor detection, `dirs`-based config paths and the IPC socket in a 0700 directory all have
Unix paths). These items still make a Unix build worse than the Windows one. Paths are relative to
the repository root. Two small ones are fixed on this branch: marked *fixed*.

**Blockers for a public release**

1. **macOS: Option key.** `crates/csm-ui/src/terminal/view.rs:206-209` and `:515-520` drop text
   typed with Option held, and send Option+letter as ESC+letter.
   - On German, Nordic and French Mac layouts, `@ [ ] { } | \ ~` need Option, so they cannot be
     typed.
   - Fix: on macOS, treat Option as a text modifier, with an "Option as Meta" setting.
2. *(fixed)* **Process add-ons not found beside the app.** `crates/csm-engine/src/proc.rs`
   `spawn_piped` passed a bare `csm-review` to exec, which searches only PATH on Unix. It now
   looks in the app's folder first.
3. **macOS: Cmd+Q, Dock Quit and logout skip the shutdown.** winit's default menu calls
   `terminate:`, so `main.rs:447-452` (`ipc.shutdown()`, `engine.shutdown()`) never runs, and the
   debounced state save can be lost. Fix: flush in `WithMonitors::exiting`
   (`apps/codeshell/src/main.rs:289`), or replace the app menu.
4. **macOS: Cmd+C with no selection sends ^C** (`crates/csm-ui/src/terminal/view.rs:530-536`),
   interrupting Claude. On macOS only Ctrl+C should send ^C.

**Degraded**

5. **No emoji or CJK fallback fonts.** `crates/csm-ui/src/terminal/fonts.rs:56-64` builds the
   paths from `WINDIR`. Add per-OS lists: Apple Color Emoji, PingFang and Hiragino on macOS; Noto
   Color Emoji, Noto Sans CJK and DejaVu on Linux, or ask fontconfig. The primary font, embedded
   JetBrains Mono, is fine.
6. **macOS: no `LANG` for sessions.** An app started from Finder has no locale, and
   `crates/csm-engine/src/lifecycle.rs:612-614` sets only TERM and COLORTERM. Set
   `LANG=en_US.UTF-8` (or derive it from AppleLocale) when no locale variable is set.
7. **Paths found through `current_exe` that move.**
   - In an AppImage, `csm setup` (`crates/csm-setup/src/plan.rs:113-118`) records a temporary
     `/tmp/.mount_*` path.
   - The same happens for a translocated .app (one run from Downloads without being moved).
   - Prefer `$APPIMAGE`, or a stable install path.
   - The samples lookup (`crates/csm-engine/src/state.rs`) now also checks
     `Contents/Resources` *(fixed)*.
8. **`csm --start` cannot find the app.** `crates/csm-mcp/src/ipc_client.rs:91` →
   `vendor/craft-ipc/src/launch.rs:33-38` and `:357`.
   - It never looks beside `csm`.
   - On macOS it only tries `/Applications/Codeshell.app`, but the bundle is
     `CodeShellManager.app`.
9. **Runtime dir mismatch.** `vendor/craft-ipc/src/discovery/dir.rs:122-128` uses
   `$XDG_RUNTIME_DIR`/`$TMPDIR`. A `csm` started without that variable (sudo, cron, some ssh
   sessions) looks in `/tmp/craft-<euid>` and reports "not running". Probe both locations.
10. **No single-instance guard.** A second launch on Linux opens a second window writing the same
    `state.json`. macOS LaunchServices prevents this for a .app. Take the IPC lock at startup,
    and if another instance holds it, ask that instance to activate.
11. **PowerShell run mode on Unix.** `crates/csm-engine/src/runs/mod.rs:239-243` and `:339` spawn
    `pwsh`. Hide the mode, or fall back to `$SHELL -lc`.
12. **Wayland: no app id.** `apps/codeshell/src/main.rs:383-388` sets no `with_app_id`, and the
    `wayland` feature of eframe/egui-winit is not enabled (only winit's own is, through
    accesskit_winit), so an app id would be ignored anyway.
    - Effect: GNOME and KDE on Wayland show a generic icon and do not match the `.desktop` file.
      X11 works through `StartupWMClass=codeshell`.
    - Fix: enable eframe's `wayland` and `x11` features and set
      `.with_app_id("ai.umage.CodeShellManager")`.
13. **Wayland: `csm` activate cannot raise the window.** It passes no XDG activation token
    (`crates/csm-mcp/src/ipc_server.rs:161`).

**Cosmetic**

14. **Windows defaults on Unix.** The launch-command defaults include `pwsh` and `cmd`
    (`crates/csm-core/src/models/app_state.rs:81-82`). Choose per-OS defaults.
15. **WSL offered on every OS.** The session kind and the distro probe appear on all platforms
    (`crates/csm-ui/src/palette.rs:1317`, `:1437`) and fail on launch. Gate them behind
    `cfg!(windows)`.
16. **No Linux tray.** `apps/codeshell/src/tray.rs:121-129` has no Linux tray, so close-to-tray is
    silently off. Hide the setting.
17. **Notifications.** `apps/codeshell/src/os_notify.rs:101-114` sets no macOS bundle id
    (notifications may show as another app's) and has no click-through.
18. **Folder comparisons ignore case.** `crates/csm-engine/src/tools/sessions.rs:617`, `:664-665`
    and `crates/csm-ui/src/git_model.rs:130` lowercase folders (and the latter turns `/` into
    `\`). Linux file systems are case-sensitive.
19. **Shortcut labels say "Ctrl+…" on macOS.** The bindings use Command correctly; only the text
    is wrong (e.g. `crates/csm-ui/src/inspector.rs:35`).
20. **Folder picker on Linux needs `xdg-desktop-portal`** (rfd's portal backend). Most desktops
    have it. A bare window manager may not.
21. **Secret storage on Linux needs a Secret Service** (gnome-keyring or KWallet). The .deb
    recommends one.

## 8. Decisions for Allan

1. **Enrol in the Apple Developer Program now** (as the organisation; 99 USD a year). Notarization
   gates every good macOS route.
2. **Attach Linux and macOS files to the v2 beta GitHub pre-releases?** If yes, either this
   workflow gets `contents: write` and uploads to the tag's release, or it is merged into
   `release-v2.yml`. That needs coordinating with the Windows release owner. Either way, a copy
   of `release-v2-unix.yml` must go on `main` for manual dispatch.
3. **Approve the names:** `ai.umage.CodeShellManager`, tap `umage-ai/homebrew-tap`, cask and
   package `codeshellmanager`, AUR `codeshellmanager-bin`. Reserve the Snap name?
4. **Order:** Releases → own Homebrew tap (after notarization) → AUR → apt/rpm repository →
   official homebrew/cask. Flathub, Snap and the Mac App Store not now. Agree?
5. **apt/rpm hosting:** GitHub Pages (free, self-managed with `reprepro`/`createrepo`) or
   Cloudsmith (free OSS tier, hosted)?
6. **Which runtime gaps block a macOS or Linux beta?** Suggestion: 1, 3, 4, 5 and 6 before
   announcing macOS; 12 and 14 before announcing Linux.
7. **The maintainer email** for the .deb, AUR and Homebrew metadata.
8. **aarch64 Linux:** keep it if the repository is public (free arm runners). Drop it otherwise.
