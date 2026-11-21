//! Add-ons (docs/addons.md): the manifest format, parsed and validated. Pure: the folder scan
//! and the enable state live in `csm-services` and `csm-engine`.

pub mod manifest;

pub use manifest::{
    AddonError, AddonKind, AddonMeta, CommandSpec, Problem, ProcessSpec, ProfileSpec, ProfileStatus, ParsedAddon, ResumeRule, RunTemplateSpec,
    SlashSpec, StatusRule, API_VERSION, match_status, parse, plain_token, valid_id,
};
