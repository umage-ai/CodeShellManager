//! Test-only crate: the phone remote's integration tests live in `tests/`, where they start the
//! real relay (`csm-relay`) in-process on 127.0.0.1 and drive it with the host (`csm-phone`) and a
//! Rust test phone. It sits above both in the layer map so that neither has to depend upward.
#![forbid(unsafe_code)]
