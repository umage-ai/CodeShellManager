# csm-remote-proto, vendored

The wire protocol of the CSM phone remote, copied into CodeShellManager so that CSM builds without
access to its source repository, which is private for now.

- **Source:** `github.com/umage-ai/csm-relay` (private), `crates/csm-remote-proto`
- **Commit:** `fa8b35b046d9ee404adca1b69586c52ef4e676e7` (the csm-relay HEAD at the time of the copy)
- **Changes:** `src/`, `vectors/` and `README.md` are byte-identical to the source. `Cargo.toml` differs
  only where workspace inheritance cannot work, because `vendor/*` is outside the CSM workspace:
  - `version`, `edition`, `license`, `rust-version` and `publish` are written out as literal values
    (the same values as the csm-relay workspace).
  - `[lints] workspace = true` is replaced by the same `unsafe_code` and clippy denies written out.
  - The `[dev-dependencies]` section (`snow`) is removed, and `tests/` is omitted: the vector tests read
    `phone/test/fixtures/` from the csm-relay repository, which CSM does not have. The vector tests stay
    in csm-relay, where they run against this same code.
  - `LICENSE-MIT.txt` is a copy of the csm-relay repository's `LICENSE` (MIT, umage.ai), added for the
    vendored code. It is not part of the crate.
- **Used by:** `crates/csm-peer` (path dependency `../../vendor/csm-remote-proto`).
- **Updating:** copy the crate again from the csm-relay commit you want, reapply the `Cargo.toml` changes
  above, and record the new commit here. Check `cargo test -p csm-peer` and the real-relay test
  (`crates/csm-peer/tests/peer_relay.rs`, run with `CSM_RELAY_BIN`) after an update.
- **Future:** once csm-relay is public, this becomes a git or crates.io dependency and the copy goes.
