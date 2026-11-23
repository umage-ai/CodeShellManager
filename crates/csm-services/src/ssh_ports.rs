//! Ports on SSH sessions: which ssh arguments name the connection, what `ssh -G` says it forwards,
//! and how a link to a loopback address on the remote host maps onto a local forward.
//!
//! Pure: no process, no file, no network. The engine runs `ssh -G` and `ssh -N -L` with the
//! arguments built here (argv, never a shell), on a worker thread. Everything read from ssh
//! output or from a link is untrusted: parsing returns `None` or skips the line, never panics.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// `ssh -G` output lines read at most (a config dump is short; this bounds a hostile one).
const MAX_CONFIG_LINES: usize = 4096;

/// Options that take the next argument (kept, with their value, when deriving the connection).
const VALUE_OPTIONS: [&str; 6] = ["-F", "-p", "-i", "-J", "-o", "-l"];

/// The ssh arguments that name a connection: the options of [`VALUE_OPTIONS`] with their values,
/// and the destination. `-t`, `-T` and the remote command are dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    /// The arguments, without the `ssh` program name: options, then the destination.
    pub args: Vec<String>,
    /// The destination as ssh was given it (`user@host` or a host alias).
    pub destination: String,
}

impl Connection {
    /// `ssh -G <args>`: the effective configuration for this connection (no connection is made).
    pub fn config_args(&self) -> Vec<String> {
        std::iter::once("-G".to_owned()).chain(self.args.iter().cloned()).collect()
    }

    /// `ssh <args> -N -o ExitOnForwardFailure=yes -o BatchMode=yes -L 127.0.0.1:<local>:127.0.0.1:<remote>`:
    /// a forward that holds the connection open and fails at once rather than prompting.
    /// The local side is bound to 127.0.0.1 only.
    pub fn forward_args(&self, local_port: u16, remote_port: u16) -> Vec<String> {
        let mut out = self.args.clone();
        out.extend(["-N", "-o", "ExitOnForwardFailure=yes", "-o", "BatchMode=yes", "-L"].map(str::to_owned));
        out.push(format!("127.0.0.1:{local_port}:127.0.0.1:{remote_port}"));
        out
    }
}

/// Derives the connection from the ssh argv an SSH session runs (`ssh -F cfg -t host cmd...`,
/// with or without the leading `ssh`). Refused when there is no destination, or when the
/// destination would be read as an option.
pub fn connection_of(argv: &[String]) -> Result<Connection, String> {
    let mut words = argv.iter().map(String::as_str).peekable();
    if words.peek().is_some_and(|w| *w == "ssh") {
        words.next();
    }
    let mut args = Vec::new();
    while let Some(word) = words.next() {
        if word == "--" {
            break;
        }
        if VALUE_OPTIONS.contains(&word) {
            let value = words.next().filter(|v| !v.is_empty()).ok_or_else(|| format!("ssh option {word} has no value"))?;
            args.push(word.to_owned());
            args.push(value.to_owned());
        } else if word.starts_with('-') && word.len() > 1 {
            // `-t`, `-T`, `-N` and the like: not part of the connection. An attached value
            // (`-p2222`, `-oKey=value`) is kept: it is the session's own option.
            if VALUE_OPTIONS.iter().any(|o| word.starts_with(o)) {
                args.push(word.to_owned());
            }
        } else {
            if word.is_empty() || word.starts_with('-') {
                return Err("the ssh destination is empty or starts with '-'".into());
            }
            args.push(word.to_owned());
            return Ok(Connection { args, destination: word.to_owned() });
        }
    }
    Err("the ssh command names no destination".into())
}

/// One `LocalForward` from `ssh -G` output: local `listen_port` to `target_host:target_port`
/// as the remote side sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalForward {
    pub listen_port: u16,
    pub target_host: String,
    pub target_port: u16,
}

/// The `localforward` lines of `ssh -G` output (`localforward <bind>:<port> <host>:<hostport>`;
/// the bind part may be a bare port). Lines that do not parse are skipped.
pub fn parse_local_forwards(config: &str) -> Vec<LocalForward> {
    config
        .lines()
        .take(MAX_CONFIG_LINES)
        .filter_map(|line| {
            let mut words = line.split_whitespace();
            if !words.next()?.eq_ignore_ascii_case("localforward") {
                return None;
            }
            let (listen, target) = (words.next()?, words.next()?);
            if words.next().is_some() {
                return None;
            }
            let (_, listen_port) = endpoint(listen)?;
            let (target_host, target_port) = endpoint(target)?;
            Some(LocalForward { listen_port, target_host, target_port })
        })
        .collect()
}

/// `host:port`, `[v6]:port` or a bare `port`, as `(host, port)`; the port is 1..=65535.
fn endpoint(text: &str) -> Option<(String, u16)> {
    let (host, port) = if let Some(rest) = text.strip_prefix('[') {
        let (inside, after) = rest.split_once(']')?;
        (inside.to_owned(), after.strip_prefix(':')?)
    } else if let Some((host, port)) = text.rsplit_once(':') {
        (host.to_owned(), port)
    } else {
        (String::new(), text)
    };
    let port: u16 = port.parse().ok()?;
    (port != 0).then_some((host, port))
}

/// True for the names and addresses a browser on this machine would send to the loopback
/// interface: `localhost`, `*.localhost`, `127.0.0.0/8` and `::1`. `127.example.com` is not one.
pub fn is_loopback_host(host: &str) -> bool {
    let host = host.trim_start_matches('[').trim_end_matches(']').trim_end_matches('.').to_ascii_lowercase();
    if host == "localhost" {
        return true;
    }
    if let Some(label) = host.strip_suffix(".localhost") {
        return !label.is_empty();
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(v4)) => v4.octets().first() == Some(&127),
        Ok(IpAddr::V6(v6)) => v6 == Ipv6Addr::LOCALHOST,
        Err(_) => false,
    }
}

/// The local port that forwards to `remote_port` on the remote host's loopback, when `forwards`
/// (from `ssh -G`) has one. The first match wins, as ssh's own order does.
pub fn forwarded_local_port(forwards: &[LocalForward], remote_port: u16) -> Option<u16> {
    forwards.iter().find(|f| f.target_port == remote_port && is_loopback_host(&f.target_host)).map(|f| f.listen_port)
}

/// An `http` or `https` URL split into its parts (only what the port rewrite needs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpUrl {
    /// `http` or `https`, lower case.
    pub scheme: String,
    /// The host as a name or an address, without brackets.
    pub host: String,
    /// The port: the one written, or 80 / 443.
    pub port: u16,
    /// Everything after the authority: path, query and fragment (may be empty).
    pub rest: String,
}

/// Splits an absolute `http://` or `https://` URL. `None` for anything else.
pub fn split_http_url(url: &str) -> Option<HttpUrl> {
    let (scheme, after) = url.split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let default_port = match scheme.as_str() {
        "http" => 80,
        "https" => 443,
        _ => return None,
    };
    let end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let (authority, rest) = after.split_at(end);
    // Userinfo is dropped: a rewritten link never carries credentials.
    let authority = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let (host, port) = if let Some(inner) = authority.strip_prefix('[') {
        let (v6, tail) = inner.split_once(']')?;
        (v6.to_owned(), tail.strip_prefix(':'))
    } else {
        match authority.matches(':').count() {
            0 => (authority.to_owned(), None),
            // More than one colon without brackets is not a host and port: refused.
            1 => {
                let (h, p) = authority.split_once(':')?;
                (h.to_owned(), Some(p))
            }
            _ => return None,
        }
    };
    if host.is_empty() {
        return None;
    }
    let port = match port {
        None | Some("") => default_port,
        Some(p) => p.parse::<u16>().ok().filter(|p| *p != 0)?,
    };
    Some(HttpUrl { scheme, host, port, rest: rest.to_owned() })
}

/// The loopback host and port a link points at, when it is an http(s) link to the loopback
/// interface.
pub fn loopback_target(url: &str) -> Option<(String, u16)> {
    let parts = split_http_url(url)?;
    is_loopback_host(&parts.host).then_some((parts.host, parts.port))
}

/// The same link pointing at a local forward's port. A localhost name stays; anything else
/// (a loopback address) becomes `127.0.0.1`, since the forward listens there. `None` when the
/// URL is not an http(s) link.
pub fn rewrite_to_local_port(url: &str, local_port: u16) -> Option<String> {
    let parts = split_http_url(url)?;
    let host = if is_localhost_name(&parts.host) { parts.host.clone() } else { Ipv4Addr::LOCALHOST.to_string() };
    Some(format!("{}://{host}:{local_port}{}", parts.scheme, parts.rest))
}

fn is_localhost_name(host: &str) -> bool {
    let h = host.to_ascii_lowercase();
    h == "localhost" || h.strip_suffix(".localhost").is_some_and(|label| !label.is_empty())
}

/// A port number from a tool parameter: 1..=65535.
pub fn check_port(value: i64) -> Result<u16, String> {
    u16::try_from(value).ok().filter(|p| *p != 0).ok_or_else(|| format!("{value} is not a port (1 to 65535)"))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::indexing_slicing)]
    use super::*;

    fn argv(s: &str) -> Vec<String> {
        s.split_whitespace().map(str::to_owned).collect()
    }

    #[test]
    fn nexus_form_keeps_the_config_and_drops_the_remote_command() {
        let a = argv(r"ssh -F C:\Users\me\.csm\nexus_ssh_config -t agent-1 tmux new-session -A -s main");
        let c = connection_of(&a).unwrap();
        assert_eq!(c.args, [r"-F", r"C:\Users\me\.csm\nexus_ssh_config", "agent-1"]);
        assert_eq!(c.destination, "agent-1");
        assert_eq!(c.config_args(), ["-G", "-F", r"C:\Users\me\.csm\nexus_ssh_config", "agent-1"]);
    }

    #[test]
    fn a_config_path_with_spaces_stays_one_argument() {
        // The launcher hands argv elements; a path with spaces is one element.
        let a: Vec<String> = ["ssh", "-F", "C:/Program Files/csm cfg/ssh config", "-t", "box", "bash"].map(String::from).to_vec();
        let c = connection_of(&a).unwrap();
        assert_eq!(c.args[1], "C:/Program Files/csm cfg/ssh config");
        assert_eq!(c.args.len(), 3);
    }

    #[test]
    fn port_user_identity_jump_and_options_are_kept() {
        let a = argv("ssh -p 2222 -i key.pem -J bastion -o StrictHostKeyChecking=no -l al -T -N dev.example.com ls -la");
        let c = connection_of(&a).unwrap();
        assert_eq!(c.args, ["-p", "2222", "-i", "key.pem", "-J", "bastion", "-o", "StrictHostKeyChecking=no", "-l", "al", "dev.example.com"]);
        assert_eq!(c.destination, "dev.example.com");
    }

    #[test]
    fn attached_values_are_kept_and_flags_are_dropped() {
        let c = connection_of(&argv("ssh -4 -v -p2222 -oBatchMode=yes user@h -t")).unwrap();
        assert_eq!(c.args, ["-p2222", "-oBatchMode=yes", "user@h"]);
    }

    #[test]
    fn a_destination_after_the_remote_command_is_not_taken() {
        let c = connection_of(&argv("ssh -t box -- tmux")).unwrap();
        assert_eq!(c.destination, "box");
        assert_eq!(c.args, ["box"]);
    }

    #[test]
    fn refusals_do_not_panic() {
        assert!(connection_of(&argv("ssh")).is_err());
        assert!(connection_of(&argv("ssh -t")).is_err());
        assert!(connection_of(&argv("ssh -F")).is_err());
        assert!(connection_of(&["ssh".into(), "-F".into(), String::new()]).is_err());
        assert!(connection_of(&[]).is_err());
        assert!(connection_of(&["ssh".into(), String::new()]).is_err());
        assert!(connection_of(&["ssh".into(), "-oProxyCommand=calc".into()]).is_err());
    }

    #[test]
    fn forward_args_bind_locally_and_refuse_prompts() {
        let c = connection_of(&argv("ssh -F cfg agent")).unwrap();
        assert_eq!(
            c.forward_args(3001, 3000),
            ["-F", "cfg", "agent", "-N", "-o", "ExitOnForwardFailure=yes", "-o", "BatchMode=yes", "-L", "127.0.0.1:3001:127.0.0.1:3000"]
        );
    }

    #[test]
    fn parses_lowercase_localforward_lines() {
        let out = "host agent\nhostname 10.0.0.5\nlocalforward 3000 localhost:3000\nlocalforward 127.0.0.1:8080 127.0.0.1:80\nlocalforward [::1]:9090 [::1]:9091\n";
        let f = parse_local_forwards(out);
        assert_eq!(
            f,
            vec![
                LocalForward { listen_port: 3000, target_host: "localhost".into(), target_port: 3000 },
                LocalForward { listen_port: 8080, target_host: "127.0.0.1".into(), target_port: 80 },
                LocalForward { listen_port: 9090, target_host: "::1".into(), target_port: 9091 },
            ]
        );
        assert_eq!(forwarded_local_port(&f, 3000), Some(3000));
        assert_eq!(forwarded_local_port(&f, 80), Some(8080));
        assert_eq!(forwarded_local_port(&f, 9091), Some(9090));
        assert_eq!(forwarded_local_port(&f, 5000), None);
    }

    #[test]
    fn a_forward_to_a_non_loopback_host_is_not_a_match() {
        let f = parse_local_forwards("localforward 5432 db.internal:5432\n");
        assert_eq!(f.len(), 1);
        assert_eq!(forwarded_local_port(&f, 5432), None);
    }

    #[test]
    fn garbage_config_never_panics() {
        for junk in [
            "",
            "localforward",
            "localforward 3000",
            "localforward 3000 localhost",
            "localforward 0 localhost:0",
            "localforward 99999 localhost:3000",
            "localforward [::1 localhost:3000",
            "localforward [::1]: localhost:3000",
            "localforward :: ::",
            "localforward a:b c:d",
            "localforward 1 2 3 4 5",
            "\u{0}\u{1}localforward \u{ffff}",
        ] {
            assert!(parse_local_forwards(junk).iter().all(|f| f.listen_port != 0 && f.target_port != 0), "{junk:?}");
        }
        let many = "localforward 1 localhost:1\n".repeat(MAX_CONFIG_LINES + 50);
        assert_eq!(parse_local_forwards(&many).len(), MAX_CONFIG_LINES);
    }

    #[test]
    fn loopback_names_and_addresses() {
        for host in ["localhost", "LOCALHOST", "localhost.", "127.0.0.1", "127.9.8.7", "::1", "[::1]", "app.localhost", "a.b.localhost"] {
            assert!(is_loopback_host(host), "{host}");
        }
        for host in [
            "127.example.com",
            "localhost.example.com",
            "example.com",
            "10.0.0.1",
            "::2",
            "localhost-evil.com",
            ".localhost",
            "1.127.0.0.1.nip.io",
            "",
        ] {
            assert!(!is_loopback_host(host), "{host}");
        }
    }

    #[test]
    fn loopback_urls_and_their_ports() {
        assert_eq!(loopback_target("http://localhost:3000/a?b=1#c"), Some(("localhost".into(), 3000)));
        assert_eq!(loopback_target("http://127.0.0.1/"), Some(("127.0.0.1".into(), 80)));
        assert_eq!(loopback_target("https://[::1]:8443"), Some(("::1".into(), 8443)));
        assert_eq!(loopback_target("http://x.localhost:5173"), Some(("x.localhost".into(), 5173)));
        assert_eq!(loopback_target("http://127.example.com:3000"), None);
        assert_eq!(loopback_target("https://example.com"), None);
        assert_eq!(loopback_target("ftp://localhost"), None);
        assert_eq!(loopback_target("javascript:alert(1)"), None);
    }

    #[test]
    fn rewrite_keeps_the_path_and_the_name() {
        assert_eq!(rewrite_to_local_port("http://localhost:3000/x?y=1#z", 3001).as_deref(), Some("http://localhost:3001/x?y=1#z"));
        assert_eq!(rewrite_to_local_port("http://127.0.0.1:3000", 3001).as_deref(), Some("http://127.0.0.1:3001"));
        assert_eq!(rewrite_to_local_port("http://[::1]:3000/", 3001).as_deref(), Some("http://127.0.0.1:3001/"));
        assert_eq!(rewrite_to_local_port("http://app.localhost:3000", 3001).as_deref(), Some("http://app.localhost:3001"));
        assert_eq!(rewrite_to_local_port("https://user:pw@localhost:8443/", 9000).as_deref(), Some("https://localhost:9000/"));
        assert_eq!(rewrite_to_local_port("file:///etc/passwd", 9000), None);
    }

    #[test]
    fn split_refuses_bad_urls_without_panicking() {
        for bad in ["http://", "http://:3000", "http://[::1", "http://h:0", "http://h:70000", "http://h:x", "https:/h", "", "http://h:1:2"] {
            assert_eq!(split_http_url(bad), None, "{bad}");
        }
    }

    #[test]
    fn ports_are_checked() {
        assert_eq!(check_port(1), Ok(1));
        assert_eq!(check_port(65535), Ok(65535));
        assert!(check_port(0).is_err());
        assert!(check_port(65536).is_err());
        assert!(check_port(-1).is_err());
    }
}
