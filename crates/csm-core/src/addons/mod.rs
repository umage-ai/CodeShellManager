//! Add-ons (docs/addons.md): the manifest format, parsed and validated, and the process add-on
//! protocol (wire types and validation, pure). The folder scan and the enable state live in
//! `csm-services` and `csm-engine`; the process itself is started by `csm-engine::plugins`.

pub mod manifest;
pub mod protocol;

pub use manifest::{
    AddonError, AddonKind, AddonMeta, CommandSpec, Problem, ProcessSpec, ProfileSpec, ProfileStatus, ParsedAddon, ResumeRule, RunTemplateSpec,
    SlashSpec, StatusRule, API_VERSION, match_status, parse, plain_token, valid_id,
};
pub use protocol::{DeclaredTool, Handshake, Incoming, ProcessEffect, RpcError};
