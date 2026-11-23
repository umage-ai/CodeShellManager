//! The add-on's own policy, separate from CSM's: folder matching, the names a phone may see, the
//! launch rate, the live-session cap and the in-flight cap. Pure apart from [`InFlight`], which
//! is a counter shared between threads.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use csm_remote_proto::msg::{MAX_FOLDER_CHARS, PhoneStatus};

/// Launches (`session.new`, `session.wake`, `session.restart`) per device, per minute and per hour.
pub const LAUNCHES_PER_MINUTE: usize = 6;
pub const LAUNCHES_PER_HOUR: usize = 30;
/// CSM-bound requests in flight across all channels (the same number CSM allows the add-on).
pub const MAX_IN_FLIGHT: usize = 8;

/// A folder on the allow list, as the add-on uses it: its own spelling (passed to CSM) and the key
/// that matching compares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Folder {
    pub spelling: String,
    pub pinned: bool,
    pub key: String,
}

/// The comparison key of an absolute folder path, or `None` when the path is not one this add-on
/// accepts: relative, containing `..`, control characters, or too long.
pub fn canon_folder(path: &str) -> Option<CanonFolder> {
    if path.is_empty() || path.chars().count() > MAX_FOLDER_CHARS || path.chars().any(char::is_control) {
        return None;
    }
    let b = path.as_bytes();
    let drive = b.len() >= 3 && b.first().is_some_and(u8::is_ascii_alphabetic) && b.get(1) == Some(&b':') && matches!(b.get(2), Some(b'/' | b'\\'));
    let root = if drive {
        path.get(..2)?.to_owned()
    } else if path.starts_with('/') || path.starts_with('\\') {
        "/".to_owned()
    } else {
        return None;
    };
    let rest = if drive { path.get(2..)? } else { path };
    let mut parts = Vec::new();
    for seg in rest.split(['/', '\\']) {
        match seg {
            "" | "." => {}
            ".." => return None,
            s => parts.push(s),
        }
    }
    let mut key = format!("{root}\u{1}{}", parts.join("\u{1}"));
    if cfg!(windows) {
        key = key.to_lowercase();
    }
    Some(CanonFolder { key })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CanonFolder {
    pub key: String,
}

/// Format and control characters a phone or a push must never carry: bidi overrides, isolates,
/// zero-width characters and the byte-order mark (section 4.1). `char::is_control` covers Cc.
fn is_hidden_format(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{061C}' | '\u{180E}'
        | '\u{200B}'..='\u{200F}'
        | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{2064}'
        | '\u{2066}'..='\u{206F}'
        | '\u{FEFF}'
        | '\u{FFF9}'..='\u{FFFB}')
}

/// Removes control and format characters, trims, and keeps at most `max_chars` characters. The
/// result may be empty; callers that need a name fall back themselves.
pub fn clean_name(s: &str, max_chars: usize) -> String {
    let kept: String = s.chars().filter(|c| !c.is_control() && !is_hidden_format(*c)).collect();
    cut(kept.trim(), max_chars)
}

/// The first `max_chars` characters of `s`.
pub fn cut(s: &str, max_chars: usize) -> String {
    s.chars().take(max_chars).collect()
}

/// Per-device launch rate: a sliding window of times (seconds) for each of the two windows.
#[derive(Debug, Default, Clone)]
pub struct LaunchBucket {
    minute: VecDeque<u64>,
    hour: VecDeque<u64>,
}

impl LaunchBucket {
    /// Records a launch at `now` (Unix seconds) if both windows have room. A refused launch is
    /// not recorded, so it does not push the next one out.
    pub fn admit(&mut self, now: u64) -> bool {
        while self.minute.front().is_some_and(|t| now.saturating_sub(*t) >= 60) {
            self.minute.pop_front();
        }
        while self.hour.front().is_some_and(|t| now.saturating_sub(*t) >= 3600) {
            self.hour.pop_front();
        }
        if self.minute.len() >= LAUNCHES_PER_MINUTE || self.hour.len() >= LAUNCHES_PER_HOUR {
            return false;
        }
        self.minute.push_back(now);
        self.hour.push_back(now);
        true
    }
}

/// How many sessions a phone created are still live: ids a `session.new` made that the latest
/// `session_list` shows with a status other than `exited`. Sessions no longer listed do not count.
pub fn live_phone_sessions(created: &std::collections::BTreeSet<String>, listed: &BTreeMap<String, PhoneStatus>) -> usize {
    created.iter().filter(|id| listed.get(*id).is_some_and(|s| *s != PhoneStatus::Exited)).count()
}

/// The global cap on CSM-bound requests in flight. A request beyond the cap is refused at once
/// (`rate_limited`); it is never queued.
#[derive(Debug, Clone)]
pub struct InFlight {
    used: Arc<AtomicUsize>,
    cap: usize,
}

/// Holds one unit of the cap until dropped.
#[derive(Debug)]
pub struct Permit {
    used: Arc<AtomicUsize>,
}

impl InFlight {
    pub fn new(cap: usize) -> Self {
        Self { used: Arc::new(AtomicUsize::new(0)), cap }
    }

    pub fn try_take(&self) -> Option<Permit> {
        self.used.fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| (n < self.cap).then_some(n + 1)).ok().map(|_| Permit { used: Arc::clone(&self.used) })
    }

    pub fn in_use(&self) -> usize {
        self.used.load(Ordering::SeqCst)
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        self.used.fetch_sub(1, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folder_keys_refuse_relative_and_dotdot() {
        assert!(canon_folder("/a/b").is_some());
        assert!(canon_folder("a/b").is_none());
        assert!(canon_folder("/a/../b").is_none());
        assert!(canon_folder("C:\\a\\..\\b").is_none());
        assert!(canon_folder("/a\u{0}b").is_none());
        assert!(canon_folder("").is_none());
        assert!(canon_folder("C:relative").is_none());
    }

    #[test]
    fn folder_keys_normalise_separators_and_dots() {
        assert_eq!(canon_folder("/a//b/./c/"), canon_folder("/a/b/c"));
        assert_ne!(canon_folder("/a/b"), canon_folder("/a/bc"));
    }

    #[test]
    fn names_lose_bidi_and_zero_width_characters() {
        let hostile = "ok\u{202E}evil\u{200B}\u{2066}x\u{FEFF}\u{0007}";
        assert_eq!(clean_name(hostile, 64), "okevilx");
        assert_eq!(clean_name("  Build\tfix  ", 64), "Buildfix");
        assert_eq!(clean_name("abcdef", 3), "abc");
    }

    #[test]
    fn launch_bucket_refuses_the_seventh_in_a_minute() {
        let mut b = LaunchBucket::default();
        for _ in 0..LAUNCHES_PER_MINUTE {
            assert!(b.admit(1000));
        }
        assert!(!b.admit(1000), "the (N+1)th launch inside the window is refused");
        assert!(!b.admit(1059), "still inside the minute");
        assert!(b.admit(1060), "the window slides");
    }

    #[test]
    fn launch_bucket_has_an_hour_cap_too() {
        let mut b = LaunchBucket::default();
        for i in 0..LAUNCHES_PER_HOUR as u64 {
            assert!(b.admit(i * 100));
        }
        assert!(!b.admit(3000), "the 31st in the hour is refused");
        assert!(b.admit(3600), "the first has aged out");
    }

    #[test]
    fn live_count_ignores_exited_and_unlisted() {
        let created: std::collections::BTreeSet<String> = ["a", "b", "c"].iter().map(|s| (*s).to_owned()).collect();
        let mut listed = BTreeMap::new();
        listed.insert("a".to_owned(), PhoneStatus::Working);
        listed.insert("b".to_owned(), PhoneStatus::Exited);
        assert_eq!(live_phone_sessions(&created, &listed), 1);
    }

    #[test]
    fn in_flight_cap_refuses_and_releases() {
        let cap = InFlight::new(2);
        let a = cap.try_take();
        let b = cap.try_take();
        assert!(a.is_some() && b.is_some());
        assert!(cap.try_take().is_none(), "beyond the cap nothing is queued, it is refused");
        drop(a);
        let c = cap.try_take();
        assert!(c.is_some());
        assert_eq!(cap.in_use(), 2, "b and the new permit");
    }
}
