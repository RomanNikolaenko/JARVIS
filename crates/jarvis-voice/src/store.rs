//! Installed voice packages: verified download, safe extraction, and
//! integrity checks of engine programs before every launch.
//!
//! Each package lives in `<voice dir>/<id>/`; `jarvis-package.json` is
//! written last, so a package without it is not installed.

use std::path::{Path, PathBuf};

use jarvis_runtime::download::{self, DownloadHandle, Expected};
use jarvis_runtime::{DownloadPhase, FileDigest, installer};
use serde::{Deserialize, Serialize};
use url::Url;

use crate::catalog::PackageSpec;
use crate::error::{Result, VoiceError};

const MARKER: &str = "jarvis-package.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Marker {
    id: String,
    /// Downloaded files and their SHA-256.
    files: Vec<FileDigest>,
    /// Engines: the program, relative to the package directory.
    executable: Option<String>,
    executable_sha256: Option<String>,
    /// Engines: shared libraries next to the program.
    libraries: Vec<FileDigest>,
    installed_at_ms: u64,
}

#[derive(Debug, Clone)]
pub struct PackageStore {
    root: PathBuf,
}

impl PackageStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn dir(&self, id: &str) -> PathBuf {
        self.root.join(id)
    }

    /// A downloaded file of an installed model or voice.
    pub fn file(&self, spec: &PackageSpec, index: usize) -> Option<PathBuf> {
        spec.files
            .get(index)
            .map(|f| self.dir(spec.id).join(&f.name))
    }

    pub fn is_installed(&self, spec: &PackageSpec) -> bool {
        self.marker(spec.id).is_some_and(|m| m.id == spec.id)
            && spec.files.iter().all(|f| {
                spec.is_engine()
                    || std::fs::metadata(self.dir(spec.id).join(&f.name))
                        .is_ok_and(|m| m.len() == f.size)
            })
    }

    fn marker(&self, id: &str) -> Option<Marker> {
        let text = std::fs::read_to_string(self.dir(id).join(MARKER)).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// Downloads, verifies and installs a package. Engines are extracted;
    /// the previous installation is replaced only after everything succeeded.
    pub async fn install(
        &self,
        spec: &PackageSpec,
        client: &reqwest::Client,
        handle: &DownloadHandle,
    ) -> Result<()> {
        let staging = self.root.join(format!("{}.partial", spec.id));
        let downloads = self.root.join("downloads");
        let _ = tokio::fs::remove_dir_all(&staging).await;
        tokio::fs::create_dir_all(&staging)
            .await
            .map_err(|err| VoiceError::io("creating the voice directory", err))?;

        let mut files = Vec::new();
        for file in &spec.files {
            let url = Url::parse(&file.url).map_err(|err| VoiceError::Invalid(err.to_string()))?;
            download::check_source(&url)?;
            let dest = if spec.is_engine() {
                downloads.join(&file.name)
            } else {
                staging.join(&file.name)
            };
            let expected = Expected {
                sha256: file.sha256.to_owned(),
                size: Some(file.size),
            };
            download::fetch_verified(client, &url, &dest, &expected, handle).await?;
            files.push(FileDigest {
                name: file.name.clone(),
                sha256: file.sha256.to_owned(),
            });
        }

        handle.phase(DownloadPhase::Installing);
        let mut marker = Marker {
            id: spec.id.to_owned(),
            files,
            executable: None,
            executable_sha256: None,
            libraries: Vec::new(),
            installed_at_ms: now_ms(),
        };
        if let (Some(executable), Some(file)) = (spec.executable, spec.files.first()) {
            let archive = downloads.join(&file.name);
            {
                let (archive, staging) = (archive.clone(), staging.clone());
                tokio::task::spawn_blocking(move || installer::extract_archive(&archive, &staging))
                    .await
                    .map_err(|err| VoiceError::Install(err.to_string()))??;
            }
            let _ = tokio::fs::remove_file(&archive).await;
            let program = find_file(&staging, executable, 4).ok_or_else(|| {
                VoiceError::Install(format!("the archive does not contain {executable}"))
            })?;
            make_executable(&program)?;
            let relative = program
                .strip_prefix(&staging)
                .map_err(|_| VoiceError::Install("unexpected archive layout".into()))?;
            marker.executable = Some(relative.to_string_lossy().replace('\\', "/"));
            marker.executable_sha256 = Some(download::sha256_file(&program).await?);
            marker.libraries = installer::library_digests(&program).await?;
        }

        let final_dir = self.dir(spec.id);
        let _ = tokio::fs::remove_dir_all(&final_dir).await;
        tokio::fs::rename(&staging, &final_dir)
            .await
            .map_err(|err| VoiceError::io("activating the voice package", err))?;
        let json = serde_json::to_vec_pretty(&marker)
            .map_err(|err| VoiceError::Install(err.to_string()))?;
        tokio::fs::write(final_dir.join(MARKER), json)
            .await
            .map_err(|err| VoiceError::io("saving the package record", err))?;
        Ok(())
    }

    /// Deletes an installed package (after the user confirmed it).
    pub async fn remove(&self, id: &str) -> Result<()> {
        let dir = self.dir(id);
        match tokio::fs::remove_dir_all(&dir).await {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(VoiceError::io(format!("deleting {}", dir.display()), err)),
        }
    }

    /// The engine program, after checking that it and its libraries are
    /// unchanged since installation.
    pub async fn verified_program(&self, spec: &PackageSpec) -> Result<PathBuf> {
        let not_installed =
            || VoiceError::NotInstalled(format!("{} is not installed", spec.display_name));
        let marker = self.marker(spec.id).ok_or_else(not_installed)?;
        let (Some(relative), Some(expected)) = (&marker.executable, &marker.executable_sha256)
        else {
            return Err(not_installed());
        };
        let program = self.dir(spec.id).join(relative);
        let changed = || {
            VoiceError::Engine(format!(
                "{} was modified after installation; reinstall it in Settings → Voice",
                spec.display_name
            ))
        };
        if &download::sha256_file(&program)
            .await
            .map_err(|_| not_installed())?
            != expected
        {
            return Err(changed());
        }
        if installer::library_digests(&program).await? != marker.libraries {
            return Err(changed());
        }
        Ok(program)
    }

    /// Records a package as installed without downloading it (tests).
    #[doc(hidden)]
    pub fn mark_installed_for_tests(&self, spec: &PackageSpec) -> Result<()> {
        let dir = self.dir(spec.id);
        std::fs::create_dir_all(&dir)
            .map_err(|err| VoiceError::io("creating a test package", err))?;
        for file in &spec.files {
            if !spec.is_engine() {
                // Full length without writing the data.
                let created =
                    std::fs::File::create(dir.join(&file.name)).and_then(|f| f.set_len(file.size));
                created.map_err(|err| VoiceError::io("writing", err))?;
            }
        }
        let marker = Marker {
            id: spec.id.to_owned(),
            files: Vec::new(),
            executable: None,
            executable_sha256: None,
            libraries: Vec::new(),
            installed_at_ms: now_ms(),
        };
        let json =
            serde_json::to_vec(&marker).map_err(|err| VoiceError::Install(err.to_string()))?;
        std::fs::write(dir.join(MARKER), json).map_err(|err| VoiceError::io("writing", err))
    }
}

fn find_file(dir: &Path, name: &str, depth: u32) -> Option<PathBuf> {
    let entries = std::fs::read_dir(dir).ok()?;
    let mut subdirs = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            subdirs.push(path);
        } else if entry
            .file_name()
            .to_string_lossy()
            .eq_ignore_ascii_case(name)
        {
            return Some(path);
        }
    }
    if depth == 0 {
        return None;
    }
    subdirs.sort();
    subdirs
        .iter()
        .find_map(|sub| find_file(sub, name, depth - 1))
}

fn make_executable(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .map_err(|err| VoiceError::io("marking the engine executable", err))?;
    }
    let _ = path;
    Ok(())
}

fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog;

    #[test]
    fn a_package_without_its_record_or_files_is_not_installed() {
        let temp = tempfile::tempdir().expect("tempdir");
        let store = PackageStore::new(temp.path());
        let Some(model) = catalog::find("whisper-base-q5_1") else {
            return; // platform without voice packages
        };
        assert!(!store.is_installed(&model));
        store.mark_installed_for_tests(&model).expect("mark");
        assert!(store.is_installed(&model));
        // A partly written file does not count.
        let path = store.file(&model, 0).expect("file");
        std::fs::File::options()
            .write(true)
            .open(&path)
            .and_then(|f| f.set_len(1000))
            .expect("truncate");
        assert!(!store.is_installed(&model));
        store.mark_installed_for_tests(&model).expect("mark");
        std::fs::remove_file(store.dir(model.id).join(MARKER)).expect("remove");
        assert!(!store.is_installed(&model));
    }

    #[test]
    fn finds_programs_in_nested_folders() {
        let temp = tempfile::tempdir().expect("tempdir");
        let nested = temp.path().join("Release").join("bin");
        std::fs::create_dir_all(&nested).expect("dirs");
        std::fs::write(nested.join("Whisper-Server.EXE"), b"x").expect("write");
        assert_eq!(
            find_file(temp.path(), "whisper-server.exe", 4),
            Some(nested.join("Whisper-Server.EXE"))
        );
        assert_eq!(find_file(temp.path(), "whisper-server.exe", 1), None);
    }
}
