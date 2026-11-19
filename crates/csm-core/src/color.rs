//! Deterministic accent colour for a session (C# `ColorService`): FNV-1a of the lowercased,
//! trailing-slash-trimmed key, modulo a 12-colour palette. Same outputs as the C# for every
//! input, including non-ASCII (see [`fnv1a`]).

// 12 colours spaced ~30 degrees apart on the HSL wheel, all at similar saturation/lightness so
// every project folder gets a clearly distinct accent.
const PALETTE: [&str; 12] = [
    "#FF6B6B", // red
    "#FF9E42", // orange
    "#FFD166", // yellow
    "#AEDE68", // lime
    "#51CF66", // green
    "#38D9A9", // emerald
    "#66D9E8", // cyan
    "#4DABF7", // blue
    "#748FFC", // indigo
    "#9775FA", // violet
    "#F783AC", // pink
    "#FF6B95", // rose
];

/// The palette, in order.
pub fn palette() -> &'static [&'static str; 12] {
    &PALETTE
}

/// The accent colour (`#RRGGBB`) for a folder path, `user@host`, or `wsl://...` key.
pub fn hex_color(key: &str) -> &'static str {
    let lowered = to_lower_invariant(key);
    let trimmed = lowered.trim_end_matches(['/', '\\']);
    let idx = (fnv1a(trimmed) as usize) % PALETTE.len();
    // `idx` is < 12 by construction; `get` keeps the no-indexing rule without an unwrap.
    PALETTE.get(idx).copied().unwrap_or("#FF6B6B")
}

/// `ToLowerInvariant`, per scalar value. Rust's `str::to_lowercase` additionally applies the
/// context-sensitive final-sigma rule and can expand one char into several (U+0130), neither of
/// which .NET's invariant mapping does, so a one-to-one per-char mapping is used.
fn to_lower_invariant(s: &str) -> String {
    s.chars()
        .map(|c| {
            let mut lower = c.to_lowercase();
            match (lower.next(), lower.next()) {
                (Some(single), None) => single,
                _ => c,
            }
        })
        .collect()
}

/// 32-bit FNV-1a over the UTF-16 code units of `s`, each truncated to its LOW BYTE.
///
/// C# parity, and a quirk worth knowing: the source is `hash ^= (byte)c` over a `string`, so a
/// non-ASCII character contributes only its low byte (U+00E9 -> 0xE9, U+20AC -> 0xAC) and an
/// astral character contributes the low bytes of its two surrogates. Distinct non-ASCII paths
/// can therefore collide. Kept, because changing it would recolour existing users' sessions.
fn fnv1a(s: &str) -> u32 {
    let mut hash: u32 = 2_166_136_261;
    for unit in s.encode_utf16() {
        hash ^= u32::from(unit & 0xFF);
        hash = hash.wrapping_mul(16_777_619);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, HashSet};

    // The 12-colour palette, kept verbatim so a palette change forces a deliberate update here.
    const EXPECTED_PALETTE: [&str; 12] = [
        "#FF6B6B", "#FF9E42", "#FFD166", "#AEDE68", "#51CF66", "#38D9A9", "#66D9E8", "#4DABF7",
        "#748FFC", "#9775FA", "#F783AC", "#FF6B95",
    ];

    #[test]
    fn palette_is_the_expected_one() {
        assert_eq!(palette(), &EXPECTED_PALETTE);
    }

    #[test]
    fn is_deterministic_same_input_produces_same_color() {
        let key = r"C:\projects\my-app";
        assert_eq!(hex_color(key), hex_color(key));
    }

    #[test]
    fn is_deterministic_across_multiple_calls() {
        for key in [
            r"C:\projects\alpha",
            r"C:\projects\beta",
            "user@host.example.com",
            "",
        ] {
            let results: HashSet<&str> = (0..5).map(|_| hex_color(key)).collect();
            assert_eq!(results.len(), 1, "{key}");
        }
    }

    #[test]
    fn different_keys_map_to_different_colors_on_small_sample() {
        // Not guaranteed by any hash, but this handpicked sample spreads for FNV-1a.
        assert_ne!(hex_color(r"C:\proj\a"), hex_color(r"C:\proj\b"));
    }

    #[test]
    fn returns_valid_hex_format() {
        for key in [
            r"C:\projects\my-app",
            "",
            "   ",
            "user@host:22",
            r"\\unc\share\path",
        ] {
            let hex = hex_color(key);
            assert_eq!(hex.len(), 7);
            assert!(hex.starts_with('#'));
            assert!(hex[1..].bytes().all(|b| b.is_ascii_hexdigit()), "{hex}");
        }
    }

    #[test]
    fn returned_value_is_always_from_palette() {
        for i in 0..200 {
            let hex = hex_color(&format!("C:\\repo\\project-{i}"));
            assert!(EXPECTED_PALETTE.contains(&hex), "{hex}");
        }
    }

    #[test]
    fn distributes_across_all_palette_colors_over_large_sample() {
        // If this becomes flaky, widen the sample: uneven distribution is a real regression signal.
        let mut counts: HashMap<&str, u32> = EXPECTED_PALETTE.iter().map(|c| (*c, 0)).collect();
        for i in 0..120 {
            *counts
                .entry(hex_color(&format!("C:\\repo\\project-{i}")))
                .or_insert(0) += 1;
        }
        for color in EXPECTED_PALETTE {
            assert!(
                counts.get(color).copied().unwrap_or(0) > 0,
                "{color} was never produced over 120 keys"
            );
        }
    }

    #[test]
    fn is_case_insensitive() {
        assert_eq!(hex_color(r"C:\Foo\Bar"), hex_color(r"c:\foo\bar"));
    }

    #[test]
    fn trims_trailing_slashes_and_backslashes() {
        let bare = hex_color(r"C:\projects\app");
        assert_eq!(bare, hex_color(r"C:\projects\app\"));
        assert_eq!(bare, hex_color(r"C:\projects\app/"));
        assert_eq!(bare, hex_color(r"C:\projects\app/\"));
    }

    #[test]
    fn does_not_trim_internal_slashes() {
        assert_ne!(hex_color(r"C:\projects\app"), hex_color("Cprojectsapp"));
    }

    #[test]
    fn empty_string_is_the_fnv_offset_basis_slot() {
        // FNV-1a of "" is the offset basis: 2166136261 % 12 == 1 -> Palette[1] == "#FF9E42".
        assert_eq!(hex_color(""), "#FF9E42");
        assert_eq!(2_166_136_261u32 % 12, 1);
    }

    #[test]
    fn whitespace_string_returns_a_valid_palette_color() {
        // TrimEnd only strips '/' and '\\', so whitespace is preserved and hashed.
        assert!(EXPECTED_PALETTE.contains(&hex_color("   ")));
    }

    // (C# `GetHexColor_Null_Throws` locks a NullReferenceException for null; a Rust `&str`
    // cannot be null. `GetColor`/`GetBrush` return WPF types and are not ported.)

    #[test]
    fn non_ascii_keys_hash_the_low_byte_of_each_utf16_unit() {
        // C# parity: (byte)c. 'e-acute' U+00E9 and U+01E9 differ as chars but hash alike.
        assert_eq!(fnv1a("\u{e9}"), fnv1a("\u{1e9}"));
        assert_eq!(hex_color("\u{e9}"), hex_color("\u{1e9}"));
        // An astral char hashes as its two surrogate low bytes (U+1F600 = D83D DE00 -> 3D 00).
        assert_eq!(fnv1a("\u{1F600}"), fnv1a("\u{3d}\u{0}"));
    }

    /// Golden values produced by running the real C# `ColorService.GetHexColor` on .NET 11
    /// (`tests/fixtures/dotnet_color_golden.tsv`: hex of the UTF-8 key, a tab, the colour):
    /// paths, UNC, WSL and non-ASCII keys, including ones where the low-byte quirk matters.
    #[test]
    fn golden_values_match_the_csharp_implementation() {
        let mut n = 0;
        for line in include_str!("../tests/fixtures/dotnet_color_golden.tsv").lines() {
            let (hex_key, expected) = line.split_once('\t').unwrap();
            let bytes: Vec<u8> = (0..hex_key.len())
                .step_by(2)
                .map(|i| u8::from_str_radix(&hex_key[i..i + 2], 16).unwrap())
                .collect();
            let key = String::from_utf8(bytes).unwrap();
            assert_eq!(hex_color(&key), expected.trim(), "{key:?}");
            n += 1;
        }
        assert!(n >= 20, "fixture looks truncated: {n} rows");
    }

    /// `ToLowerInvariant` over every Unicode scalar value, against .NET 11's actual mapping
    /// (`tests/fixtures/dotnet_lower_invariant.txt`: `HEX:HEX` for each scalar that changes).
    ///
    /// The only differences allowed are letters that this Rust toolchain's newer Unicode tables
    /// lowercase and .NET's do not: all at or above U+1C89 (Unicode 16/17 additions: Cyrillic
    /// tje, Latin Extended-D, Garay, Todhri, Latin Extended-G). They are not in any real path.
    #[test]
    fn to_lower_invariant_matches_dotnet_for_every_scalar() {
        let mut expected: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
        for line in include_str!("../tests/fixtures/dotnet_lower_invariant.txt").lines() {
            let (a, b) = line.trim().split_once(':').unwrap();
            expected.insert(
                u32::from_str_radix(a, 16).unwrap(),
                u32::from_str_radix(b, 16).unwrap(),
            );
        }
        assert!(expected.len() > 1000);
        let mut newer_unicode_only = 0;
        for cp in 0..=0x10FFFFu32 {
            let Some(c) = char::from_u32(cp) else {
                continue;
            };
            let got = to_lower_invariant(&c.to_string())
                .chars()
                .next()
                .map(u32::from);
            let want = expected.get(&cp).copied();
            if got == Some(want.unwrap_or(cp)) {
                continue;
            }
            assert!(
                want.is_none() && cp >= 0x1C89,
                "U+{cp:04X}: rust={got:?} dotnet={want:?}"
            );
            newer_unicode_only += 1;
        }
        assert!(newer_unicode_only < 100, "{newer_unicode_only}");
    }
}
