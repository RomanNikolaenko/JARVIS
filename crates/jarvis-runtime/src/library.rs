//! Library of GGUF models known to JARVIS (`models/library.json`).
//!
//! Imported files stay where they are and are never deleted by JARVIS.
//! Downloaded files live in the models directory and may be deleted on
//! request.

use std::path::{Path, PathBuf};
use std::sync::{PoisonError, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::io::AsyncWriteExt;

use crate::error::{Result, RuntimeError};
use crate::gguf;
use crate::types::{LocalModel, ModelSource, ProjectorFile};

const INDEX_FILE: &str = "library.json";
const INDEX_VERSION: u32 = 1;
const MAX_MODELS: usize = 256;

#[derive(Debug, Default, Serialize, Deserialize)]
struct Index {
    version: u32,
    models: Vec<LocalModel>,
}

/// Model ids: lowercase letters, digits, `-`, `_`, `.`; 1–96 characters.
pub fn is_valid_model_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 96
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.'))
}

fn slug(name: &str) -> String {
    let mut slug = String::new();
    for c in name.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_alphanumeric() || c == '.' || c == '_' {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');
    let slug: String = slug.chars().take(80).collect();
    if slug.is_empty() {
        "model".to_owned()
    } else {
        slug
    }
}

/// Canonical path without the Windows verbatim prefix (`\\?\`), which
/// some programs (and users) cannot handle.
pub(crate) async fn canonical(path: &Path) -> std::io::Result<PathBuf> {
    let canonical = tokio::fs::canonicalize(path).await?;
    Ok(simplify(canonical))
}

/// Whether `path` lies inside `dir`. Both sides are canonicalized, so links,
/// junctions and redirected folders (e.g. a relocated AppData) still match.
async fn is_inside(path: &Path, dir: &Path) -> bool {
    match (canonical(path).await, canonical(dir).await) {
        (Ok(path), Ok(dir)) => path.starts_with(dir),
        _ => false,
    }
}

pub(crate) fn simplify(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => path,
    }
}

pub(crate) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[derive(Debug)]
pub struct ModelLibrary {
    dir: PathBuf,
    models: RwLock<Vec<LocalModel>>,
    write_lock: tokio::sync::Mutex<()>,
}

impl ModelLibrary {
    /// Loads the index from `dir`, creating the directory if needed. A
    /// corrupt index is backed up and replaced by an empty one.
    pub async fn load(dir: PathBuf) -> Result<Self> {
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|err| RuntimeError::io("creating the models directory", err))?;
        let index_path = dir.join(INDEX_FILE);
        let mut models = match tokio::fs::read_to_string(&index_path).await {
            Ok(text) => match serde_json::from_str::<Index>(&text) {
                Ok(index) => index.models,
                Err(err) => {
                    let backup =
                        index_path.with_extension(format!("json.invalid-{}.bak", now_ms()));
                    tracing::warn!(
                        "model library index is invalid ({err}); backing up to {}",
                        backup.display()
                    );
                    tokio::fs::rename(&index_path, &backup)
                        .await
                        .map_err(|err| RuntimeError::io("backing up the model index", err))?;
                    Vec::new()
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(err) => return Err(RuntimeError::io("reading the model index", err)),
        };
        // `managed` allows JARVIS to delete a file, so it follows where the
        // file actually is instead of trusting the index.
        let models_dir = canonical(&dir).await.ok();
        for model in &mut models {
            if let Ok(path) = canonical(Path::new(&model.path)).await {
                model.managed = models_dir.as_ref().is_some_and(|d| path.starts_with(d));
            }
        }
        Ok(Self {
            dir,
            models: RwLock::new(models),
            write_lock: tokio::sync::Mutex::new(()),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// All models; `available` reflects the current state of the files.
    pub fn list(&self) -> Vec<LocalModel> {
        self.models
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .iter()
            .cloned()
            .map(|mut model| {
                model.available = std::fs::metadata(&model.path)
                    .is_ok_and(|meta| meta.is_file() && meta.len() == model.size_bytes);
                model
            })
            .collect()
    }

    pub fn get(&self, id: &str) -> Option<LocalModel> {
        self.list().into_iter().find(|m| m.id == id)
    }

    /// Path of a downloaded file inside the models directory.
    pub fn download_path(&self, file_name: &str) -> PathBuf {
        self.dir.join(file_name)
    }

    /// Imports a GGUF file in place (it is not copied).
    pub async fn import(&self, path: &Path) -> Result<LocalModel> {
        let path = canonical(path)
            .await
            .map_err(|err| RuntimeError::io(format!("opening {}", path.display()), err))?;
        let model = describe(path.clone(), ModelSource::Imported, None, &self.dir).await?;
        // Duplicate validation and the eventual commit share the same lock:
        // two imports of the same file cannot both pass the check.
        let _guard = self.write_lock.lock().await;
        for existing in self.list() {
            let existing = Path::new(&existing.path);
            if existing == path || canonical(existing).await.is_ok_and(|p| p == path) {
                return Err(RuntimeError::InvalidInput(format!(
                    "{} is already in the library",
                    path.display()
                )));
            }
        }
        self.add(model, self.snapshot()).await
    }

    /// Adds a verified download.
    pub async fn add_downloaded(
        &self,
        path: PathBuf,
        source: ModelSource,
        sha256: String,
    ) -> Result<LocalModel> {
        let path = canonical(&path)
            .await
            .map_err(|err| RuntimeError::io(format!("opening {}", path.display()), err))?;
        let mut model = describe(path, source, Some(sha256), &self.dir).await?;
        let _guard = self.write_lock.lock().await;
        let mut models = self.snapshot();
        // A completed transfer may race with an import. Only a retry of the
        // exact source and checksum can refresh an existing entry.
        for existing in &mut models {
            let path = Path::new(&existing.path);
            if path == Path::new(&model.path)
                || canonical(path)
                    .await
                    .is_ok_and(|path| path == Path::new(&model.path))
            {
                if existing.source != model.source || existing.sha256 != model.sha256 {
                    return Err(RuntimeError::InvalidInput(format!(
                        "{} is already registered for a different model; choose a different download filename",
                        model.path
                    )));
                }
                model.id = existing.id.clone();
                model.projector = existing.projector.clone();
                model.added_at_ms = existing.added_at_ms;
                *existing = model.clone();
                self.commit(models).await?;
                return Ok(model);
            }
        }
        self.add(model, models).await
    }

    /// Attaches (or detaches) a vision projector to a model.
    pub async fn set_projector(
        &self,
        id: &str,
        projector: Option<ProjectorFile>,
    ) -> Result<LocalModel> {
        let _guard = self.write_lock.lock().await;
        let mut models = self.snapshot();
        let model = models.iter_mut().find(|m| m.id == id).ok_or_else(|| {
            RuntimeError::NotFound(format!("model \"{id}\" is not in the library"))
        })?;
        model.projector = projector;
        self.commit(models).await?;
        self.get(id)
            .ok_or_else(|| RuntimeError::NotFound(format!("model \"{id}\" is not in the library")))
    }

    /// Removes a model from the library. The file is deleted only when
    /// `delete_file` is set and JARVIS manages it.
    pub async fn remove(&self, id: &str, delete_file: bool) -> Result<()> {
        let _guard = self.write_lock.lock().await;
        let model = self.get(id).ok_or_else(|| {
            RuntimeError::NotFound(format!("model \"{id}\" is not in the library"))
        })?;
        if delete_file && !model.managed {
            return Err(RuntimeError::PermissionDenied(
                "JARVIS only deletes files it downloaded; remove the imported file yourself".into(),
            ));
        }
        let previous = self.snapshot();
        let mut models = previous.clone();
        models.retain(|m| m.id != id);
        // Commit the index before destructive cleanup. A failed index write
        // must never delete the user's weights or projector.
        self.persist(&models).await?;
        if delete_file {
            match tokio::fs::remove_file(&model.path).await {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => {
                    // Keep a failed deletion retryable. If restoring the old
                    // index also fails, publish the index that did commit and
                    // report the remaining file explicitly.
                    if let Err(rollback) = self.persist(&previous).await {
                        self.publish(models);
                        return Err(RuntimeError::Internal(format!(
                            "The model was removed from the library, but its file could not be deleted: {} ({err}). Restoring the library entry also failed: {rollback}. Remove the remaining file manually.",
                            model.path
                        )));
                    }
                    return Err(RuntimeError::io("deleting the model file", err));
                }
            }
            // A downloaded projector goes with its model unless another
            // model uses it too.
            if let Some(projector) = &model.projector
                && is_inside(Path::new(&projector.path), &self.dir).await
                && !models.iter().any(|m| {
                    m.projector
                        .as_ref()
                        .is_some_and(|p| p.path == projector.path)
                })
            {
                let _ = tokio::fs::remove_file(&projector.path).await;
            }
        }
        self.publish(models);
        Ok(())
    }

    /// Caller holds `write_lock` while building and committing this candidate.
    async fn add(&self, mut model: LocalModel, mut models: Vec<LocalModel>) -> Result<LocalModel> {
        if models.len() >= MAX_MODELS {
            return Err(RuntimeError::InvalidInput(format!(
                "the library is limited to {MAX_MODELS} models"
            )));
        }
        let base = model.id.clone();
        let mut n = 2;
        while models.iter().any(|m| m.id == model.id) {
            model.id = format!("{base}-{n}");
            n += 1;
        }
        models.push(model.clone());
        self.commit(models).await?;
        model.available = true;
        Ok(model)
    }

    fn snapshot(&self) -> Vec<LocalModel> {
        self.models
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn publish(&self, models: Vec<LocalModel>) {
        *self.models.write().unwrap_or_else(PoisonError::into_inner) = models;
    }

    async fn commit(&self, models: Vec<LocalModel>) -> Result<()> {
        self.persist(&models).await?;
        self.publish(models);
        Ok(())
    }

    async fn persist(&self, models: &[LocalModel]) -> Result<()> {
        let index = Index {
            version: INDEX_VERSION,
            models: models.to_vec(),
        };
        let json = serde_json::to_vec_pretty(&index)
            .map_err(|err| RuntimeError::Internal(err.to_string()))?;
        write_atomic(&self.dir.join(INDEX_FILE), &json).await
    }
}

async fn describe(
    path: PathBuf,
    source: ModelSource,
    sha256: Option<String>,
    models_dir: &Path,
) -> Result<LocalModel> {
    let metadata = tokio::fs::metadata(&path)
        .await
        .map_err(|err| RuntimeError::io(format!("reading {}", path.display()), err))?;
    if !metadata.is_file() {
        return Err(RuntimeError::InvalidInput(format!(
            "{} is not a file",
            path.display()
        )));
    }
    let header_path = path.clone();
    let gguf = tokio::task::spawn_blocking(move || gguf::read_gguf_info(&header_path))
        .await
        .map_err(|err| RuntimeError::Internal(err.to_string()))??;

    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "model".to_owned());
    let managed = is_inside(&path, models_dir).await;
    Ok(LocalModel {
        id: slug(&stem),
        display_name: gguf.name.clone().unwrap_or_else(|| stem.clone()),
        path: path.display().to_string(),
        size_bytes: metadata.len(),
        sha256,
        source,
        gguf,
        added_at_ms: now_ms(),
        managed,
        available: true,
        projector: None,
    })
}

pub(crate) async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut tmp_name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
    let context = format!("writing {}", path.display());
    {
        let mut file = tokio::fs::File::create(&tmp)
            .await
            .map_err(|err| RuntimeError::io(&context, err))?;
        file.write_all(bytes)
            .await
            .map_err(|err| RuntimeError::io(&context, err))?;
        file.sync_all()
            .await
            .map_err(|err| RuntimeError::io(&context, err))?;
    }
    tokio::fs::rename(&tmp, path)
        .await
        .map_err(|err| RuntimeError::io(&context, err))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gguf::tests::sample_gguf;

    #[test]
    fn slugs_are_valid_ids() {
        assert_eq!(
            slug("qwen2.5-0.5b-instruct-q4_k_m"),
            "qwen2.5-0.5b-instruct-q4_k_m"
        );
        assert_eq!(slug("Llama 3.2 1B (Q4)"), "llama-3.2-1b-q4");
        assert_eq!(slug("Модель"), "model");
        assert!(is_valid_model_id(&slug("Some Model!!")));
    }

    #[tokio::test]
    async fn imports_lists_and_removes_models() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("tempdir");
        let file = outside.path().join("Qwen Test Q4.gguf");
        std::fs::write(&file, sample_gguf(&[])).expect("write");

        let library = ModelLibrary::load(dir.path().join("models"))
            .await
            .expect("load");
        let model = library.import(&file).await.expect("import");
        assert_eq!(model.id, "qwen-test-q4");
        assert_eq!(model.display_name, "Qwen2.5 0.5B Instruct");
        assert!(!model.managed, "imported files are not managed");
        assert!(library.import(&file).await.is_err(), "duplicate import");

        // The index survives a reload.
        let reloaded = ModelLibrary::load(dir.path().join("models"))
            .await
            .expect("reload");
        assert_eq!(reloaded.list().len(), 1);
        assert!(reloaded.list()[0].available);

        // Imported files cannot be deleted by JARVIS.
        assert!(matches!(
            reloaded.remove(&model.id, true).await,
            Err(RuntimeError::PermissionDenied(_))
        ));
        reloaded.remove(&model.id, false).await.expect("remove");
        assert!(file.exists(), "the user's file is untouched");
        assert!(reloaded.list().is_empty());
    }

    #[tokio::test]
    async fn rejects_non_gguf_files_and_tracks_missing_files() {
        let dir = tempfile::tempdir().expect("tempdir");
        let library = ModelLibrary::load(dir.path().to_path_buf())
            .await
            .expect("load");
        let fake = dir.path().join("notes.gguf");
        std::fs::write(&fake, b"definitely not a model").expect("write");
        assert!(matches!(
            library.import(&fake).await,
            Err(RuntimeError::InvalidModel(_))
        ));

        let managed = library.download_path("m.gguf");
        std::fs::write(&managed, sample_gguf(&[])).expect("write");
        let model = library
            .add_downloaded(
                managed.clone(),
                ModelSource::Downloaded {
                    repo: "a/b".into(),
                    file: "m.gguf".into(),
                    revision: "abc".into(),
                },
                "00".repeat(32),
            )
            .await
            .expect("add");
        assert!(model.managed);
        std::fs::remove_file(&managed).expect("delete");
        assert!(!library.get(&model.id).expect("still listed").available);
        library
            .remove(&model.id, true)
            .await
            .expect("remove missing file");
    }

    fn hf_source(file: &str) -> ModelSource {
        ModelSource::Downloaded {
            repo: "a/b".into(),
            file: file.into(),
            revision: "abc".into(),
        }
    }

    /// A directory cannot be opened as the temporary index file on either OS.
    fn block_index_write(library: &ModelLibrary) -> PathBuf {
        let path = library.dir().join(format!("{INDEX_FILE}.tmp"));
        std::fs::create_dir(&path).expect("block index writes");
        path
    }

    async fn assert_unchanged(library: &ModelLibrary, expected: &[LocalModel]) {
        assert_eq!(library.list(), expected, "live library remains committed");
        let reloaded = ModelLibrary::load(library.dir().to_path_buf())
            .await
            .expect("reopen committed index");
        assert_eq!(reloaded.list(), expected, "disk and live library agree");
    }

    #[tokio::test]
    async fn failed_import_does_not_publish_a_model_and_can_be_retried() {
        let dir = tempfile::tempdir().expect("tempdir");
        let library = ModelLibrary::load(dir.path().join("models"))
            .await
            .expect("library");
        let first = dir.path().join("first.gguf");
        let second = dir.path().join("second.gguf");
        std::fs::write(&first, sample_gguf(&[])).expect("first model");
        std::fs::write(&second, sample_gguf(&[])).expect("second model");
        library.import(&first).await.expect("seed library");
        let before = library.list();
        let blocker = block_index_write(&library);

        assert!(library.import(&second).await.is_err());
        assert_unchanged(&library, &before).await;
        assert!(
            second.exists(),
            "failed import leaves the user's file intact"
        );

        std::fs::remove_dir(blocker).expect("restore index writes");
        library.import(&second).await.expect("retry import");
        assert_eq!(library.list().len(), 2);
        assert_unchanged(&library, &library.list()).await;
    }

    #[tokio::test]
    async fn failed_download_retry_keeps_the_original_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let library = ModelLibrary::load(dir.path().join("models"))
            .await
            .expect("library");
        let file = library.download_path("model.gguf");
        std::fs::write(&file, sample_gguf(&[])).expect("model");
        let model = library
            .add_downloaded(file.clone(), hf_source("model.gguf"), "00".repeat(32))
            .await
            .expect("seed download");
        library
            .set_projector(
                &model.id,
                Some(ProjectorFile {
                    path: "original-projector.gguf".into(),
                    size_bytes: 123,
                    sha256: Some("22".repeat(32)),
                }),
            )
            .await
            .expect("seed projector");
        let before = library.list();
        let blocker = block_index_write(&library);

        assert!(
            library
                .add_downloaded(file.clone(), hf_source("model.gguf"), "00".repeat(32))
                .await
                .is_err()
        );
        assert_unchanged(&library, &before).await;

        std::fs::remove_dir(blocker).expect("restore index writes");
        let replaced = library
            .add_downloaded(file, hf_source("model.gguf"), "00".repeat(32))
            .await
            .expect("retry download registration");
        assert_eq!(replaced, before[0]);
        assert_eq!(library.list().len(), 1);
        assert_unchanged(&library, &library.list()).await;
    }

    #[tokio::test]
    async fn failed_new_download_registration_does_not_publish_an_entry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let library = ModelLibrary::load(dir.path().join("models"))
            .await
            .expect("library");
        let file = library.download_path("model.gguf");
        std::fs::write(&file, sample_gguf(&[])).expect("model");
        let blocker = block_index_write(&library);

        assert!(
            library
                .add_downloaded(file.clone(), hf_source("model.gguf"), "00".repeat(32))
                .await
                .is_err()
        );
        assert_unchanged(&library, &[]).await;
        assert!(file.exists(), "verified weights remain available for retry");

        std::fs::remove_dir(blocker).expect("restore index writes");
        library
            .add_downloaded(file, hf_source("model.gguf"), "00".repeat(32))
            .await
            .expect("retry registration");
        assert_eq!(library.list().len(), 1);
        assert_unchanged(&library, &library.list()).await;
    }

    #[tokio::test]
    async fn a_download_cannot_replace_an_import_or_another_source_or_checksum() {
        for conflict in ["import", "source", "checksum"] {
            let dir = tempfile::tempdir().expect("tempdir");
            let library = ModelLibrary::load(dir.path().join("models"))
                .await
                .expect("library");
            let file = library.download_path("model.gguf");
            std::fs::write(&file, sample_gguf(&[])).expect("model");
            if conflict == "import" {
                library.import(&file).await.expect("import model");
            } else {
                library
                    .add_downloaded(file.clone(), hf_source("model.gguf"), "00".repeat(32))
                    .await
                    .expect("seed download");
            }
            let before = library.list();
            let source = hf_source(if conflict == "source" {
                "different/model.gguf"
            } else {
                "model.gguf"
            });
            let sha = if conflict == "checksum" { "11" } else { "00" }.repeat(32);
            assert!(
                library.add_downloaded(file, source, sha).await.is_err(),
                "{conflict}"
            );
            assert_unchanged(&library, &before).await;
        }
    }

    #[tokio::test]
    async fn an_exact_download_retry_preserves_the_original_id_and_projector() {
        let dir = tempfile::tempdir().expect("tempdir");
        let other = dir.path().join("model.gguf");
        std::fs::write(&other, sample_gguf(&[])).expect("imported model");
        let library = ModelLibrary::load(dir.path().join("models"))
            .await
            .expect("library");
        let imported = library.import(&other).await.expect("import model");
        let file = library.download_path("model.gguf");
        std::fs::write(&file, sample_gguf(&[])).expect("downloaded model");
        let downloaded = library
            .add_downloaded(file.clone(), hf_source("model.gguf"), "00".repeat(32))
            .await
            .expect("seed download");
        assert_ne!(downloaded.id, imported.id);
        library
            .set_projector(
                &downloaded.id,
                Some(ProjectorFile {
                    path: "vision.gguf".into(),
                    size_bytes: 123,
                    sha256: None,
                }),
            )
            .await
            .expect("seed projector");
        library
            .remove(&imported.id, false)
            .await
            .expect("remove occupied base ID");
        let before = library.list();

        let retried = library
            .add_downloaded(file, hf_source("model.gguf"), "00".repeat(32))
            .await
            .expect("retry download registration");
        assert_eq!(retried, before[0], "retry must not change routes or vision");
        assert_unchanged(&library, &before).await;
    }

    #[tokio::test]
    async fn failed_projector_changes_keep_the_previous_configuration() {
        let dir = tempfile::tempdir().expect("tempdir");
        let library = ModelLibrary::load(dir.path().join("models"))
            .await
            .expect("library");
        let file = dir.path().join("model.gguf");
        std::fs::write(&file, sample_gguf(&[])).expect("model");
        let model = library.import(&file).await.expect("seed model");
        for projector in [
            Some(ProjectorFile {
                path: "vision.gguf".into(),
                size_bytes: 123,
                sha256: None,
            }),
            None,
        ] {
            let before = library.list();
            let blocker = block_index_write(&library);
            assert!(
                library
                    .set_projector(&model.id, projector.clone())
                    .await
                    .is_err()
            );
            assert_unchanged(&library, &before).await;

            std::fs::remove_dir(blocker).expect("restore index writes");
            let changed = library
                .set_projector(&model.id, projector.clone())
                .await
                .expect("retry projector change");
            assert_eq!(changed.projector, projector);
            assert_unchanged(&library, &library.list()).await;
        }
    }

    #[tokio::test]
    async fn failed_remove_keeps_the_index_and_model_files_for_retry() {
        for delete_file in [false, true] {
            let dir = tempfile::tempdir().expect("tempdir");
            let library = ModelLibrary::load(dir.path().join("models"))
                .await
                .expect("library");
            let file = library.download_path("model.gguf");
            let projector = library.download_path("vision.gguf");
            std::fs::write(&file, sample_gguf(&[])).expect("model");
            std::fs::write(&projector, b"projector").expect("projector");
            let model = library
                .add_downloaded(file.clone(), hf_source("model.gguf"), "00".repeat(32))
                .await
                .expect("seed model");
            library
                .set_projector(
                    &model.id,
                    Some(ProjectorFile {
                        path: projector.display().to_string(),
                        size_bytes: 9,
                        sha256: None,
                    }),
                )
                .await
                .expect("seed projector");
            let before = library.list();
            let blocker = block_index_write(&library);

            assert!(library.remove(&model.id, delete_file).await.is_err());
            assert_unchanged(&library, &before).await;
            assert!(file.exists(), "index failure must not delete weights");
            assert!(
                projector.exists(),
                "index failure must not delete vision data"
            );

            std::fs::remove_dir(blocker).expect("restore index writes");
            library
                .remove(&model.id, delete_file)
                .await
                .expect("retry removal");
            assert!(library.list().is_empty());
            assert_eq!(file.exists(), !delete_file);
            assert_eq!(projector.exists(), !delete_file);
            assert_unchanged(&library, &[]).await;
        }
    }

    #[tokio::test]
    async fn a_failed_file_deletion_restores_the_entry_for_retry() {
        let dir = tempfile::tempdir().expect("tempdir");
        let library = ModelLibrary::load(dir.path().join("models"))
            .await
            .expect("library");
        let file = library.download_path("model.gguf");
        let bytes = sample_gguf(&[]);
        std::fs::write(&file, &bytes).expect("model");
        let model = library
            .add_downloaded(file.clone(), hf_source("model.gguf"), "00".repeat(32))
            .await
            .expect("seed model");
        // A directory cannot be deleted with remove_file, even in an elevated
        // test process. This deterministically simulates failed file cleanup.
        std::fs::remove_file(&file).expect("remove fixture file");
        std::fs::create_dir(&file).expect("block file deletion");
        let before = library.list();
        assert!(!before[0].available);

        assert!(library.remove(&model.id, true).await.is_err());
        assert_unchanged(&library, &before).await;

        std::fs::remove_dir(&file).expect("unblock deletion");
        std::fs::write(&file, bytes).expect("restore model fixture");
        library
            .remove(&model.id, true)
            .await
            .expect("retry file deletion");
        assert!(!file.exists());
        assert_unchanged(&library, &[]).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_imports_are_serialized_without_duplicates_or_lost_entries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let library = ModelLibrary::load(dir.path().join("models"))
            .await
            .expect("library");
        let first = dir.path().join("first.gguf");
        let second = dir.path().join("second.gguf");
        std::fs::write(&first, sample_gguf(&[])).expect("first model");
        std::fs::write(&second, sample_gguf(&[])).expect("second model");

        let (a, b, duplicate) = tokio::join!(
            library.import(&first),
            library.import(&second),
            library.import(&first)
        );
        assert!(b.is_ok());
        assert_eq!(usize::from(a.is_ok()) + usize::from(duplicate.is_ok()), 1);
        assert_eq!(library.list().len(), 2);
        assert_unchanged(&library, &library.list()).await;
    }

    #[tokio::test]
    async fn managed_follows_the_file_location_not_the_index() {
        let dir = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("tempdir");
        let models_dir = dir.path().join("models");
        let library = ModelLibrary::load(models_dir.clone()).await.expect("load");

        let inside = library.download_path("in.gguf");
        std::fs::write(&inside, sample_gguf(&[])).expect("write");
        let downloaded = library
            .add_downloaded(inside, hf_source("in.gguf"), "00".repeat(32))
            .await
            .expect("add");
        let user_file = outside.path().join("user.gguf");
        std::fs::write(&user_file, sample_gguf(&[])).expect("write");
        let imported = library.import(&user_file).await.expect("import");

        // Flip every flag in the index: it must not decide what JARVIS deletes.
        let index_path = models_dir.join(INDEX_FILE);
        let mut index: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&index_path).expect("read")).expect("json");
        for model in index["models"].as_array_mut().expect("models") {
            let managed = model["managed"].as_bool().expect("flag");
            model["managed"] = serde_json::Value::Bool(!managed);
        }
        std::fs::write(&index_path, serde_json::to_vec(&index).expect("json")).expect("write");

        let reloaded = ModelLibrary::load(models_dir).await.expect("reload");
        assert!(reloaded.get(&downloaded.id).expect("downloaded").managed);
        assert!(!reloaded.get(&imported.id).expect("imported").managed);
        assert!(matches!(
            reloaded.remove(&imported.id, true).await,
            Err(RuntimeError::PermissionDenied(_))
        ));
        assert!(user_file.exists());
    }

    /// The data folder may be reached through a link: a symlink, a junction
    /// or a redirected AppData folder.
    #[tokio::test]
    async fn files_reached_through_a_link_are_managed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let real = dir.path().join("real");
        let link = dir.path().join("link");
        std::fs::create_dir(&real).expect("mkdir");
        link_dir(&real, &link);

        let library = ModelLibrary::load(link.join("models")).await.expect("load");
        let file = library.download_path("m.gguf");
        std::fs::write(&file, sample_gguf(&[])).expect("write");
        let model = library
            .add_downloaded(file, hf_source("m.gguf"), "00".repeat(32))
            .await
            .expect("add");
        assert!(model.managed);

        let real_file = real.join("models").join("m.gguf");
        assert!(
            library.import(&real_file).await.is_err(),
            "the same file through its real path is a duplicate"
        );
        library.remove(&model.id, true).await.expect("delete");
        assert!(!real_file.exists());
    }

    #[cfg(unix)]
    fn link_dir(target: &Path, link: &Path) {
        std::os::unix::fs::symlink(target, link).expect("symlink");
    }

    /// A junction, which unlike a symlink needs no special privileges.
    #[cfg(windows)]
    fn link_dir(target: &Path, link: &Path) {
        let status = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(link)
            .arg(target)
            .stdout(std::process::Stdio::null())
            .status()
            .expect("mklink");
        assert!(status.success(), "mklink /J failed");
    }

    #[tokio::test]
    async fn corrupt_index_is_backed_up() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(INDEX_FILE), b"{ not json").expect("write");
        let library = ModelLibrary::load(dir.path().to_path_buf())
            .await
            .expect("load");
        assert!(library.list().is_empty());
        let backups = std::fs::read_dir(dir.path())
            .expect("list")
            .filter_map(std::result::Result::ok)
            .filter(|e| e.file_name().to_string_lossy().ends_with(".bak"))
            .count();
        assert_eq!(backups, 1);
    }
}
