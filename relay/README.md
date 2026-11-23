# csm-relay

The relay of the CodeShellManager phone remote: a small server that joins one CSM installation
(the *host*, connected outbound) to the phones paired with it, so the phone works from any network
with no open port on the PC. Open source, self-hostable, one binary or one container.

**The relay is untrusted by design.** Everything a phone and CSM say to each other is end-to-end
encrypted (Noise, see `crates/csm-remote-proto`); the relay copies opaque frames and cannot read,
forge or replay them. It keeps all state in memory, has no accounts and no database, and logs
counts and refusal codes, never payloads. A hostile or compromised relay can drop or delay traffic
and learn who talks when and how much, nothing more.

umage runs a relay **for umage's own use**; it admits only hosts umage enrols (see Access control).
Everybody else runs their own: that is what this README is for.

## Run it

```sh
cargo run --release -p csm-relay -- --config relay/relay.toml     # from the repository root
# or the container, with automatic TLS through Caddy:
cp relay/relay.example.toml relay/relay.toml                      # edit [access] at least
docker compose -f relay/docker-compose.yml up -d --build
```

The relay speaks plain HTTP/WebSocket. Put TLS in front of it (the supplied `Caddyfile` does it
with automatic certificates; any reverse proxy that passes WebSocket upgrades works). Phones and
CSM connect to `wss://<your relay>/v1/...`.

Then, in CSM, point the Phone remote add-on at your relay (relay URL, plus the host's enrolment
token if you use one) and pair a phone.

### Endpoints

| Path | What |
|---|---|
| `GET /v1/host` | WebSocket: the CSM add-on |
| `GET /v1/client` | WebSocket: a phone (browser `Origin` checked when `allowed_origins` is set) |
| `GET /v1/vapid` | `{"public_key": "..."}`: the VAPID key phones subscribe to Web Push with (404 when push is off) |
| `GET /healthz` | `ok` |

Nothing else is served. The phone web app is **not** served by the relay: it lives on its own static
origin, because whoever serves the app's JavaScript can use the phone's keys.

## Configuration

A TOML file (`--config <file>` or `CSM_RELAY_CONFIG`), with environment overrides. Unknown keys are
an error. `relay.example.toml` documents every key.

| Setting | Environment | Default |
|---|---|---|
| `bind` | `CSM_RELAY_BIND` | `127.0.0.1:8787` |
| `trust_forwarded_for` | `CSM_RELAY_TRUST_FORWARDED_FOR` | `false` |
| `allowed_origins` | `CSM_RELAY_ALLOWED_ORIGINS` (comma list) | any |
| `log_format` | `CSM_RELAY_LOG_FORMAT` | `text` (or `json`); level from `CSM_RELAY_LOG` |
| `access.open` | `CSM_RELAY_OPEN` | `false` |
| `access.allowed_hosts` | `CSM_RELAY_ALLOWED_HOSTS` | none |
| `access.allowed_hosts_file` | `CSM_RELAY_ALLOWED_HOSTS_FILE` | none |
| `access.enrol_tokens` | `CSM_RELAY_ENROL_TOKENS` | none |
| `push.enabled` | `CSM_RELAY_PUSH` | `true` |
| `push.vapid_key_file` | `CSM_RELAY_VAPID_KEY_FILE` | `vapid.key` |
| `push.vapid_subject` | `CSM_RELAY_VAPID_SUBJECT` | `mailto:admin@example.invalid` (set yours) |
| `push.allowed_hosts` | `CSM_RELAY_PUSH_ALLOWED_HOSTS` | Apple, FCM, Mozilla, Windows push services |
| `limits.*` | `CSM_RELAY_MAX_HOSTS` only | see below |

`trust_forwarded_for`: take the client address from the last `X-Forwarded-For` entry. Turn it on
behind a proxy that sets the header and only when clients cannot reach the relay directly;
otherwise any client could pick its own address and dodge the per-IP limits.

## Access control (who may be a host)

The relay is a router for CSM installations, and an operator may want it to serve only their own.
Hosts are checked when they connect; **phones are not**: a phone gets in only through its host's
allow list, which is end-to-end authenticated by the host.

The relay is **closed unless you configure it**, and it refuses to start if it would admit nobody
(no `open`, no list, no token), because that is almost certainly a mistake. A host is admitted if
any rule says so:

1. `access.open = true`: every host. For a relay on your own network, or one you deliberately run
   for everyone.
2. `access.allowed_hosts` / `access.allowed_hosts_file`: the host's id is listed. The id
   (26 characters) is shown in CSM under Settings > Add-ons > Phone remote. The file is re-read at
   every host connection, so adding a line needs no restart.
3. `access.enrol_tokens`: the host presents a secret in its hello. Use this to hand a team one
   token instead of collecting ids. Each entry is the secret (16+ characters) or `sha256:<hex>`
   (`csm-relay enrol-token <secret>` prints it), so the config file need not hold the secret.
   Remove the entry and restart to cut a token off; hosts already connected stay until they
   reconnect.

Refusal is `not_enrolled`. The host id is `base32(SHA-256("csm-relay host" || host_secret))`; the
relay checks the secret the host sends (inside TLS) hashes to the id it claims, so only the
holder of the secret can be that host and a stranger cannot squat someone else's id.

This is access control for the operator, not security for the users: the E2E encryption does not
depend on it.

## What the relay enforces

| Limit (all configurable) | Default |
|---|---|
| Frame size | 64 KiB |
| Channels per host | 16 |
| Allow-list entries per host | 64 |
| Open sockets per IP | 20 |
| New connections per IP per minute | 10 |
| Frames / bytes per second per channel (burst 2x), each direction | 50 / 256 KiB |
| Host silent (no frame, no ping) | 90 s |
| Phone idle | 10 min |
| First message (hello) within | 10 s |
| Pairing slot | 10 min, at most 5 attempts, one pairing channel at a time |
| Hosts on the relay | 10 000 |
| Pushes per host / per endpoint per hour | 60 / 20 |

Refusals are stable codes (the phone turns them into plain messages), sent as
`{"t":"error","code":"..."}` and as the WebSocket close reason with close code `4000 + n`:

| Code | n | Meaning |
|---|---|---|
| `bad_request` | 1 | malformed or out-of-order message |
| `unsupported_version` | 2 | protocol version unknown |
| `not_allowed` | 3 | wrong host secret; token not on the host's allow list; unknown, expired or used-up pairing slot |
| `not_enrolled` | 4 | the operator's access control does not admit this host |
| `host_offline` | 5 | the host has no live connection |
| `rate_limited` | 6 | a rate limit or the per-IP socket cap |
| `relay_full` | 7 | global host cap, channels per host, allow-list size |
| `frame_too_large` | 8 | over the size cap |
| `idle_timeout` | 9 | nothing sent for too long |
| `revoked` | 10 | the device was removed from the host's allow list |
| `host_replaced` | 11 | a newer connection for the same host id took over |
| `push_refused` | 12 | push off, endpoint not allowed |
| `push_failed` | 13 | the push service did not accept it |

### Pairing slots and revocation

The host sends the relay an allow list (hashes of each paired device's routing token) and at most
one open pairing slot (an id, at most 10 minutes). A phone names the host and presents a routing
token or the slot id; the relay matches the hash in memory. Revoking a device is the host sending
a new list: that device's channel is closed at once with `revoked`. A slot ends when the host
closes it (after confirming or denying a device), on expiry, after 5 attempts, or when the host
disconnects. Everything is re-sent by the host when it reconnects, so a relay restart loses
nothing that matters.

## Web Push

The add-on encrypts each notification itself (RFC 8291) and sends the relay the endpoint and the
ciphertext. The relay adds the VAPID `Authorization` header (ES256 JWT, 12 h) and POSTs the opaque
body. It only POSTs to `push.allowed_hosts` (https, port 443, no userinfo) so a host cannot make
it fetch arbitrary URLs, and it caps pushes per host and per endpoint. Apple, Google and the relay
see ciphertext and timing, never the text.

The relay has one VAPID key pair for all its phones (a browser allows one push subscription per
service worker, so per-PC keys would break a phone paired with two PCs). It is created on first run
in `push.vapid_key_file` (keep that file; with Docker it is the `/data` volume). Phones fetch the
public half from `GET /v1/vapid`. Set `push.vapid_subject` to your own contact.

## Logs

Structured (`log_format = "json"` for JSON lines), through `tracing`. Info level has events
("connected", "channel open", "refused", "push forwarded") and a status line every 10 minutes with
host and channel counts. Host ids, tokens, IP addresses and payloads are never logged.

## Operating notes

- One process holds all state, so run one instance. Restart it freely: hosts reconnect within
  seconds (1-2-5-10-30 s backoff), re-send their allow lists, and phones reconnect when opened.
- Memory per host is a few kilobytes plus per-channel queues (at most 64 frames each).
- Cargo audit / `cargo deny check` runs at the repository root (`deny.toml`); the relay's direct
  dependencies are `axum`, `tokio`, `tracing`, `p256`, `ureq` (rustls with `ring`), `sha2`,
  `getrandom`, `serde`, `toml`; no copyleft.

## Tests

`cargo test -p csm-relay` starts the real relay on an ephemeral port and drives it with in-process
host and phone WebSocket clients: pairing, routing (byte-identical frames), isolation, revocation,
host replacement, every refusal code, rate limits, access control, push forwarding and throttles.

## CSM peers (the office and the laptop)

A CodeShellManager office can be reached through the relay too (see `docs/remote-access.md`). Its
endpoints are a separate namespace from the phones':

| Path | What |
|---|---|
| `GET /v1/peer-host` | WebSocket: a CSM office (the same hello as `/v1/host`; hosts are keyed by kind, so a phone never finds an office, nor the reverse) |
| `GET /v1/peer-client` | WebSocket: a CSM laptop (the same hello as `/v1/client`, with the device's routing token or the pairing id) |

Both refuse any request with an `Origin` header: a browser cannot open them. The relay copies the
Noise frames of the peer session exactly as it copies a phone's. Its limits for peer channels are
separate and higher, because terminal output bursts:

| Limit (`[limits]`) | Default |
|---|---|
| `peer_max_channels_per_host` | 16 |
| `peer_frames_per_second_per_channel` | 2000 (burst 2x) |
| `peer_bytes_per_second_per_channel` | 8 MiB |

A peer host has no push. Its client channels queue 1024 frames (phones: 64). The laptop acknowledges what
its reader has taken (a text `{"t":"ack","frames":N,"bytes":N}` on its socket, peer clients only); the relay
forwards it to the office as an `Ack` frame (counts only, never Noise bytes). The office keeps at most 512
frames and 2 MiB in flight per channel, so a well-behaved office never fills the queue: a slow laptop slows
the office's writes, and a well-behaved office never fills the relay's queue. A sustained rate over the
per-channel cap still closes it with `rate_limited`. The same access rules (`[access]`) admit an office as
they admit a phone host.

The office sets its relay in Settings › Remote access (`peer_relay_set`): the address, and the operator's
enrolment token when `[access]` asks for one. The office keeps the token and its relay secret in its OS
keychain, never in a file. It connects to the relay only while remote access is on, so a relay never
runs for an office that is not listening. The relay needs no change for this: an office's relay channel
is an ordinary peer host.

The office paces its own writes to the caps the relay announces in its welcome (`frames_per_second` and
`bytes_per_second` for the kind of host it is), so a burst from the terminal waits at the office and is not
cut off here. A relay that lowers `peer_*` is followed, not refused. A phone can never open a peer
channel and a peer can never open a phone channel: the namespaces are separate, and the relay tests
check both directions.

Tests: `relay/tests/relay.rs` (the namespaces, the Origin refusal, the peer limits, revocation) and
`relay/tests/peer_relay.rs` (a real office and a real laptop through an in-process relay on 127.0.0.1: a slow laptop reader keeps the link and the final screen;
pairing, connecting, revoking, the direct address winning when it answers, and the relay-only paths).

Crate tests: `csm-remote-proto` pins the `Ack` frame and its payload; `csm-peer` `relay_pump` tests the
window (a writer waits at the window and is released by an ack or by a close), and that a relay which stops
reading cannot hold a close, and that an Open arriving while the gate is full does not deadlock.
