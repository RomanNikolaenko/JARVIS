//! Verified model sources: a curated catalog with pinned revisions and
//! checksums, and resolution of arbitrary Hugging Face files to an immutable
//! commit plus the SHA-256 that Hugging Face stores for the LFS object.

use serde::Deserialize;
use url::Url;

use crate::download::Expected;
use crate::error::{Result, RuntimeError};
use crate::types::{CuratedModel, ModelDownloadRequest, ServerRole};

const HF: &str = "https://huggingface.co";

/// Small instruction-tuned models that run on ordinary laptops, and an
/// embedding model for semantic memory search. Revisions are commit hashes,
/// so the files are immutable and the SHA-256 values hold.
pub fn curated_models() -> Vec<CuratedModel> {
    let chat = |id: &str,
                name: &str,
                description: &str,
                repo: &str,
                file: &str,
                revision: &str,
                sha256: &str,
                size_bytes: u64,
                license: &str| CuratedModel {
        id: id.to_owned(),
        display_name: name.to_owned(),
        description: description.to_owned(),
        repo: repo.to_owned(),
        file: file.to_owned(),
        revision: revision.to_owned(),
        sha256: sha256.to_owned(),
        size_bytes,
        license: license.to_owned(),
        role: ServerRole::Chat,
    };
    vec![
        chat(
            "qwen2.5-0.5b-instruct-q4_k_m",
            "Qwen2.5 0.5B Instruct (Q4_K_M)",
            "Tiny and fast; good for testing and simple tasks. ~1 GB RAM.",
            "Qwen/Qwen2.5-0.5B-Instruct-GGUF",
            "qwen2.5-0.5b-instruct-q4_k_m.gguf",
            "9217f5db79a29953eb74d5343926648285ec7e67",
            "74a4da8c9fdbcd15bd1f6d01d621410d31c6fc00986f5eb687824e7b93d7a9db",
            491_400_032,
            "Apache-2.0",
        ),
        chat(
            "llama-3.2-1b-instruct-q4_k_m",
            "Llama 3.2 1B Instruct (Q4_K_M)",
            "Small multilingual assistant model. ~1.5 GB RAM.",
            "bartowski/Llama-3.2-1B-Instruct-GGUF",
            "Llama-3.2-1B-Instruct-Q4_K_M.gguf",
            "067b946cf014b7c697f3654f621d577a3e3afd1c",
            "6f85a640a97cf2bf5b8e764087b1e83da0fdb51d7c9fab7d0fece9385611df83",
            807_694_464,
            "Llama 3.2 Community License",
        ),
        chat(
            "qwen2.5-1.5b-instruct-q4_k_m",
            "Qwen2.5 1.5B Instruct (Q4_K_M)",
            "Balanced quality and speed; good multilingual support. ~2 GB RAM.",
            "Qwen/Qwen2.5-1.5B-Instruct-GGUF",
            "qwen2.5-1.5b-instruct-q4_k_m.gguf",
            "91cad51170dc346986eccefdc2dd33a9da36ead9",
            "6a1a2eb6d15622bf3c96857206351ba97e1af16c30d7a74ee38970e434e9407e",
            1_117_320_736,
            "Apache-2.0",
        ),
        chat(
            "qwen2.5-3b-instruct-q4_k_m",
            "Qwen2.5 3B Instruct (Q4_K_M)",
            "Noticeably better answers; needs a stronger machine. ~3.5 GB RAM.",
            "Qwen/Qwen2.5-3B-Instruct-GGUF",
            "qwen2.5-3b-instruct-q4_k_m.gguf",
            "7dabda4d13d513e3e842b20f0d435c732f172cbe",
            "626b4a6678b86442240e33df819e00132d3ba7dddfe1cdc4fbb18e0a9615c62d",
            2_104_932_768,
            "Qwen Research License",
        ),
        // Published by the llama.cpp project; BAAI/bge-m3 converted to GGUF
        // (architecture `bert`, CLS pooling, 8 192-token training context).
        CuratedModel {
            role: ServerRole::Embeddings,
            ..chat(
                "bge-m3-q8_0",
                "BGE-M3 embeddings (Q8_0)",
                "Multilingual embedding model (100+ languages, incl. Ukrainian) for semantic memory search; runs next to a chat model. ~0.9 GB RAM.",
                "ggml-org/bge-m3-Q8_0-GGUF",
                "bge-m3-q8_0.gguf",
                "9eba04c5d75ba5a1595e45de734d36bef4e5cb98",
                "aa473d51f451a22f0fcf39ba3330c14bed38a385712b1113440f69df4047a173",
                634_553_760,
                "MIT",
            )
        },
    ]
}

/// A Hugging Face file resolved to an immutable commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFile {
    pub repo: String,
    pub file: String,
    pub revision: String,
    pub url: Url,
    pub expected: Expected,
}

impl ResolvedFile {
    pub fn from_curated(model: &CuratedModel) -> Result<Self> {
        Ok(Self {
            url: resolve_url(&model.repo, &model.revision, &model.file)?,
            repo: model.repo.clone(),
            file: model.file.clone(),
            revision: model.revision.clone(),
            expected: Expected {
                sha256: model.sha256.clone(),
                size: Some(model.size_bytes),
            },
        })
    }

    /// Local file name: the last path segment of the repository file.
    pub fn file_name(&self) -> &str {
        self.file.rsplit('/').next().unwrap_or(&self.file)
    }
}

/// Validates a user request before any network access.
pub fn validate_request(request: &ModelDownloadRequest) -> Result<()> {
    let invalid = |message: &str| Err(RuntimeError::InvalidInput(message.to_owned()));
    let segment_ok = |s: &str| {
        !s.is_empty()
            && s.len() <= 96
            && s != "."
            && s != ".."
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
    };
    let mut parts = request.repo.split('/');
    let (Some(owner), Some(name), None) = (parts.next(), parts.next(), parts.next()) else {
        return invalid("repository must look like \"owner/name\"");
    };
    if !segment_ok(owner) || !segment_ok(name) {
        return invalid("repository name contains invalid characters");
    }
    let file = request.file.trim();
    if file.len() > 255
        || !file.to_ascii_lowercase().ends_with(".gguf")
        || file.split('/').any(|segment| !segment_ok(segment))
    {
        return invalid("file must be a .gguf path inside the repository");
    }
    if let Some(revision) = &request.revision
        && (revision.is_empty()
            || revision.len() > 100
            || revision.split('/').any(|segment| !segment_ok(segment)))
    {
        return invalid("revision contains invalid characters");
    }
    Ok(())
}

fn resolve_url(repo: &str, revision: &str, file: &str) -> Result<Url> {
    let mut url = Url::parse(HF).map_err(|err| RuntimeError::Internal(err.to_string()))?;
    {
        let mut segments = url
            .path_segments_mut()
            .map_err(|()| RuntimeError::Internal("invalid base URL".into()))?;
        segments.extend(repo.split('/'));
        segments.push("resolve");
        segments.push(revision);
        segments.extend(file.split('/'));
    }
    Ok(url)
}

#[derive(Debug, Deserialize)]
struct RepoInfo {
    sha: String,
    #[serde(default)]
    gated: serde_json::Value,
    #[serde(default)]
    private: bool,
}

#[derive(Debug, Deserialize)]
struct PathInfo {
    #[serde(default)]
    lfs: Option<LfsInfo>,
}

#[derive(Debug, Deserialize)]
struct LfsInfo {
    oid: String,
    size: u64,
}

/// Resolves a user request through the Hugging Face API: the revision to a
/// commit hash and the file to its LFS SHA-256 and size.
pub async fn resolve(
    client: &reqwest::Client,
    request: &ModelDownloadRequest,
) -> Result<ResolvedFile> {
    validate_request(request)?;
    let revision = request.revision.as_deref().unwrap_or("main");
    let file = request.file.trim();

    let info_url = format!("{HF}/api/models/{}/revision/{revision}", request.repo);
    let response =
        client.get(&info_url).send().await.map_err(|err| {
            RuntimeError::Unavailable(format!("cannot reach Hugging Face: {err}"))
        })?;
    if response.status() == reqwest::StatusCode::NOT_FOUND
        || response.status() == reqwest::StatusCode::UNAUTHORIZED
    {
        return Err(RuntimeError::NotFound(format!(
            "repository {} (revision {revision}) was not found or is private",
            request.repo
        )));
    }
    let info: RepoInfo = response
        .error_for_status()
        .map_err(|err| RuntimeError::Unavailable(err.to_string()))?
        .json()
        .await
        .map_err(|err| {
            RuntimeError::Unavailable(format!("unexpected Hugging Face response: {err}"))
        })?;
    if info.private
        || !matches!(
            info.gated,
            serde_json::Value::Bool(false) | serde_json::Value::Null
        )
    {
        return Err(RuntimeError::PermissionDenied(
            "gated or private repositories need a Hugging Face login, which JARVIS does not support yet".into(),
        ));
    }
    if info.sha.len() != 40 || !info.sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(RuntimeError::Unavailable(
            "Hugging Face returned an invalid commit".into(),
        ));
    }

    let paths_url = format!("{HF}/api/models/{}/paths-info/{}", request.repo, info.sha);
    let paths: Vec<PathInfo> = client
        .post(&paths_url)
        .json(&serde_json::json!({ "paths": [file] }))
        .send()
        .await
        .map_err(|err| RuntimeError::Unavailable(format!("cannot reach Hugging Face: {err}")))?
        .error_for_status()
        .map_err(|err| RuntimeError::Unavailable(err.to_string()))?
        .json()
        .await
        .map_err(|err| {
            RuntimeError::Unavailable(format!("unexpected Hugging Face response: {err}"))
        })?;
    let lfs = paths
        .into_iter()
        .next()
        .ok_or_else(|| RuntimeError::NotFound(format!("{file} was not found in {}", request.repo)))?
        .lfs
        .ok_or_else(|| {
            RuntimeError::InvalidInput(format!("{file} is not a model file (no LFS object)"))
        })?;
    if lfs.oid.len() != 64 || !lfs.oid.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err(RuntimeError::Unavailable(
            "Hugging Face returned an invalid checksum".into(),
        ));
    }

    Ok(ResolvedFile {
        url: resolve_url(&request.repo, &info.sha, file)?,
        repo: request.repo.clone(),
        file: file.to_owned(),
        revision: info.sha,
        expected: Expected {
            sha256: lfs.oid.to_ascii_lowercase(),
            size: Some(lfs.size),
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::download::check_source;

    fn request(repo: &str, file: &str, revision: Option<&str>) -> ModelDownloadRequest {
        ModelDownloadRequest {
            repo: repo.into(),
            file: file.into(),
            revision: revision.map(str::to_owned),
        }
    }

    #[test]
    fn curated_models_are_pinned_and_downloadable() {
        for model in curated_models() {
            assert_eq!(model.revision.len(), 40, "{}", model.id);
            assert_eq!(model.sha256.len(), 64, "{}", model.id);
            assert!(crate::library::is_valid_model_id(&model.id));
            let resolved = ResolvedFile::from_curated(&model).expect("resolve");
            check_source(&resolved.url).expect("allowed source");
            assert!(resolved.url.as_str().contains(&model.revision));
        }
        let embedding: Vec<_> = curated_models()
            .into_iter()
            .filter(|m| m.role == ServerRole::Embeddings)
            .collect();
        assert_eq!(embedding.len(), 1, "one embedding model is offered");
        // The library derives ids from file names; the UI matches them.
        assert_eq!(embedding[0].id, "bge-m3-q8_0");
    }

    #[test]
    fn builds_resolve_urls_with_encoded_segments() {
        let url = resolve_url("owner/repo", "main", "sub dir/model q4.gguf").expect("url");
        assert_eq!(
            url.as_str(),
            "https://huggingface.co/owner/repo/resolve/main/sub%20dir/model%20q4.gguf"
        );
    }

    #[test]
    fn validates_requests() {
        assert!(
            validate_request(&request("Qwen/Qwen2.5-0.5B-Instruct-GGUF", "q.gguf", None)).is_ok()
        );
        assert!(validate_request(&request("a/b", "dir/q.gguf", Some("main"))).is_ok());
        for bad in [
            request("no-slash", "q.gguf", None),
            request("a/b/c", "q.gguf", None),
            request("a/b", "../q.gguf", None),
            request("a/b", "q.bin", None),
            request("a/b", "q.gguf", Some("../x")),
            request("a/b?x=1", "q.gguf", None),
        ] {
            assert!(validate_request(&bad).is_err(), "{bad:?}");
        }
    }
}
