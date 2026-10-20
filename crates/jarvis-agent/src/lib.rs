//! JARVIS agents: a Task Manager (states, limits, persisted journal) and an
//! Agent Orchestrator (planner → executor → verifier, with recovery) that
//! uses tools only through the Security Policy Engine.
//!
//! The model is reached through [`AgentModel`], implemented by the host
//! (the core chooses provider and model and applies the privacy rules).

mod error;
mod orchestrator;
mod protocol;
mod store;
mod types;

pub use error::{AgentError, Result};
pub use orchestrator::{
    AgentEnvironment, AgentManager, AgentModel, AgentSettings, NewTask, SettingsSource,
};
pub use store::TaskStore;
pub use types::*;
