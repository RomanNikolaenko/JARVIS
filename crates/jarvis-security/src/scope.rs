//! Scoped file access: tools may only touch paths inside the workspace
//! folders the user chose, never JARVIS's own data, and paths are compared
//! after canonicalization so `..`, symlinks and junctions cannot escape.

use std::path::{Component, Path, PathBuf};

/// Why a path was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScopeError {
    #[error("no workspace folder is configured; add one under Agents → Tools & permissions")]
    NoWorkspace,
    #[error("{0} is outside the workspace folders")]
    Outside(String),
    #[error("{0} belongs to JARVIS itself and cannot be used by tools")]
    Protected(String),
    #[error("invalid path {0}")]
    Invalid(String),
}

/// Directories tools may use, and directories they must never touch.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileScope {
    roots: Vec<PathBuf>,
    protected: Vec<PathBuf>,
}

impl FileScope {
    /// Roots that do not exist are ignored; protected directories are kept
    /// even if they do not exist yet.
    pub fn new(
        roots: impl IntoIterator<Item = PathBuf>,
        protected: impl IntoIterator<Item = PathBuf>,
    ) -> Self {
        let roots = roots
            .into_iter()
            .filter_map(|root| match std::fs::canonicalize(&root) {
                Ok(path) if path.is_dir() => Some(simplify(path)),
                _ => {
                    // Built before every agent step: the UI shows missing
                    // folders, so this is not repeated as a warning.
                    tracing::debug!("workspace folder {} is unavailable", root.display());
                    None
                }
            })
            .collect();
        // Resolved like the paths they are compared with, so short (8.3)
        // names or links in the given form cannot make them miss.
        let protected = protected
            .into_iter()
            .map(|dir| canonical_with_missing_tail(&dir).unwrap_or(dir))
            .collect();
        Self { roots, protected }
    }

    pub fn roots(&self) -> &[PathBuf] {
        &self.roots
    }

    /// How a resolved path is shown to the model: relative to the first
    /// workspace folder with `/` separators (`.` for the folder itself),
    /// otherwise as it is. Short paths save tokens, and models copy them
    /// back without escaping mistakes.
    pub fn relative(&self, path: &Path) -> String {
        if let Some(rest) = self
            .roots
            .first()
            .and_then(|root| path.strip_prefix(root).ok())
        {
            let parts: Vec<String> = rest
                .components()
                .map(|part| part.as_os_str().to_string_lossy().into_owned())
                .collect();
            return if parts.is_empty() {
                ".".to_owned()
            } else {
                parts.join("/")
            };
        }
        path.display().to_string()
    }

    /// Resolves a path given by a model or the user: absolute, or relative
    /// to the first workspace folder. A path that does not exist yet is
    /// resolved through its nearest existing parent.
    pub fn resolve(&self, raw: &str) -> Result<PathBuf, ScopeError> {
        let raw = raw.trim();
        if raw.is_empty() || raw.contains('\0') {
            return Err(ScopeError::Invalid(raw.to_owned()));
        }
        let first = self.roots.first().ok_or(ScopeError::NoWorkspace)?;
        let given = Path::new(raw);
        let joined = if given.is_absolute() {
            given.to_path_buf()
        } else {
            first.join(given)
        };
        let resolved = canonical_with_missing_tail(&joined)?;
        self.check(&resolved)?;
        Ok(resolved)
    }

    /// Re-checks a resolved path right before it is used (it may have been
    /// replaced by a link in the meantime).
    pub fn ensure_inside(&self, path: &Path) -> Result<PathBuf, ScopeError> {
        let resolved = canonical_with_missing_tail(path)?;
        self.check(&resolved)?;
        Ok(resolved)
    }

    fn check(&self, path: &Path) -> Result<(), ScopeError> {
        if self.roots.is_empty() {
            return Err(ScopeError::NoWorkspace);
        }
        let shown = path.display().to_string();
        if self.protected.iter().any(|dir| is_within(path, dir)) {
            return Err(ScopeError::Protected(shown));
        }
        if !self.roots.iter().any(|root| is_within(path, root)) {
            return Err(ScopeError::Outside(shown));
        }
        Ok(())
    }
}

/// `path` is `dir` or inside it. Windows file names are case-insensitive:
/// the part of a path that does not exist yet keeps the case it was given
/// in, so `.JARVIS\x` must still match a protected `.jarvis`.
fn is_within(path: &Path, dir: &Path) -> bool {
    if cfg!(windows) {
        let mut parts = path.components();
        dir.components().all(|expected| {
            parts.next().is_some_and(|part| {
                part.as_os_str().to_string_lossy().to_lowercase()
                    == expected.as_os_str().to_string_lossy().to_lowercase()
            })
        })
    } else {
        path.starts_with(dir)
    }
}

/// Canonical form of `path`; for a path that does not exist yet, the
/// canonical form of its nearest existing ancestor plus the remaining plain
/// components (`..` in the missing part is rejected).
fn canonical_with_missing_tail(path: &Path) -> Result<PathBuf, ScopeError> {
    let invalid = || ScopeError::Invalid(path.display().to_string());
    let mut existing = path.to_path_buf();
    let mut tail = Vec::new();
    loop {
        match std::fs::canonicalize(&existing) {
            Ok(canonical) => {
                let mut resolved = simplify(canonical);
                for part in tail.iter().rev() {
                    resolved.push(part);
                }
                return Ok(resolved);
            }
            Err(_) => {
                let name = match existing.components().next_back() {
                    Some(Component::Normal(name)) => name.to_owned(),
                    _ => return Err(invalid()),
                };
                tail.push(name);
                if !existing.pop() {
                    return Err(invalid());
                }
            }
        }
    }
}

/// Strips the Windows verbatim prefix (`\\?\`) so paths compare and display
/// like the user typed them.
fn simplify(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        Some(rest) if !rest.starts_with("UNC") => PathBuf::from(rest),
        _ => path,
    }
}

const SECRET_DIRS: &[&str] = &[
    ".ssh",
    ".gnupg",
    ".aws",
    ".azure",
    ".kube",
    ".docker",
    ".password-store",
];
const SECRET_FILES: &[&str] = &[
    "id_rsa",
    "id_dsa",
    "id_ecdsa",
    "id_ed25519",
    ".netrc",
    ".npmrc",
    ".pypirc",
    ".git-credentials",
];
const SECRET_EXTENSIONS: &[&str] = &["pem", "key", "p12", "pfx", "kdbx", "keystore"];

/// Whether a path looks like it holds secrets (keys, credentials, `.env`).
pub fn is_sensitive(path: &Path) -> bool {
    let lower = |s: &std::ffi::OsStr| s.to_string_lossy().to_lowercase();
    let in_secret_dir = path.components().any(|c| match c {
        Component::Normal(name) => SECRET_DIRS.contains(&lower(name).as_str()),
        _ => false,
    });
    let Some(name) = path.file_name().map(lower) else {
        return in_secret_dir;
    };
    let extension = path.extension().map(lower).unwrap_or_default();
    in_secret_dir
        || SECRET_FILES.contains(&name.as_str())
        || SECRET_EXTENSIONS.contains(&extension.as_str())
        || name == ".env"
        || name.starts_with(".env.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scope() -> (tempfile::TempDir, FileScope) {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir_all(dir.path().join("work").join("src")).expect("mkdir");
        std::fs::create_dir_all(dir.path().join("jarvis-data")).expect("mkdir");
        std::fs::create_dir_all(dir.path().join("elsewhere")).expect("mkdir");
        let scope = FileScope::new(
            [dir.path().join("work"), dir.path().join("missing")],
            [dir.path().join("work").join(".jarvis")],
        );
        (dir, scope)
    }

    #[test]
    fn paths_resolve_inside_the_workspace() {
        let (_dir, scope) = scope();
        assert_eq!(scope.roots().len(), 1, "missing roots are ignored");
        let root = scope.roots()[0].clone();
        assert_eq!(scope.resolve("src").expect("relative"), root.join("src"));
        assert_eq!(
            scope.resolve("src/new/file.txt").expect("missing tail"),
            root.join("src").join("new").join("file.txt")
        );
        let absolute = root.join("src").display().to_string();
        assert_eq!(
            scope.resolve(&absolute).expect("absolute"),
            root.join("src")
        );
    }

    #[test]
    fn escapes_and_protected_paths_are_refused() {
        let (dir, scope) = scope();
        assert!(matches!(
            scope.resolve("../elsewhere"),
            Err(ScopeError::Outside(_))
        ));
        assert!(matches!(
            scope.resolve("src/../../elsewhere/x.txt"),
            Err(ScopeError::Outside(_))
        ));
        // Windows resolves `..` lexically (outside), Linux refuses `..` after
        // a missing directory (invalid): refused either way.
        assert!(matches!(
            scope.resolve("missing-dir/../../../x"),
            Err(ScopeError::Invalid(_) | ScopeError::Outside(_))
        ));
        assert!(matches!(
            scope.resolve(&dir.path().join("elsewhere").display().to_string()),
            Err(ScopeError::Outside(_))
        ));
        assert!(matches!(
            scope.resolve(".jarvis/state.json"),
            Err(ScopeError::Protected(_))
        ));
        // Windows names ignore case, also in the part that does not exist.
        #[cfg(windows)]
        assert!(matches!(
            scope.resolve(".JARVIS/state.json"),
            Err(ScopeError::Protected(_))
        ));
        assert!(matches!(scope.resolve("  "), Err(ScopeError::Invalid(_))));
        assert!(matches!(
            FileScope::default().resolve("x"),
            Err(ScopeError::NoWorkspace)
        ));
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_cannot_escape() {
        let (dir, scope) = scope();
        std::os::unix::fs::symlink(
            dir.path().join("elsewhere"),
            dir.path().join("work").join("link"),
        )
        .expect("symlink");
        assert!(matches!(
            scope.resolve("link/secret.txt"),
            Err(ScopeError::Outside(_))
        ));
    }

    #[cfg(windows)]
    #[test]
    fn junctions_cannot_escape() {
        let (dir, scope) = scope();
        let status = std::process::Command::new("cmd")
            .arg("/C")
            .arg("mklink")
            .arg("/J")
            .arg(dir.path().join("work").join("link"))
            .arg(dir.path().join("elsewhere"))
            .stdout(std::process::Stdio::null())
            .status()
            .expect("mklink");
        assert!(status.success());
        assert!(matches!(
            scope.resolve("link/secret.txt"),
            Err(ScopeError::Outside(_))
        ));
    }

    #[test]
    fn secret_files_are_recognised() {
        for path in [
            "/home/u/.ssh/config",
            "C:/Users/u/.aws/credentials",
            "project/.env",
            "project/.env.local",
            "certs/server.PEM",
            "keys/id_ed25519",
        ] {
            assert!(is_sensitive(Path::new(path)), "{path}");
        }
        for path in [
            "project/src/main.rs",
            "notes/environment.md",
            "a/.envrc-docs/x.md",
        ] {
            assert!(!is_sensitive(Path::new(path)), "{path}");
        }
    }
}
