mod advanced;
mod basic;

pub(crate) use advanced::{advanced_section, multimodal_section, remote_section};
pub(super) use basic::{
    api_section, appearance_section, context_section, models_section, subagent_section,
    workspace_section,
};
