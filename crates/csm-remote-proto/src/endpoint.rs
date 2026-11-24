//! The host part of a Web Push endpoint, checked the same way by the relay (which POSTs to it) and by the
//! add-on (which chooses the endpoints a phone may register). One rule, so the two cannot disagree.

/// The host of an `https://` endpoint, lowercase; `None` for anything else: another scheme, userinfo
/// (`@`), a backslash, a port other than 443, an empty host, a host with characters outside
/// `[A-Za-z0-9.-]`, or a leading or trailing dot.
pub fn push_endpoint_host(endpoint: &str) -> Option<String> {
    let rest = endpoint.strip_prefix("https://")?;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.strip_suffix(":443").unwrap_or(authority);
    if host.is_empty() || !host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-') || host.starts_with('.') || host.ends_with('.') {
        return None;
    }
    Some(host.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_is_the_authority_without_userinfo_or_odd_characters() {
        assert_eq!(push_endpoint_host("https://FCM.googleapis.com:443/x").as_deref(), Some("fcm.googleapis.com"));
        assert_eq!(push_endpoint_host("https://x.notify.windows.com/w/?token=1").as_deref(), Some("x.notify.windows.com"));
        assert_eq!(push_endpoint_host("https://evil.example\\@x.notify.windows.com/"), None, "a backslash is not a host character");
        assert_eq!(push_endpoint_host("https://evil.example@x.notify.windows.com/"), None, "userinfo is refused");
        assert_eq!(push_endpoint_host("https://fcm.googleapis.com:8443/x"), None, "only port 443");
        assert_eq!(push_endpoint_host("http://fcm.googleapis.com/x"), None, "https only");
        assert_eq!(push_endpoint_host("https:///x"), None, "an empty host");
        assert_eq!(push_endpoint_host("https://.example.com/x"), None);
    }
}
