//! The update check's one request (blocking: call it from a worker thread, never the UI or the
//! engine thread). The rules (which feed, what is offered) are `csm_core::update`.
//!
//! One HTTPS GET to GitHub's API with a fixed deadline, HTTPS only (redirects too), the system's
//! proxy settings, and a capped body. The only header that says anything about the caller is a
//! generic User-Agent (`CodeShellManager/<version>`): no id, no cookie, no token.

use std::time::Duration;

use csm_core::update::MAX_BODY_BYTES;

/// The whole request, connect to last byte, takes at most this long.
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// GETs `url` (an `https://api.github.com` URL from `csm_core::update::Feed`) and returns the
/// body. A 404 (no release published yet) is an empty list; another status is an error.
pub fn fetch(url: &str, current_version: &str) -> Result<String, String> {
    if !url.starts_with("https://api.github.com/") {
        return Err(format!("not a GitHub API address: {url}"));
    }
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(TIMEOUT))
        .https_only(true)
        .max_redirects(3)
        .http_status_as_error(false)
        .user_agent(csm_core::update::user_agent(current_version))
        .build()
        .into();
    let mut resp = agent
        .get(url)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .call()
        .map_err(|e| format!("could not reach GitHub: {e}"))?;
    let status = resp.status().as_u16();
    if status == 404 {
        return Ok("[]".into());
    }
    if !(200..300).contains(&status) {
        return Err(format!("GitHub answered {status}"));
    }
    resp.body_mut().with_config().limit(MAX_BODY_BYTES as u64).read_to_string().map_err(|e| format!("could not read GitHub's answer: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_github_api_is_asked() {
        // Refused before any connection is made.
        assert!(fetch("http://api.github.com/repos/x", "2.0.0").unwrap_err().contains("not a GitHub API address"));
        assert!(fetch("https://example.com/", "2.0.0").unwrap_err().contains("not a GitHub API address"));
        assert!(fetch("file:///C:/x", "2.0.0").is_err());
    }
}
