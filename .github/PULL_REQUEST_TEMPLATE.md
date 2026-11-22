## Summary

<!-- One paragraph: what does this PR do, end-to-end? -->

## Why

<!-- The motivation: bug, user-visible issue, design pressure. Link the issue if there is one. -->

## Implementation notes

<!-- Optional: anything non-obvious about the approach, trade-offs you considered, or invariants the reviewer should keep in mind. Skip if the diff speaks for itself. -->

## Test plan

<!-- Bulleted checklist the reviewer can run through. Include both the golden path and any edge cases you specifically thought about. -->

- [ ] 

## Checklist

- [ ] `cargo +1.97 test --workspace --no-fail-fast` passes
- [ ] `cargo +1.97 clippy --workspace --all-targets -- -D warnings` passes
- [ ] `cargo +1.97 xtask ci` passes (run `cargo xtask tools` if you added or changed a tool)
- [ ] A new user-visible action is a tool, with honest effects (see `docs/tools.md`)

## Screenshots

<!-- Optional: before/after for UI changes. -->
