//! "Open on GitHub": which GitHub repository a checkout's remote points at, and the web pages
//! for it. Pure: [`super::run::github_repo`] runs the one `git config` read.
//!
//! Only ever produces `https://github.com/<owner>/<repo>...`: the host is fixed, owner and repo
//! are checked against GitHub's own character rules (anything else is "not GitHub"), and the
//! branch is percent-encoded. Credentials in a remote URL (`https://token@github.com/...`) are
//! never carried into the page address.

/// The web origin every URL here starts with.
const ORIGIN: &str = "https://github.com";

/// A repository on github.com.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubRepo {
    pub owner: String,
    pub repo: String,
}

impl GithubRepo {
    /// `https://github.com/owner/repo`.
    #[must_use]
    pub fn url(&self) -> String {
        format!("{ORIGIN}/{}/{}", self.owner, self.repo)
    }

    /// The branch's page (`…/tree/<branch>`), or the repository's default page when the branch is
    /// unknown or detached (`None` or empty).
    #[must_use]
    pub fn branch_url(&self, branch: Option<&str>) -> String {
        match branch.filter(|b| !b.trim().is_empty()) {
            Some(b) => format!("{}/tree/{}", self.url(), encode_branch(b)),
            None => self.url(),
        }
    }

    /// The pull requests page.
    #[must_use]
    pub fn pulls_url(&self) -> String {
        format!("{}/pulls", self.url())
    }
}

/// `git config --null --get-regexp` for every remote's URL and every branch's tracked remote.
/// The pattern is one argv element; no shell sees it.
#[must_use]
pub fn remote_config_args() -> Vec<String> {
    ["config", "--null", "--get-regexp", r"^(remote\..+\.url|branch\..+\.remote)$"].map(String::from).to_vec()
}

/// The GitHub repository for a checkout, from [`remote_config_args`]' output: `origin` when it
/// is a GitHub URL, else the remote `branch` tracks when that one is. `None` otherwise.
#[must_use]
pub fn pick(config: &str, branch: Option<&str>) -> Option<GithubRepo> {
    let mut urls: Vec<(&str, &str)> = Vec::new();
    let mut tracked: Option<&str> = None;
    // `--null`: entries end in NUL; the key ends at the first newline, the value is the rest.
    for entry in config.split('\0') {
        let entry = entry.trim_start_matches(['\r', '\n']);
        let Some((key, value)) = entry.split_once('\n') else { continue };
        if let Some(name) = key.strip_prefix("remote.").and_then(|k| k.strip_suffix(".url")) {
            urls.push((name, value));
        } else if let (Some(b), Some(name)) = (branch, key.strip_prefix("branch.").and_then(|k| k.strip_suffix(".remote")))
            && name == b
        {
            tracked = Some(value);
        }
    }
    let url_of = |remote: &str| urls.iter().rev().find(|(n, _)| *n == remote).and_then(|(_, u)| parse_remote_url(u));
    url_of("origin").or_else(|| tracked.filter(|t| *t != ".").and_then(url_of))
}

/// A GitHub repository from a remote URL: `https://github.com/owner/repo(.git)`,
/// `git@github.com:owner/repo(.git)` or `ssh://git@github.com/owner/repo(.git)`. Any other
/// host, shape or character is `None`.
#[must_use]
pub fn parse_remote_url(url: &str) -> Option<GithubRepo> {
    let url = url.trim();
    if url.chars().any(|c| c.is_control() || c.is_whitespace()) {
        return None;
    }
    let path = if let Some(rest) = strip_prefix_ci(url, "https://") {
        let (authority, path) = rest.split_once('/')?;
        // Credentials (`user:token@`) may precede the host; they are dropped.
        let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
        host.eq_ignore_ascii_case("github.com").then_some(path)?
    } else if let Some(rest) = strip_prefix_ci(url, "ssh://") {
        let (authority, path) = rest.split_once('/')?;
        authority.eq_ignore_ascii_case("git@github.com").then_some(path)?
    } else {
        let (authority, path) = url.split_once(':')?;
        authority.eq_ignore_ascii_case("git@github.com").then_some(path)?
    };
    let path = path.strip_suffix('/').unwrap_or(path);
    let path = path.strip_suffix(".git").unwrap_or(path);
    let (owner, repo) = path.split_once('/')?;
    (valid_owner(owner) && valid_repo(repo)).then(|| GithubRepo { owner: owner.to_owned(), repo: repo.to_owned() })
}

fn strip_prefix_ci<'a>(s: &'a str, prefix: &str) -> Option<&'a str> {
    let head = s.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then(|| s.get(prefix.len()..)).flatten()
}

/// GitHub user and organisation names: 1-39 ASCII letters, digits and hyphens, not starting
/// with a hyphen.
fn valid_owner(s: &str) -> bool {
    (1..=39).contains(&s.len()) && !s.starts_with('-') && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// GitHub repository names: 1-100 ASCII letters, digits, `-`, `_` and `.`; never `.` or `..`.
fn valid_repo(s: &str) -> bool {
    (1..=100).contains(&s.len()) && s != "." && s != ".." && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

/// A branch name as URL path segments: `/` stays a separator, every byte outside RFC 3986's
/// unreserved set is `%XX`, and a segment of only dots is encoded whole (so it can never walk up
/// the path).
#[must_use]
pub fn encode_branch(branch: &str) -> String {
    branch
        .split('/')
        .map(|seg| {
            let all_dots = !seg.is_empty() && seg.bytes().all(|b| b == b'.');
            let mut out = String::with_capacity(seg.len());
            for b in seg.bytes() {
                if !all_dots && (b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')) {
                    out.push(char::from(b));
                } else {
                    out.push_str(&format!("%{b:02X}"));
                }
            }
            out
        })
        .collect::<Vec<_>>()
        .join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo(owner: &str, name: &str) -> Option<GithubRepo> {
        Some(GithubRepo { owner: owner.into(), repo: name.into() })
    }

    #[test]
    fn the_three_remote_forms_parse() {
        for url in [
            "https://github.com/umage-ai/CodeShellManager",
            "https://github.com/umage-ai/CodeShellManager.git",
            "https://github.com/umage-ai/CodeShellManager/",
            "HTTPS://GitHub.com/umage-ai/CodeShellManager.git",
            "https://x-access-token:ghp_secret@github.com/umage-ai/CodeShellManager.git",
            "git@github.com:umage-ai/CodeShellManager.git",
            "git@github.com:umage-ai/CodeShellManager",
            "ssh://git@github.com/umage-ai/CodeShellManager",
            "ssh://git@github.com/umage-ai/CodeShellManager.git",
            "  https://github.com/umage-ai/CodeShellManager  ",
        ] {
            assert_eq!(parse_remote_url(url), repo("umage-ai", "CodeShellManager"), "{url}");
        }
        assert_eq!(parse_remote_url("git@github.com:a/b.c_d-e"), repo("a", "b.c_d-e"));
    }

    #[test]
    fn anything_else_is_not_github() {
        for url in [
            "",
            "https://gitlab.com/a/b",
            "https://github.com.evil.com/a/b",
            "https://evil.com/github.com/a/b",
            "https://github.com@evil.com/a/b",
            "https://github.com:443/a/b",
            "http://github.com/a/b",
            "git://github.com/a/b",
            "file:///C:/src/repo",
            "C:\\src\\repo",
            "../other",
            "git@gitlab.com:a/b.git",
            "evil@github.com:a/b.git",
            "ssh://git@github.com:22/a/b",
            "https://github.com/a",
            "https://github.com/a/b/c",
            "https://github.com/a/b/tree/main",
            "https://github.com//b",
            "https://github.com/a/",
            "https://github.com/a/.git",
            "https://github.com/a/..",
            "https://github.com/-a/b",
            "https://github.com/a_b/c",
            "https://github.com/a/b?x=1",
            "https://github.com/a/b#x",
            "https://github.com/a/b%2Fc",
            "https://github.com/a/b c",
            "https://github.com/a/b\nhttps://github.com/c/d",
            "https://github.com/a/\u{0430}",
            "https://github.com/a/b\"onclick=x",
            "javascript:alert(1)//github.com/a/b",
        ] {
            assert_eq!(parse_remote_url(url), None, "{url:?}");
        }
        let long = format!("https://github.com/{}/b", "a".repeat(40));
        assert_eq!(parse_remote_url(&long), None);
        let long = format!("https://github.com/a/{}", "b".repeat(101));
        assert_eq!(parse_remote_url(&long), None);
    }

    #[test]
    fn urls_are_github_pages_with_the_branch_encoded() {
        let r = GithubRepo { owner: "o".into(), repo: "r".into() };
        assert_eq!(r.url(), "https://github.com/o/r");
        assert_eq!(r.pulls_url(), "https://github.com/o/r/pulls");
        assert_eq!(r.branch_url(None), "https://github.com/o/r");
        assert_eq!(r.branch_url(Some("")), "https://github.com/o/r", "detached/unknown: the default page");
        assert_eq!(r.branch_url(Some("main")), "https://github.com/o/r/tree/main");
        assert_eq!(r.branch_url(Some("feat/brand-facet")), "https://github.com/o/r/tree/feat/brand-facet");
        assert_eq!(r.branch_url(Some("a b#c?d&e%f")), "https://github.com/o/r/tree/a%20b%23c%3Fd%26e%25f");
        assert_eq!(r.branch_url(Some("../../evil")), "https://github.com/o/r/tree/%2E%2E/%2E%2E/evil");
        assert_eq!(r.branch_url(Some("v1.2")), "https://github.com/o/r/tree/v1.2");
        assert_eq!(r.branch_url(Some("ünï")), "https://github.com/o/r/tree/%C3%BCn%C3%AF");
        assert_eq!(r.branch_url(Some("x\"><script>")), "https://github.com/o/r/tree/x%22%3E%3Cscript%3E");
        for b in ["//", "@{-1}", "a:b", "\\\\server\\x", "\u{202e}"] {
            let u = r.branch_url(Some(b));
            assert!(u.starts_with("https://github.com/o/r/tree/"), "{u}");
            assert!(u.bytes().all(|c| c.is_ascii_graphic()), "{u}");
        }
    }

    #[test]
    fn origin_first_else_the_tracked_remote() {
        let cfg = "remote.origin.url\nhttps://gitlab.com/a/b\0remote.gh.url\ngit@github.com:o/r.git\0branch.main.remote\ngh\0";
        assert_eq!(pick(cfg, Some("main")), repo("o", "r"), "origin is not GitHub: main tracks gh");
        assert_eq!(pick(cfg, Some("other")), None, "nothing GitHub tracked");
        assert_eq!(pick(cfg, None), None, "detached");
        let cfg = "remote.upstream.url\nhttps://github.com/u/r\0remote.origin.url\nhttps://github.com/o/r.git\0branch.main.remote\nupstream\0";
        assert_eq!(pick(cfg, Some("main")), repo("o", "r"), "origin wins");
        // Dotted remote and branch names; `.` (a local branch) tracks nothing remote.
        let cfg = "remote.my.fork.url\nssh://git@github.com/f/r\0branch.feat/x.y.remote\nmy.fork\0branch.loc.remote\n.\0";
        assert_eq!(pick(cfg, Some("feat/x.y")), repo("f", "r"));
        assert_eq!(pick(cfg, Some("loc")), None);
        // Hostile or empty output.
        for bad in ["", "\0\0", "garbage", "remote.origin.url", "remote.origin.url\n\0", "remote.origin.url\nhttps://github.com/a/b\nextra\0"] {
            assert_eq!(pick(bad, Some("main")), None, "{bad:?}");
        }
        assert_eq!(pick("remote.origin.url\nhttps://github.com/a/b", Some("main")), repo("a", "b"), "no trailing NUL");
    }
}
