//! WSL discovery parsing and the `\\wsl$` UNC mirror.
//!
//! Ports of `WslDiscoveryService.Parse`, `ToUncPath`, `TryParseUncPath` and
//! `ResyncWslWorkingFolder`. These are the *only* UNC to Linux path converters; git and the
//! New Session dialog delegate here. The probes that spawn `wsl.exe` are I/O and come later.

use crate::winpath;

/// One installed distro from `wsl -l -v`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WslDistro {
    pub name: String,
    pub version: i32,
    pub is_default: bool,
    pub state: String,
}

/// Decodes the bytes `wsl.exe` writes for its listings: UTF-16LE (with or without a BOM).
/// Reading it as UTF-8/ANSI interleaves every character with NULs and the parser sees
/// gibberish. Input that does not look like UTF-16 (no NUL in the second byte) is decoded as
/// UTF-8 so a future `wsl.exe` that switches encoding still works.
#[must_use]
pub fn decode_wsl_output(bytes: &[u8]) -> String {
    let body = bytes.strip_prefix(&[0xFF, 0xFE]).unwrap_or(bytes);
    let utf16 = bytes.starts_with(&[0xFF, 0xFE]) || body.get(1) == Some(&0);
    if !utf16 {
        return String::from_utf8_lossy(body).into_owned();
    }
    let (pairs, _odd_trailing_byte) = body.as_chunks::<2>();
    let units: Vec<u16> = pairs.iter().map(|c| u16::from_le_bytes(*c)).collect();
    String::from_utf16_lossy(&units)
}

/// Parses the body of `wsl -l -v`.
///
/// Header row skipped; `*` marks the default; the name may contain spaces (`wsl --import "My
/// Distro"`), so STATE and VERSION are taken from the end of the row. A real row's VERSION is
/// always an integer, which is what rejects a *localised* header (the literal `NAME` check only
/// catches English). Docker Desktop's internal distros are filtered out. Default first, then
/// alphabetical (ordinal ignore-case).
#[must_use]
pub fn parse_distros(raw: &str) -> Vec<WslDistro> {
    if raw.trim().is_empty() {
        return Vec::new();
    }
    let mut results = Vec::new();
    for line in raw.replace('\r', "").split('\n') {
        if line.trim().is_empty() {
            continue;
        }
        // Header row: "  NAME                   STATE           VERSION".
        if line.trim_start().starts_with("NAME") {
            continue;
        }
        let tokens: Vec<&str> = line.split(' ').filter(|t| !t.is_empty()).collect();
        let is_default = tokens.first() == Some(&"*");
        let first_name = usize::from(is_default);
        // NAME, STATE, VERSION: at least three columns after the marker.
        if tokens.len().saturating_sub(first_name) < 3 {
            continue;
        }
        let version_idx = tokens.len() - 1;
        let state_idx = tokens.len() - 2;
        let name = tokens
            .get(first_name..state_idx)
            .unwrap_or_default()
            .join(" ");
        let state = tokens.get(state_idx).copied().unwrap_or("").to_string();
        let Some(version) = tokens.get(version_idx).and_then(|v| v.parse::<i32>().ok()) else {
            continue;
        };
        results.push(WslDistro {
            name,
            version,
            is_default,
            state,
        });
    }
    results.retain(|d| !is_docker_internal_distro(&d.name));
    results.sort_by(|a, b| {
        b.is_default
            .cmp(&a.is_default)
            .then_with(|| a.name.to_uppercase().cmp(&b.name.to_uppercase()))
    });
    results
}

/// True for Docker Desktop's own distros (`docker-desktop`, `docker-desktop-data`): BusyBox
/// plumbing with no bash, rebuilt on Docker updates, not a user environment. Exact,
/// case-insensitive match only: a user-imported `my-docker-desktop-clone` must still be offered.
fn is_docker_internal_distro(name: &str) -> bool {
    winpath::eq_ic(name, "docker-desktop") || winpath::eq_ic(name, "docker-desktop-data")
}

/// Converts a WSL distro and Linux path to the Windows UNC view (`\\wsl$\Ubuntu\home\alice`).
/// Empty when the distro is blank; the distro root when the Linux path is blank.
#[must_use]
pub fn to_unc_path(distro: &str, linux_path: &str) -> String {
    if distro.trim().is_empty() {
        return String::new();
    }
    if linux_path.trim().is_empty() {
        return format!(r"\\wsl$\{distro}");
    }
    let trimmed = linux_path.trim_start_matches('/').replace('/', "\\");
    format!(r"\\wsl$\{distro}\{trimmed}")
}

/// Splits a WSL UNC (`\\wsl$\Ubuntu\home\alice` or `\\wsl.localhost\...`, either slash
/// direction, prefix case-insensitive) into `(distro, linux_path)`. `linux_path` is `/` for the
/// distro root. `(None, "")` for anything that is not a WSL UNC.
#[must_use]
pub fn try_parse_unc_path(path: &str) -> (Option<String>, String) {
    if path.trim().is_empty() {
        return (None, String::new());
    }
    let normalized = path.replace('/', "\\");
    let normalized = normalized.trim_end_matches('\\');
    for prefix in [r"\\wsl$\", r"\\wsl.localhost\"] {
        let has_prefix = normalized
            .get(..prefix.len())
            .is_some_and(|head| head.eq_ignore_ascii_case(prefix));
        if !has_prefix {
            continue;
        }
        let rest = normalized.get(prefix.len()..).unwrap_or("");
        if rest.is_empty() {
            return (None, String::new());
        }
        let (distro, linux_rest) = match rest.find('\\') {
            None => (rest, ""),
            Some(i) => (rest.get(..i).unwrap_or(""), rest.get(i + 1..).unwrap_or("")),
        };
        if distro.is_empty() {
            return (None, String::new());
        }
        let linux = if linux_rest.is_empty() {
            "/".to_string()
        } else {
            format!("/{}", linux_rest.replace('\\', "/"))
        };
        return (Some(distro.to_string()), linux);
    }
    (None, String::new())
}

/// The UNC mirror invariant: for a WSL session, `WorkingFolder` is re-derived from the distro
/// and Linux folder rather than trusted (it may be stale, hand-edited, or copied from a
/// RecentlyClosed entry or an import). Returns the new working folder, or `None` for a
/// non-WSL session (leave it alone).
#[must_use]
pub fn resync_wsl_working_folder(
    kind: crate::launch::SessionKind,
    distro: &str,
    wsl_working_folder: &str,
) -> Option<String> {
    (kind == crate::launch::SessionKind::Wsl)
        .then(|| to_unc_path(distro.trim(), wsl_working_folder))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launch::SessionKind;

    // Sample copied from `wsl -l -v` on a host with two distros installed. The leading
    // whitespace in front of "NAME" and the spacing are intentional: wsl pads with spaces.
    const SAMPLE: &str = "  NAME                   STATE           VERSION\n\
                          * Ubuntu                 Running         2\n\
                          \x20 Debian                 Stopped         2\n";

    #[test]
    fn parse_two_distros_returns_both() {
        assert_eq!(parse_distros(SAMPLE).len(), 2);
    }

    #[test]
    fn parse_marks_default_distro() {
        let r = parse_distros(SAMPLE);
        assert_eq!(r.iter().filter(|d| d.is_default).count(), 1);
        assert_eq!(r[0].name, "Ubuntu"); // default sorted first
    }

    #[test]
    fn parse_version_and_state() {
        let r = parse_distros(SAMPLE);
        let ubuntu = r.iter().find(|d| d.name == "Ubuntu").unwrap();
        assert_eq!(ubuntu.version, 2);
        assert_eq!(ubuntu.state, "Running");
    }

    #[test]
    fn parse_empty_input_returns_empty() {
        assert!(parse_distros("").is_empty());
        assert!(parse_distros("   \n").is_empty());
    }

    #[test]
    fn parse_header_only_returns_empty() {
        assert!(parse_distros("  NAME                   STATE           VERSION\n").is_empty());
    }

    #[test]
    fn parse_non_default_then_default_orders_default_first() {
        let reversed = "  NAME       STATE           VERSION\n\
                        \x20 Debian     Stopped         2\n\
                        * Ubuntu     Running         2\n";
        let r = parse_distros(reversed);
        assert_eq!(r[0].name, "Ubuntu");
        assert!(r[0].is_default);
    }

    #[test]
    fn parse_distro_name_with_space() {
        // `wsl --import "My Distro" ...`: NAME spans two tokens, so STATE/VERSION are read
        // from the end of the row.
        let raw = "  NAME             STATE           VERSION\n\
                   * My Distro        Running         2\n";
        let r = parse_distros(raw);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name, "My Distro");
        assert_eq!(r[0].state, "Running");
        assert_eq!(r[0].version, 2);
        assert!(r[0].is_default);
    }

    #[test]
    fn parse_localised_header_is_not_a_phantom_distro() {
        // German `wsl -l -v` header: no literal NAME, VERSION column is a word.
        let raw =
            "  NAME   STATUS   VERSION\n  Bezeichnung   Status   Version\n* Ubuntu Running 2\n";
        let r = parse_distros(raw);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name, "Ubuntu");
    }

    #[test]
    fn parse_sorts_non_defaults_alphabetically_ignoring_case() {
        let raw = "  NAME STATE VERSION\n  zeta Running 2\n  Alpha Running 2\n  beta Running 1\n";
        let names: Vec<_> = parse_distros(raw).into_iter().map(|d| d.name).collect();
        assert_eq!(names, ["Alpha", "beta", "zeta"]);
    }

    #[test]
    fn parse_tolerates_crlf() {
        let r = parse_distros(&SAMPLE.replace('\n', "\r\n"));
        assert_eq!(r.len(), 2);
        assert_eq!(r[1].state, "Stopped");
    }

    #[test]
    fn parse_utf16_input_from_wsl_exe() {
        // wsl.exe writes UTF-16LE; the C# reader set StandardOutputEncoding = Unicode.
        let mut bytes = vec![0xFF, 0xFE];
        for u in SAMPLE.encode_utf16() {
            bytes.extend_from_slice(&u.to_le_bytes());
        }
        let text = decode_wsl_output(&bytes);
        assert_eq!(text, SAMPLE);
        assert_eq!(parse_distros(&text).len(), 2);

        // without a BOM
        let no_bom: Vec<u8> = SAMPLE.encode_utf16().flat_map(u16::to_le_bytes).collect();
        assert_eq!(decode_wsl_output(&no_bom), SAMPLE);
        // and plain UTF-8 still works
        assert_eq!(decode_wsl_output(SAMPLE.as_bytes()), SAMPLE);
        // an odd trailing byte or invalid data never panics
        let _ = decode_wsl_output(&[0xFF, 0xFE, 0x41]);
        let _ = decode_wsl_output(&[0x00]);
        let _ = decode_wsl_output(&[]);
    }

    #[test]
    fn to_unc_path_happy_path() {
        assert_eq!(
            to_unc_path("Ubuntu", "/home/alice/proj"),
            r"\\wsl$\Ubuntu\home\alice\proj"
        );
    }

    #[test]
    fn to_unc_path_no_linux_path_returns_distro_root() {
        assert_eq!(to_unc_path("Ubuntu", ""), r"\\wsl$\Ubuntu");
    }

    #[test]
    fn to_unc_path_no_distro_returns_empty() {
        assert_eq!(to_unc_path("", "/home/x"), "");
    }

    #[test]
    fn try_parse_unc_path_known_shapes() {
        for (path, distro, linux) in [
            (r"\\wsl$\Ubuntu\home\alice", Some("Ubuntu"), "/home/alice"),
            (
                r"\\wsl.localhost\Debian\srv\app",
                Some("Debian"),
                "/srv/app",
            ),
            (r"\\WSL$\Ubuntu\", Some("Ubuntu"), "/"),
            ("//wsl$/Ubuntu/home/alice", Some("Ubuntu"), "/home/alice"),
            (r"\\wsl$\Ubuntu", Some("Ubuntu"), "/"),
            (r"\\wsl$\", None, ""),
            (r"\\wsl$\\home\alice", None, ""),
            (r"C:\proj", None, ""),
            ("", None, ""),
        ] {
            let (d, l) = try_parse_unc_path(path);
            assert_eq!(d.as_deref(), distro, "{path}");
            assert_eq!(l, linux, "{path}");
        }
    }

    #[test]
    fn try_parse_unc_path_never_panics_on_odd_input() {
        for p in [
            "\u{e9}\u{e9}\u{e9}",
            r"\\wsl",
            "\\\\wsl$",
            "ä\\\\wsl$\\x",
            "\0",
        ] {
            let _ = try_parse_unc_path(p);
        }
    }

    #[test]
    fn to_unc_and_back_round_trips() {
        let unc = to_unc_path("My Distro", "/home/a b/proj");
        let (d, l) = try_parse_unc_path(&unc);
        assert_eq!(d.as_deref(), Some("My Distro"));
        assert_eq!(l, "/home/a b/proj");
    }

    #[test]
    fn resync_re_derives_from_distro_and_linux_folder() {
        // A hand-edited / stale RecentlyClosed entry: WorkingFolder points somewhere
        // unrelated (the UNC mirror invariant).
        assert_eq!(
            resync_wsl_working_folder(SessionKind::Wsl, "Ubuntu", "/home/alice/proj").as_deref(),
            Some(r"\\wsl$\Ubuntu\home\alice\proj")
        );
    }

    #[test]
    fn resync_non_wsl_session_leaves_working_folder_alone() {
        assert_eq!(resync_wsl_working_folder(SessionKind::Local, "", ""), None);
        assert_eq!(resync_wsl_working_folder(SessionKind::Ssh, "x", "/y"), None);
    }

    #[test]
    fn parse_filters_docker_desktop_distro() {
        let raw = "  NAME                   STATE           VERSION\n\
                   * Ubuntu                 Running         2\n\
                   \x20 docker-desktop         Running         2\n";
        let r = parse_distros(raw);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name, "Ubuntu");
    }

    #[test]
    fn parse_filters_docker_desktop_data_case_insensitive() {
        let raw = "  NAME                       STATE           VERSION\n\
                   \x20 DOCKER-DESKTOP-DATA        Running         2\n";
        assert!(parse_distros(raw).is_empty());
    }

    #[test]
    fn parse_keeps_name_that_only_contains_the_docker_phrase() {
        let raw = "  NAME                       STATE           VERSION\n\
                   \x20 my-docker-desktop-clone    Running         2\n";
        let r = parse_distros(raw);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].name, "my-docker-desktop-clone");
    }

    #[test]
    fn parse_only_docker_distros_yields_empty_list() {
        let raw = "  NAME                       STATE           VERSION\n\
                   * docker-desktop             Running         2\n\
                   \x20 docker-desktop-data        Stopped         2\n";
        assert!(parse_distros(raw).is_empty());
    }
}
