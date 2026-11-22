# Third-party components and licences

CodeShellManager itself is MIT-licensed ([LICENSE](../LICENSE)). It is built on open-source
components, all under permissive licences. We never link or bundle copyleft code.

## Rust crates

The crates linked into the app are checked against an allow list of permissive licences with
[cargo-deny](https://github.com/EmbarkStudios/cargo-deny) (`deny.toml`, run in CI as
`cargo deny check`). The full list of crates with their licence texts is produced with
[cargo-about](https://github.com/EmbarkStudios/cargo-about) from `about.toml` and `about.hbs`:

```sh
cargo about generate about.hbs -o THIRD-PARTY-LICENSES.html
```

The allow list: MIT, MIT-0, Apache-2.0 (also with the LLVM exception), BSD-2-Clause,
BSD-3-Clause, ISC, Zlib, 0BSD, Unicode-3.0, BSL-1.0, CC0-1.0, Unlicense, CDLA-Permissive-2.0,
OFL-1.1 and Ubuntu-font-1.0 (egui's embedded fonts), and MPL-2.0 for one crate only
(`option-ext`), as `deny.toml` enforces.

Notable components:

| Component | Licence | Used for |
|---|---|---|
| [egui / eframe](https://github.com/emilk/egui), [wgpu](https://github.com/gfx-rs/wgpu) | MIT OR Apache-2.0 | the UI and its GPU rendering |
| [alacritty_terminal](https://github.com/alacritty/alacritty) | Apache-2.0 | the terminal emulator inside each pane |
| [rusqlite](https://github.com/rusqlite/rusqlite) (bundled SQLite, public domain) | MIT | the search index |
| `craft-ipc` (vendored in `vendor/craft-ipc`) | MIT OR Apache-2.0 | the link between `csm` / `csm mcp` and the app |
| syntect, two-face and bat's grammars | MIT, MIT OR Apache-2.0 | syntax highlighting in add-on screens; see [third-party-syntaxes.md](third-party-syntaxes.md) |

## Bundled files

| File | Licence | Notes |
|---|---|---|
| `conpty.dll`, `OpenConsole.exe` (Windows) | MIT ([microsoft/terminal](https://github.com/microsoft/terminal)) | Microsoft's ConPTY package `Microsoft.Windows.Console.ConPTY` from nuget.org, Microsoft-signed; fetched at build time by `scripts/get-conpty.ps1`, never committed |
| JetBrains Mono (terminal font) | SIL OFL 1.1 | `crates/csm-ui/assets/fonts/OFL-JetBrainsMono.txt` |
| Figtree (UI font) | SIL OFL 1.1 | `crates/csm-ui/assets/fonts/OFL-Figtree.txt` |
| The umage.ai logo | © umage.ai | shown on the About page; not covered by the MIT licence |

The colour themes follow the [Catppuccin](https://github.com/catppuccin/catppuccin) palette (MIT).
