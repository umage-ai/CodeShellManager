# csm-remote-proto

The wire protocol of the CSM phone remote, as pure Rust (no I/O, no clock, no randomness), shared by
the add-on, the relay (`relay/`) and, through `vectors/*.json`, the phone web app.

```
phone (PWA) --wss--> relay <--wss-- CSM add-on        Noise IK, end to end: the relay sees ciphertext
```

## Layers

1. **Relay layer** (`relay`): what the relay parses.
   - Text WebSocket messages are JSON objects tagged by `t`. Host to relay: `hello` (host id, host
     secret, optional enrolment token), `allow` (token hashes), `pairing_open` / `pairing_close`,
     `push`. Relay to host: `welcome`, `allow_ok`, `pairing_ok`, `push_result`, `error`. Phone to
     relay: one `hello` with `token` (routing token) or `pairing_id`. Relay to phone: `ready` or
     `error`.
   - Binary messages on the **host** socket are frames: `version(1) kind(1) channel(u32 BE)
     payload`; kinds `data` (1), `open` (2, relay to host: `0x00 token_hash(32)` or `0x01
     pairing_id(16)`), `close` (3, `u16 BE` refusal number or 0). Binary messages on a **phone**
     socket are the raw Noise messages (one socket = one channel).
   - Refusals are `RefusalCode`s (`codes`): stable names and numbers; WebSocket close code
     `4000 + number`.
2. **Noise** (`noise`): `Noise_IKpsk2_25519_AESGCM_SHA256` for pairing (PSK from the QR),
   `Noise_IK_25519_AESGCM_SHA256` afterwards; prologue `"csm-remote/1" 0x00 host_id`; every
   primitive exists in WebCrypto. One app message = one Noise message; rekey every 10 000 messages
   per direction (both ends, by counting). The 4-digit check code of the handshake hash is shown at
   pairing.
3. **Pairing** (`pairing`): the QR offer `{v, r (relay), h (host id), k (host static key), i
   (pairing id), p (PSK)}` as base64url JSON in the URL fragment of
   `<app origin>/v/<version>/#p=...`; inside the encrypted channel the phone sends `pair_hello`
   (a name), the PC answers `pair_accept` (routing token, level) or `pair_deny`.
4. **App messages** (`msg`): JSON in Noise messages, at most 48 KiB. Requests `{"id","op","args"}`
   with a **fixed op set** (`Op`; there is no tool id, free text or terminal access), responses
   `{"id","ok","data"|"error"}`, events `{"ev": "snapshot"|"delta"|"inbox"|"bye"}`. Two access
   levels: `viewer` (read, own preferences) and `operator` (wake/sleep/restart/close/new/command/
   clear status). `Op::required_level`, `Op::validate` and `Request::parse` are the add-on's gate.

`ids`: `host_id = base32(SHA-256("csm-relay host" || host_secret))[..26]`, routing hash
`SHA-256("csm-relay token" || token)`. `limits`: every cap.

## Test vectors

`vectors/*.json` are checked against the code by `tests/vectors.rs` (and regenerated with
`CSM_UPDATE_VECTORS=1 cargo test -p csm-remote-proto --test vectors`). A JS implementation is
tested against the same files:

| File | Covers |
|---|---|
| `ids.json` | host id, token hash, check code, prologue |
| `codes.json` | refusal codes |
| `frames.json` | binary frames, open/close payloads, control message JSON, invalid frames |
| `pairing.json` | the QR payload, URL, invalid offers, pairing messages |
| `messages.json` | requests (valid and invalid, with the error code and level), responses, events, data types, push payloads |
| `noise.json` | both handshakes with fixed static and ephemeral keys (every byte reproducible), transport messages, rekey across the 10 000 boundary |

Noise is exercised with `snow` (dev-dependency); the crate itself does not link a Noise library.
