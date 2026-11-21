# Third-party code behind syntax highlighting

The code and diff widgets of add-on screens highlight text with these crates. They are linked into
the app; the grammars are data inside one of them.

| crate | version | licence | what it is |
|---|---|---|---|
| `syntect` | 5.3 | MIT | the highlighting engine (parsing only; no themes, no HTML). Built with `regex-fancy`, so the regex engine is `fancy-regex` (MIT): pure Rust, no Oniguruma, no C build |
| `fancy-regex` | 0.16 | MIT | regexes with look-around, for the grammars |
| `two-face` | 0.5.2 (bat 0.26.1) | MIT OR Apache-2.0 | the grammar set (bat's `syntaxes.bin`, versioned): TypeScript, TSX, TOML, Dockerfile and many more |
| `bincode` 1.3, `bit-set`, `bit-vec`, `fnv` | | MIT or MIT OR Apache-2.0 | their dependencies, all on the `deny.toml` allow list |

CSM uses the grammars only. It uses none of two-face's or syntect's colour themes: every colour on
screen comes from CSM's own theme tokens (`syn_*`, `diff_*`).

## The grammars' own licences

Each grammar in bat's set comes from a Sublime Text package with its own licence. two-face embeds
the notices of the ones that require acknowledgement (50 MIT, 13 Apache-2.0, 3 BSD) and lists
the rest in
<https://codeberg.org/CosmicHarper/two-face/src/tag/v0.5.2+bat-0.26.1/generated/acknowledgements_full.md>.
The set contains only permissive licences: MIT, BSD 2- and 3-clause, Apache-2.0, the Unlicense, the
WTFPL and Sublime's own permissive grant ("permission to copy, use, modify, sell and distribute").
No GPL, LGPL, MPL or other copyleft grammar is in it.

This was checked by hand on 2026-11-21 against two-face 0.5.2, and a test
(`csm-services`, `highlight::tests::the_bundled_grammars_carry_only_permissive_licences`) fails if
an update brings a licence type that was not reviewed. `cargo about generate about.hbs` (the
release's third-party bundle) lists the crates; the grammar notices come with two-face's data and
the link above.

The WTFPL is not on `deny.toml`'s allow list because `cargo deny` reads crate metadata, not the
data inside a crate; the grammars are data, not code CSM links against.

## The fancy-regex build leaves out a few grammars

two-face's `fancy` build omits the grammars that use regex features `fancy-regex` lacks, to keep
regex compilation infallible. A language that is missing simply shows as plain text.
