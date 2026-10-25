//! Plugins in the core: the plugin manager shares the agents' tool registry,
//! so tools of running plugins go through the same Security Policy Engine.

use std::sync::Arc;

use jarvis_events::EventBus;
use jarvis_platform::current_adapter;
use jarvis_plugins::{ClientInfo, PluginManager, PluginManagerOptions};
use jarvis_security::SecretStore;

use crate::agents::AgentSystem;
use crate::error::CoreError;
use crate::{AppPaths, JarvisCore};

/// Opens `plugins.json` in the protected settings folder. Servers without a
/// working folder of their own run in `<data>/plugins/<id>`.
pub(crate) async fn open(
    paths: &AppPaths,
    app_version: &str,
    agents: &AgentSystem,
    secrets: &Arc<dyn SecretStore>,
    events: &EventBus,
) -> Result<PluginManager, CoreError> {
    let options = PluginManagerOptions::new(
        paths.config_dir.join("plugins.json"),
        paths.data_dir.join("plugins"),
        Arc::clone(&agents.tools),
        Arc::clone(secrets),
        events.clone(),
        Arc::from(current_adapter()),
        ClientInfo {
            name: "JARVIS".to_owned(),
            version: app_version.to_owned(),
        },
    );
    Ok(PluginManager::open(options).await?)
}

impl JarvisCore {
    /// Plugins (MCP servers): reviews, approvals, lifecycle and tools.
    pub fn plugins(&self) -> &PluginManager {
        &self.plugins
    }
}
