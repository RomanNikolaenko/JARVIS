//! Model router: picks a provider and model for a task category.
//!
//! Explicit user choices win. Otherwise the router chooses automatically
//! among available models that support the category, honouring the privacy
//! policy (cloud providers only when allowed) and preferring, in order:
//! real models over the diagnostic provider, on-device over local network
//! over cloud, then the user's provider order.

use jarvis_ai::{
    AiError, DataLocality, ModelCapabilities, ModelInfo, ProviderDescriptor, ProviderId,
    ProviderKind,
};
use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::catalog::ProviderModels;
use crate::config::PrivacySettings;
use crate::error::CoreError;

/// Kinds of work JARVIS routes to models. Speech categories arrive with the
/// voice engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub enum TaskCategory {
    General,
    Reasoning,
    Coding,
    Vision,
    Embeddings,
    ToolCalling,
}

impl TaskCategory {
    pub const ALL: [Self; 6] = [
        Self::General,
        Self::Reasoning,
        Self::Coding,
        Self::Vision,
        Self::Embeddings,
        Self::ToolCalling,
    ];

    /// Settings key of the category.
    pub fn key(self) -> &'static str {
        match self {
            Self::General => "general",
            Self::Reasoning => "reasoning",
            Self::Coding => "coding",
            Self::Vision => "vision",
            Self::Embeddings => "embeddings",
            Self::ToolCalling => "toolCalling",
        }
    }

    /// Whether a model with `caps` can serve this category.
    pub fn supports(self, caps: &ModelCapabilities) -> bool {
        match self {
            Self::General | Self::Reasoning | Self::Coding => caps.chat,
            Self::Vision => caps.chat && caps.vision,
            Self::ToolCalling => caps.chat && caps.tool_calling,
            Self::Embeddings => caps.embeddings,
        }
    }

    /// Category whose explicit choice is used when this one has none.
    fn inherits_from(self) -> Option<Self> {
        match self {
            Self::Reasoning | Self::Coding => Some(Self::General),
            _ => None,
        }
    }
}

/// Reference to a model of a specific provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub struct ModelRef {
    pub provider_id: ProviderId,
    pub model: String,
}

/// Explicit model choice per category; `None` means automatic.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", default)]
#[ts(export_to = "core.ts")]
pub struct ModelRouting {
    pub general: Option<ModelRef>,
    pub reasoning: Option<ModelRef>,
    pub coding: Option<ModelRef>,
    pub vision: Option<ModelRef>,
    pub embeddings: Option<ModelRef>,
    pub tool_calling: Option<ModelRef>,
}

impl ModelRouting {
    pub fn get(&self, category: TaskCategory) -> Option<&ModelRef> {
        match category {
            TaskCategory::General => self.general.as_ref(),
            TaskCategory::Reasoning => self.reasoning.as_ref(),
            TaskCategory::Coding => self.coding.as_ref(),
            TaskCategory::Vision => self.vision.as_ref(),
            TaskCategory::Embeddings => self.embeddings.as_ref(),
            TaskCategory::ToolCalling => self.tool_calling.as_ref(),
        }
    }

    /// All explicit choices.
    pub fn entries(&self) -> impl Iterator<Item = (TaskCategory, &ModelRef)> {
        TaskCategory::ALL
            .into_iter()
            .filter_map(|category| self.get(category).map(|r| (category, r)))
    }
}

/// How a category is currently routed (for display).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
#[ts(export_to = "core.ts")]
pub struct RouteResolution {
    pub category: TaskCategory,
    pub provider_id: Option<ProviderId>,
    pub model: Option<String>,
    /// `true` when no explicit choice applies and the router picked the model.
    pub automatic: bool,
    pub error: Option<String>,
}

/// A routing decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Selection {
    pub provider: ProviderDescriptor,
    pub model: ModelInfo,
    pub automatic: bool,
}

/// Picks a model for `category` from the current catalog.
pub(crate) fn select(
    category: TaskCategory,
    routing: &ModelRouting,
    catalog: &[ProviderModels],
    allow_cloud: bool,
) -> Result<Selection, CoreError> {
    let explicit = routing.get(category).or_else(|| {
        category
            .inherits_from()
            .and_then(|parent| routing.get(parent))
    });

    match explicit {
        Some(model_ref) => select_explicit(category, model_ref, catalog, allow_cloud),
        None => select_automatic(category, catalog, allow_cloud),
    }
}

fn select_explicit(
    category: TaskCategory,
    model_ref: &ModelRef,
    catalog: &[ProviderModels],
    allow_cloud: bool,
) -> Result<Selection, CoreError> {
    let entry = catalog
        .iter()
        .find(|p| p.provider.id == model_ref.provider_id)
        .ok_or_else(|| {
            CoreError::NotFound(format!(
                "AI provider \"{}\" selected for {} is not configured or disabled",
                model_ref.provider_id,
                category.key()
            ))
        })?;
    check_privacy(&entry.provider, allow_cloud)?;
    if let Some(error) = &entry.error {
        return Err(AiError::Unavailable(format!(
            "{} is unavailable: {error}",
            entry.provider.display_name
        ))
        .into());
    }
    let model = entry
        .models
        .iter()
        .find(|m| m.id == model_ref.model)
        .ok_or_else(|| {
            CoreError::NotFound(format!(
                "model \"{}\" selected for {} is not available on {}",
                model_ref.model,
                category.key(),
                entry.provider.display_name
            ))
        })?;
    if !category.supports(&model.capabilities) {
        return Err(CoreError::Validation(format!(
            "model \"{}\" does not support {} tasks",
            model.id,
            category.key()
        )));
    }
    Ok(Selection {
        provider: entry.provider.clone(),
        model: model.clone(),
        automatic: false,
    })
}

fn select_automatic(
    category: TaskCategory,
    catalog: &[ProviderModels],
    allow_cloud: bool,
) -> Result<Selection, CoreError> {
    catalog
        .iter()
        .enumerate()
        .filter(|(_, entry)| entry.error.is_none())
        .filter(|(_, entry)| allow_cloud || entry.provider.locality != DataLocality::ThirdPartyCloud)
        .flat_map(|(provider_index, entry)| {
            entry
                .models
                .iter()
                .enumerate()
                .filter(|(_, model)| category.supports(&model.capabilities))
                .map(move |(model_index, model)| {
                    let rank = (
                        entry.provider.kind == ProviderKind::Diagnostic,
                        locality_rank(entry.provider.locality),
                        provider_index,
                        model_index,
                    );
                    (rank, entry, model)
                })
        })
        .min_by_key(|(rank, _, _)| *rank)
        .map(|(_, entry, model)| Selection {
            provider: entry.provider.clone(),
            model: model.clone(),
            automatic: true,
        })
        .ok_or_else(|| {
            let blocked_cloud_model_exists = !allow_cloud
                && catalog.iter().any(|entry| {
                    entry.error.is_none()
                        && entry.provider.locality == DataLocality::ThirdPartyCloud
                        && entry.models.iter().any(|m| category.supports(&m.capabilities))
                });
            if blocked_cloud_model_exists {
                CoreError::PermissionDenied(format!(
                    "Only cloud models support {} tasks, and sending data to the cloud is disabled in privacy settings",
                    category.key()
                ))
            } else {
                CoreError::NotFound(format!(
                    "No available AI model supports {} tasks. Install a suitable model on a running server, or add a provider on the Models screen.",
                    category.key()
                ))
            }
        })
}

fn locality_rank(locality: DataLocality) -> u8 {
    match locality {
        DataLocality::OnDevice => 0,
        DataLocality::LocalNetwork => 1,
        DataLocality::ThirdPartyCloud => 2,
    }
}

/// Rejects cloud providers unless the user allowed them.
pub(crate) fn check_privacy(
    provider: &ProviderDescriptor,
    allow_cloud: bool,
) -> Result<(), CoreError> {
    if provider.locality == DataLocality::ThirdPartyCloud && !allow_cloud {
        return Err(CoreError::PermissionDenied(format!(
            "Sending data to the cloud provider \"{}\" is disabled in privacy settings",
            provider.display_name
        )));
    }
    Ok(())
}

/// Checks the permissions for the data actually included in a request.
/// Call at dispatch with current settings and the destination's own descriptor.
pub(crate) fn check_data_privacy(
    provider: &ProviderDescriptor,
    privacy: &PrivacySettings,
    shares_memory: bool,
    shares_captures: bool,
) -> Result<(), CoreError> {
    check_privacy(provider, privacy.allow_cloud_providers)?;
    if provider.locality != DataLocality::ThirdPartyCloud {
        return Ok(());
    }
    if shares_memory && !privacy.share_memory_with_cloud {
        return Err(CoreError::PermissionDenied(
            "Saved memory is not shared with cloud models unless allowed in Settings → Privacy"
                .to_owned(),
        ));
    }
    if shares_captures && !privacy.share_captures_with_cloud {
        return Err(CoreError::PermissionDenied(
            "Images are not sent to cloud models unless allowed in Settings → Privacy".to_owned(),
        ));
    }
    Ok(())
}

/// Resolves every category for display.
pub(crate) fn resolve_all(
    routing: &ModelRouting,
    catalog: &[ProviderModels],
    allow_cloud: bool,
) -> Vec<RouteResolution> {
    TaskCategory::ALL
        .into_iter()
        .map(
            |category| match select(category, routing, catalog, allow_cloud) {
                Ok(selection) => RouteResolution {
                    category,
                    provider_id: Some(selection.provider.id),
                    model: Some(selection.model.id),
                    automatic: selection.automatic,
                    error: None,
                },
                Err(err) => RouteResolution {
                    category,
                    provider_id: None,
                    model: None,
                    automatic: routing.get(category).is_none(),
                    error: Some(err.to_string()),
                },
            },
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ErrorCode;

    fn provider(id: &str, kind: ProviderKind, locality: DataLocality) -> ProviderDescriptor {
        ProviderDescriptor {
            id: ProviderId::new(id),
            display_name: id.to_owned(),
            kind,
            locality,
        }
    }

    fn model(provider_id: &str, id: &str, caps: ModelCapabilities) -> ModelInfo {
        ModelInfo {
            id: id.to_owned(),
            display_name: id.to_owned(),
            provider_id: ProviderId::new(provider_id),
            context_window: None,
            capabilities: caps,
        }
    }

    const CHAT: ModelCapabilities = ModelCapabilities {
        chat: true,
        streaming: true,
        embeddings: false,
        tool_calling: false,
        vision: false,
    };
    const VISION: ModelCapabilities = ModelCapabilities {
        vision: true,
        ..CHAT
    };
    const EMBED: ModelCapabilities = ModelCapabilities {
        chat: false,
        streaming: false,
        embeddings: true,
        tool_calling: false,
        vision: false,
    };

    fn catalog() -> Vec<ProviderModels> {
        vec![
            ProviderModels {
                provider: provider(
                    "diagnostic-echo",
                    ProviderKind::Diagnostic,
                    DataLocality::OnDevice,
                ),
                models: vec![model("diagnostic-echo", "echo", CHAT)],
                error: None,
            },
            ProviderModels {
                provider: provider("cloud", ProviderKind::Cloud, DataLocality::ThirdPartyCloud),
                models: vec![model("cloud", "big-vision", VISION)],
                error: None,
            },
            ProviderModels {
                provider: provider("lan", ProviderKind::LocalServer, DataLocality::LocalNetwork),
                models: vec![model("lan", "lan-chat", CHAT)],
                error: None,
            },
            ProviderModels {
                provider: provider("ollama", ProviderKind::LocalServer, DataLocality::OnDevice),
                models: vec![
                    model("ollama", "nomic-embed", EMBED),
                    model("ollama", "llama3.2", CHAT),
                    model("ollama", "llava", VISION),
                ],
                error: None,
            },
            ProviderModels {
                provider: provider("offline", ProviderKind::LocalServer, DataLocality::OnDevice),
                models: Vec::new(),
                error: Some("connection refused".into()),
            },
        ]
    }

    #[test]
    fn automatic_routing_prefers_on_device_real_models() {
        let catalog = catalog();
        let routing = ModelRouting::default();
        let general = select(TaskCategory::General, &routing, &catalog, false).expect("general");
        assert_eq!(general.model.id, "llama3.2");
        assert!(general.automatic);

        let vision = select(TaskCategory::Vision, &routing, &catalog, false).expect("vision");
        assert_eq!(vision.model.id, "llava");
        let embed = select(TaskCategory::Embeddings, &routing, &catalog, false).expect("embed");
        assert_eq!(embed.model.id, "nomic-embed");

        let err = select(TaskCategory::ToolCalling, &routing, &catalog, false).expect_err("none");
        assert_eq!(err.code(), ErrorCode::NotFound);
    }

    #[test]
    fn diagnostic_provider_is_the_last_resort() {
        let catalog = vec![catalog().remove(0)];
        let general = select(
            TaskCategory::General,
            &ModelRouting::default(),
            &catalog,
            false,
        )
        .expect("echo");
        assert_eq!(general.model.id, "echo");
    }

    #[test]
    fn cloud_models_need_permission() {
        let mut catalog = catalog();
        catalog.retain(|p| p.provider.id.as_str() == "cloud");
        let routing = ModelRouting::default();
        assert_eq!(
            select(TaskCategory::Vision, &routing, &catalog, false)
                .expect_err("blocked")
                .code(),
            ErrorCode::PermissionDenied
        );
        assert_eq!(
            select(TaskCategory::Vision, &routing, &catalog, true)
                .expect("allowed")
                .model
                .id,
            "big-vision"
        );

        let explicit = ModelRouting {
            vision: Some(ModelRef {
                provider_id: ProviderId::new("cloud"),
                model: "big-vision".into(),
            }),
            ..ModelRouting::default()
        };
        let err = select(TaskCategory::Vision, &explicit, &catalog, false).expect_err("blocked");
        assert_eq!(err.code(), ErrorCode::PermissionDenied);
    }

    #[test]
    fn explicit_choices_win_and_are_inherited() {
        let routing = ModelRouting {
            general: Some(ModelRef {
                provider_id: ProviderId::new("lan"),
                model: "lan-chat".into(),
            }),
            ..ModelRouting::default()
        };
        let catalog = catalog();
        let coding = select(TaskCategory::Coding, &routing, &catalog, false).expect("coding");
        assert_eq!(coding.model.id, "lan-chat");
        assert!(!coding.automatic);
        // Vision does not inherit from general.
        let vision = select(TaskCategory::Vision, &routing, &catalog, false).expect("vision");
        assert_eq!(vision.model.id, "llava");
    }

    #[test]
    fn explicit_choices_report_unavailable_models() {
        let catalog = catalog();
        let missing = ModelRouting {
            general: Some(ModelRef {
                provider_id: ProviderId::new("ollama"),
                model: "deleted-model".into(),
            }),
            ..ModelRouting::default()
        };
        assert_eq!(
            select(TaskCategory::General, &missing, &catalog, false)
                .expect_err("missing")
                .code(),
            ErrorCode::NotFound
        );

        let offline = ModelRouting {
            general: Some(ModelRef {
                provider_id: ProviderId::new("offline"),
                model: "x".into(),
            }),
            ..ModelRouting::default()
        };
        assert_eq!(
            select(TaskCategory::General, &offline, &catalog, false)
                .expect_err("offline")
                .code(),
            ErrorCode::Unavailable
        );

        let wrong_capability = ModelRouting {
            embeddings: Some(ModelRef {
                provider_id: ProviderId::new("ollama"),
                model: "llama3.2".into(),
            }),
            ..ModelRouting::default()
        };
        assert_eq!(
            select(TaskCategory::Embeddings, &wrong_capability, &catalog, false)
                .expect_err("not an embedding model")
                .code(),
            ErrorCode::Validation
        );
    }

    #[test]
    fn resolves_all_categories_for_display() {
        let resolutions = resolve_all(&ModelRouting::default(), &catalog(), false);
        assert_eq!(resolutions.len(), TaskCategory::ALL.len());
        let tools = resolutions
            .iter()
            .find(|r| r.category == TaskCategory::ToolCalling)
            .expect("tool calling");
        assert!(tools.error.is_some() && tools.model.is_none());
    }
}
