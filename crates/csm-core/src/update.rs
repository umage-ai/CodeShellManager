//! The update check's rules (pure): which GitHub releases feed to read, how a release list is
//! parsed (the payload is untrusted), and which release, if any, to offer.
//!
//! Ported from the WPF app's `UpdateService` (0.x): GitHub's `releases/latest` of
//! `umage-ai/CodeShellManager`, compared with the running version, a newer one shown as a badge
//! that opens the release page. Nothing is downloaded. The I/O is `csm-services::update`; the
//! schedule and the tools are `csm-engine`'s `update_check`.
//!
//! **Which releases are offered** (docs/release.md, "Update check"):
//!
//! - A **final** build (`2.0.0`) reads `releases/latest`, which GitHub never fills with a
//!   pre-release, and is offered only final releases newer than itself.
//! - A **pre-release** build (`2.1.0-beta.3`, `2.0.0-rc.1`) reads the release list instead
//!   (`releases/latest` cannot name a pre-release), and is offered the newest of: final releases,
//!   and pre-releases *of its own kind* (the first pre-release identifier: a beta sees newer betas,
//!   an rc newer rcs), newer than itself. A beta is never offered an rc: an rc is the stable
//!   channel's MSI, which replaces the beta install.
//! - Releases below 2.0.0 (the WPF app's own, 0.x) and drafts are never offered.
//! - The link is built from the release's validated tag, never taken from the payload's URLs.

use semver::Version;
use serde_json::Value;

/// The repository releases are read from (the WPF app's and version 2's).
pub const REPO: &str = "umage-ai/CodeShellManager";
/// `releases/latest`: the newest final release (what 0.x read).
pub const LATEST_URL: &str = "https://api.github.com/repos/umage-ai/CodeShellManager/releases/latest";
/// The newest releases, pre-releases included (pre-release builds only).
pub const LIST_URL: &str = "https://api.github.com/repos/umage-ai/CodeShellManager/releases?per_page=30";
/// Where a release's page is.
const RELEASE_PAGE: &str = "https://github.com/umage-ai/CodeShellManager/releases/tag/";
/// A payload bigger than this is not a release list (30 releases with long notes are well under).
pub const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;
/// At most this many releases of a list are looked at.
const MAX_RELEASES: usize = 100;
/// A longer tag is not one of ours.
const MAX_TAG_LEN: usize = 64;

/// Which feed a build reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Feed {
    /// `releases/latest` (final builds).
    Latest,
    /// The release list (pre-release builds).
    List,
}

impl Feed {
    pub fn url(self) -> &'static str {
        match self {
            Feed::Latest => LATEST_URL,
            Feed::List => LIST_URL,
        }
    }

    pub fn id(self) -> &'static str {
        match self {
            Feed::Latest => "latest",
            Feed::List => "list",
        }
    }
}

/// The running version, parsed (`None`: not semver, nothing can be compared).
pub fn parse_version(s: &str) -> Option<Version> {
    let s = s.trim();
    let s = s.strip_prefix('v').or_else(|| s.strip_prefix('V')).unwrap_or(s);
    Version::parse(s).ok()
}

/// The feed a build of `current` reads.
pub fn feed_for(current: &Version) -> Feed {
    if current.pre.is_empty() { Feed::Latest } else { Feed::List }
}

/// Whether the app checks on its own: only a released version 2 build. A local or development
/// build (the workspace's `0.0.1`) never does; `update_check` still runs when asked.
pub fn auto_check_applies(current: &str) -> bool {
    parse_version(current).is_some_and(|v| v.major >= 2)
}

/// The User-Agent sent with the request (GitHub's API requires one). Only the version: nothing
/// that identifies the person or the machine.
pub fn user_agent(current: &str) -> String {
    let v: String = current.chars().filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+')).take(MAX_TAG_LEN).collect();
    format!("CodeShellManager/{v}")
}

/// One release from the payload, already checked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    /// The tag as published (`v2.1.0`).
    pub tag: String,
    pub version: Version,
    /// GitHub's `prerelease` flag (or a pre-release version).
    pub prerelease: bool,
}

/// What a check found newer than the running build.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct Offer {
    /// `2.1.0` (no `v`).
    pub version: String,
    pub tag: String,
    /// The release's page on GitHub, built from the tag.
    pub url: String,
    pub prerelease: bool,
}

/// Why a payload was not read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    #[error("the answer is too large to be a release list")]
    TooLarge,
    #[error("the answer is not JSON: {0}")]
    NotJson(String),
    #[error("the answer is not a release or a release list")]
    NotReleases,
}

/// The releases in a `releases/latest` object or a release list. Entries that are drafts, have
/// no usable tag, or are not semver are left out; a payload of the wrong shape is an error.
pub fn parse_releases(body: &str) -> Result<Vec<Release>, ParseError> {
    if body.len() > MAX_BODY_BYTES {
        return Err(ParseError::TooLarge);
    }
    let v: Value = serde_json::from_str(body.trim_start_matches('\u{feff}')).map_err(|e| ParseError::NotJson(e.to_string()))?;
    match &v {
        Value::Object(_) => Ok(release_of(&v).into_iter().collect()),
        Value::Array(list) => Ok(list.iter().take(MAX_RELEASES).filter_map(release_of).collect()),
        _ => Err(ParseError::NotReleases),
    }
}

/// A tag we can link to: `v` and a semver version made of ASCII letters, digits, dots and dashes.
fn tag_ok(tag: &str) -> bool {
    !tag.is_empty() && tag.len() <= MAX_TAG_LEN && tag.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-'))
}

fn release_of(v: &Value) -> Option<Release> {
    let o = v.as_object()?;
    if o.get("draft").and_then(Value::as_bool).unwrap_or(false) {
        return None;
    }
    let tag = o.get("tag_name")?.as_str()?;
    if !tag_ok(tag) {
        return None;
    }
    let version = parse_version(tag)?;
    let flagged = o.get("prerelease").and_then(Value::as_bool).unwrap_or(false);
    Some(Release { tag: tag.to_owned(), prerelease: flagged || !version.pre.is_empty(), version })
}

/// The pre-release kind: `beta` of `2.1.0-beta.3`, `rc` of `2.0.0-rc.1`; empty for a final.
fn kind(v: &Version) -> &str {
    v.pre.as_str().split('.').next().unwrap_or("")
}

/// The release to offer a build of `current`, if any (the rules are in the module docs).
pub fn pick(current: &Version, releases: &[Release]) -> Option<Offer> {
    releases
        .iter()
        .filter(|r| r.version.major >= 2 && r.version > *current)
        .filter(|r| {
            let final_release = !r.prerelease && r.version.pre.is_empty();
            final_release || (!current.pre.is_empty() && !r.version.pre.is_empty() && kind(&r.version) == kind(current))
        })
        .max_by(|a, b| a.version.cmp(&b.version))
        .map(|r| Offer {
            version: r.version.to_string(),
            tag: r.tag.clone(),
            url: format!("{RELEASE_PAGE}{}", r.tag),
            prerelease: !r.version.pre.is_empty(),
        })
}

/// A whole check's verdict from the payload: the offer (`None`: up to date).
pub fn evaluate(current: &str, body: &str) -> Result<Option<Offer>, String> {
    let cur = parse_version(current).ok_or_else(|| format!("the running version `{current}` is not a version number"))?;
    let releases = parse_releases(body).map_err(|e| e.to_string())?;
    Ok(pick(&cur, &releases))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        parse_version(s).unwrap()
    }

    fn rel(tag: &str, pre: bool) -> Release {
        Release { tag: tag.into(), version: v(tag), prerelease: pre }
    }

    fn offered(current: &str, list: &[Release]) -> Option<String> {
        pick(&v(current), list).map(|o| o.version)
    }

    #[test]
    fn versions_parse_with_or_without_v_and_garbage_does_not() {
        assert_eq!(v("v2.1.0"), Version::new(2, 1, 0));
        assert_eq!(v(" 2.0.0-beta.3 ").pre.as_str(), "beta.3");
        for bad in ["", "v", "2", "2.0", "2.0.0.1", "two", "2.0.0-", "v2.0.0 extra", "\u{0}"] {
            assert!(parse_version(bad).is_none(), "{bad:?}");
        }
    }

    #[test]
    fn a_final_build_reads_latest_and_a_prerelease_reads_the_list() {
        assert_eq!(feed_for(&v("2.0.0")), Feed::Latest);
        assert_eq!(feed_for(&v("2.1.0-beta.2")), Feed::List);
        assert_eq!(feed_for(&v("2.0.0-rc.1")), Feed::List);
    }

    #[test]
    fn only_released_version_2_builds_check_on_their_own() {
        assert!(auto_check_applies("2.0.0"));
        assert!(auto_check_applies("2.1.0-beta.1"));
        assert!(!auto_check_applies("0.0.1"), "a local build");
        assert!(!auto_check_applies("garbage"));
    }

    #[test]
    fn a_final_build_is_offered_newer_finals_only() {
        let list = [rel("v2.0.0", false), rel("v2.1.0", false), rel("v2.2.0-beta.1", true), rel("v2.2.0-rc.1", true)];
        assert_eq!(offered("2.0.0", &list), Some("2.1.0".into()));
        assert_eq!(offered("2.1.0", &list), None, "up to date: pre-releases are ignored");
        assert_eq!(offered("2.2.0", &list), None, "nothing newer");
        // A final tag GitHub flags as a pre-release is not offered to a final build either.
        assert_eq!(offered("2.0.0", &[rel("v2.3.0", true)]), None);
    }

    #[test]
    fn a_beta_sees_newer_betas_and_finals_but_never_an_rc() {
        let list = [rel("v2.1.0-beta.2", true), rel("v2.1.0-beta.10", true), rel("v2.1.0-rc.1", true), rel("v2.0.0", false)];
        assert_eq!(offered("2.1.0-beta.2", &list), Some("2.1.0-beta.10".into()), "numeric, not text, order");
        assert_eq!(offered("2.1.0-beta.10", &list), None, "the rc is not for a beta");
        let with_final = [rel("v2.1.0-beta.3", true), rel("v2.1.0", false)];
        assert_eq!(offered("2.1.0-beta.2", &with_final), Some("2.1.0".into()), "the final outranks its betas");
    }

    #[test]
    fn an_rc_sees_newer_rcs_and_finals() {
        let list = [rel("v2.0.0-beta.9", true), rel("v2.0.0-rc.2", true)];
        assert_eq!(offered("2.0.0-rc.1", &list), Some("2.0.0-rc.2".into()));
        assert_eq!(offered("2.0.0-rc.2", &[rel("v2.0.0", false)]), Some("2.0.0".into()));
    }

    #[test]
    fn the_wpf_apps_releases_are_never_offered() {
        assert_eq!(offered("0.0.1", &[rel("v0.11.0", false)]), None, "a local build and the 0.x latest");
        assert_eq!(offered("0.0.1", &[rel("v2.0.0", false)]), Some("2.0.0".into()));
    }

    #[test]
    fn the_link_is_built_from_the_tag() {
        let o = pick(&v("2.0.0"), &[rel("v2.0.1", false)]).unwrap();
        assert_eq!(o.url, "https://github.com/umage-ai/CodeShellManager/releases/tag/v2.0.1");
        assert_eq!(o.tag, "v2.0.1");
        assert!(!o.prerelease);
    }

    /// The shape of a real `releases/latest` answer (trimmed).
    const LATEST: &str = r#"{
      "url": "https://api.github.com/repos/umage-ai/CodeShellManager/releases/1",
      "html_url": "https://github.com/umage-ai/CodeShellManager/releases/tag/v2.1.0",
      "id": 1, "tag_name": "v2.1.0", "name": "CodeShellManager 2.1.0",
      "draft": false, "prerelease": false, "created_at": "2026-12-01T10:00:00Z",
      "assets": [{"name": "CodeShellManager-2.1.0-Setup.msi", "browser_download_url": "https://example.com/x.msi"}],
      "body": "notes"
    }"#;

    #[test]
    fn a_latest_payload_is_read() {
        assert_eq!(evaluate("2.0.0", LATEST).unwrap().map(|o| o.version), Some("2.1.0".into()));
        assert_eq!(evaluate("2.1.0", LATEST).unwrap(), None);
        assert!(evaluate("not a version", LATEST).is_err());
    }

    #[test]
    fn a_list_payload_is_read_and_drafts_are_skipped() {
        let body = r#"[
          {"tag_name": "v2.2.0-beta.1", "draft": true, "prerelease": true},
          {"tag_name": "v2.1.0-beta.4", "draft": false, "prerelease": true},
          {"tag_name": "v2.0.0", "prerelease": false}
        ]"#;
        let list = parse_releases(body).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(evaluate("2.1.0-beta.3", body).unwrap().map(|o| o.version), Some("2.1.0-beta.4".into()));
    }

    #[test]
    fn hostile_payloads_offer_nothing_and_never_panic() {
        // The html_url is never used: the link is always the release page of the tag.
        let swapped = r#"{"tag_name": "v9.0.0", "html_url": "file:///C:/Windows/System32/calc.exe"}"#;
        assert_eq!(evaluate("2.0.0", swapped).unwrap().unwrap().url, "https://github.com/umage-ai/CodeShellManager/releases/tag/v9.0.0");
        for tag in [
            r#""v2.1.0\" --x""#,
            r#""v2.1.0 & calc""#,
            r#""../../v2.1.0""#,
            r#""v2.1.0+build""#,
            r#""v2.1.0?x=1""#,
            r#""v2.1.0/../..""#,
            r#""v2.1.0\n""#,
            r#""""#,
            r#"2"#,
            r#"null"#,
            r#"["v2.1.0"]"#,
            r#"{"x": "v2.1.0"}"#,
        ] {
            let body = format!(r#"{{"tag_name": {tag}}}"#);
            assert_eq!(evaluate("2.0.0", &body).unwrap(), None, "{tag}");
        }
        let long = format!(r#"{{"tag_name": "v2.1.0-{}"}}"#, "a".repeat(200));
        assert_eq!(evaluate("2.0.0", &long).unwrap(), None, "an over-long tag");
        // A huge version number does not overflow.
        assert_eq!(evaluate("2.0.0", r#"{"tag_name": "v99999999999999999999.0.0"}"#).unwrap(), None);
        // Wrong types for the flags are read as absent.
        assert!(evaluate("2.0.0", r#"{"tag_name": "v2.1.0", "draft": "yes", "prerelease": 1}"#).unwrap().is_some());
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        for body in ["", "   ", "<html>rate limited</html>", "{", "[1, 2", "42", "\"v2.1.0\"", "null", "true"] {
            assert!(evaluate("2.0.0", body).is_err(), "{body:?}");
        }
        // GitHub's own error object (rate limit, not found) is an object without a tag: nothing offered.
        assert_eq!(evaluate("2.0.0", r#"{"message": "API rate limit exceeded", "documentation_url": "https://docs.github.com"}"#).unwrap(), None);
        assert_eq!(evaluate("2.0.0", "[]").unwrap(), None);
        assert_eq!(evaluate("2.0.0", "[1, \"x\", null, [], {}]").unwrap(), None);
        let big = format!("[{}]", "{},".repeat(MAX_BODY_BYTES / 3) + "{}");
        assert_eq!(parse_releases(&big), Err(ParseError::TooLarge));
        // Deep nesting is refused by serde_json's recursion limit, as an error.
        let deep = "[".repeat(10_000) + &"]".repeat(10_000);
        assert!(parse_releases(&deep).is_err());
        // A BOM is tolerated.
        assert!(evaluate("2.0.0", &format!("\u{feff}{LATEST}")).unwrap().is_some());
    }

    #[test]
    fn the_user_agent_carries_only_the_version() {
        assert_eq!(user_agent("2.1.0-beta.3"), "CodeShellManager/2.1.0-beta.3");
        assert_eq!(user_agent("2.0.0\r\nX-Evil: 1"), "CodeShellManager/2.0.0X-Evil1");
    }
}
