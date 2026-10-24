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
        for existing in self.list() {
            let existing = Path::new(&existing.path);
            if existing == path || canonical(existing).await.is_ok_and(|p| p == path) {
                return Err(RuntimeError::InvalidInput(format!(
                    "{} is already in the library",
                    path.display()
                )));
            }
        }
        let model = describe(path, ModelSource::Imported, None, &self.dir).await?;
        self.add(model).await
    }

    /// Adds a verified download.
    pub async fn add_downloaded(
        &self,
        path: PathBuf,
        source: ModelSource,
        sha256: String,
    ) -> Result<LocalModel> {
        let model = describe(path, source, Some(sha256), &self.dir).await?;
        // Re-downloading the same file replaces the entry.
        self.models
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|m| m.path != model.path);
        self.add(model).await
    }

    /// Attaches (or detaches) a vision projector to a model.
    pub async fn set_projector(
        &self,
        id: &str,
        projector: Option<ProjectorFile>,
    ) -> Result<LocalModel> {
        {
            let mut models = self.models.write().unwrap_or_else(PoisonError::into_inner);
            let model = models.iter_mut().find(|m| m.id == id).ok_or_else(|| {
                RuntimeError::NotFound(format!("model \"{id}\" is not in the library"))
            })?;
            model.projector = projector;
        }
        self.save().await?;
        self.get(id)
            .ok_or_else(|| RuntimeError::NotFound(format!("model \"{id}\" is not in the library")))
    }

    /// Removes a model from the library. The file is deleted only when
    /// `delete_file` is set and JARVIS manages it.
    pub async fn remove(&self, id: &str, delete_file: bool) -> Result<()> {
        let model = self.get(id).ok_or_else(|| {
            RuntimeError::NotFound(format!("model \"{id}\" is not in the library"))
        })?;
        if delete_file && !model.managed {
            return Err(RuntimeError::PermissionDenied(
                "JARVIS only deletes files it downloaded; remove the imported file yourself".into(),
            ));
        }
        self.models
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|m| m.id != id);
        self.save().await?;
        if delete_file {
            match tokio::fs::remove_file(&model.path).await {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                Err(err) => return Err(RuntimeError::io("deleting the model file", err)),
            }
            // A downloaded projector goes with its model unless another
            // model uses it too.
            if let Some(projector) = &model.projector
                && is_inside(Path::new(&projector.path), &self.dir).await
                && !self.list().iter().any(|m| {
                    m.projector
                        .as_ref()
                        .is_some_and(|p| p.path == projector.path)
                })
            {
                let _ = tokio::fs::remove_file(&projector.path).await;
            }
        }
        Ok(())
    }

    async fn add(&self, mut model: LocalModel) -> Result<LocalModel> {
        {
            let mut models = self.models.write().unwrap_or_else(PoisonError::into_inner);
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
        }
        self.save().await?;
        model.available = true;
        Ok(model)
    }

    async fn save(&self) -> Result<()> {
        let _guard = self.write_lock.lock().await;
        let index = Index {
            version: INDEX_VERSION,
            models: self
                .models
                .read()
                .unwrap_or_else(PoisonError::into_inner)
                .clone(),
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
