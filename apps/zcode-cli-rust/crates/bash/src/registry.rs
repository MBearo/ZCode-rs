//! The fig command registry (`generated/bash-command-registry.ts`), parsed on
//! first use: only "always allow" suggestions need it.
use serde::Deserialize;
use std::{collections::HashMap, sync::OnceLock};

/// `[names, takesArg]`.
#[derive(Deserialize)]
pub(crate) struct CommandOption(pub Vec<String>, pub u8);

/// `[names, options, argFlags, children]`.
#[derive(Deserialize)]
pub(crate) struct Node(
    pub Vec<String>,
    pub Vec<CommandOption>,
    pub u32,
    pub Vec<Node>,
);

pub(crate) const ARG_IS_COMMAND: u32 = 1;
pub(crate) const ARG_IS_MODULE: u32 = 2;

#[derive(Deserialize)]
struct Registry {
    registry: HashMap<String, Node>,
}

/// Own keys only: a lowercased executable name never hits the JS prototype.
pub(crate) fn command(name: &str) -> Option<&'static Node> {
    static REGISTRY: OnceLock<HashMap<String, Node>> = OnceLock::new();
    REGISTRY
        .get_or_init(|| {
            let parsed: Registry =
                serde_json::from_str(include_str!("../schema/command-registry.json"))
                    .expect("bash command registry");
            parsed.registry
        })
        .get(name)
}
