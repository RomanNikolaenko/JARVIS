//! Installation of official llama.cpp builds from GitHub releases.
//!
//! Safety rules:
//! - only `github.com/ggml-org/llama.cpp` release assets with a GitHub-provided
//!   SHA-256 digest are accepted;
//! - the user approves a concrete release (tag, asset, size, digest); the core
//!   re-fetches that release and checks it still matches before downloading;
//! - archives are extracted without following paths outside the target
//!   directory, and the server is run once with `--version` to validate it.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use serde::Deserialize;
use url::Url;

use crate::error::{Result, RuntimeError};
use crate::types::{FileDigest, RuntimeBackend, RuntimeRelease};

pub const LLAMA_CPP_REPO: &str = "ggml-org/llama.cpp";
const RELEASES_TO_SCAN: u32 = 10;
const MAX_ARCHIVE_ENTRIES: usize = 10_000;
const MAX_EXTRACTED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
/// More shared libraries than this next to `llama-server` is not a llama.cpp
/// build folder.
const MAX_LIBRARIES: usize = 512;
const VERSION_TIMEOUT: Duration = Duration::from_secs(20);

/// Name of the server executable on this platform.
pub fn server_executable_name() -> &'static str {
    if cfg!(windows) {
        "llama-server.exe"
    } else {
        "llama-server"
    }
}

/// Release asset name for a backend on the current OS and architecture, e.g.
/// `llama-b6000-bin-win-vulkan-x64.zip`. `None` if the combination has no
/// official build.
pub fn asset_name(tag: &str, backend: RuntimeBackend, os: &str, arch: &str) -> Option<String> {
    let arch = match arch {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        _ => return None,
    };
    let flavour = match (os, backend) {
        ("windows", RuntimeBackend::Cpu) => format!("win-cpu-{arch}.zip"),
        ("windows", RuntimeBackend::Vulkan) => format!("win-vulkan-{arch}.zip"),
        ("linux", RuntimeBackend::Cpu) => format!("ubuntu-{arch}.tar.gz"),
        ("linux", RuntimeBackend::Vulkan) => format!("ubuntu-vulkan-{arch}.tar.gz"),
        _ => return None,
    };
    Some(format!("llama-{tag}-bin-{flavour}"))
}

fn current_asset_name(tag: &str, backend: RuntimeBackend) -> Option<String> {
    asset_name(tag, backend, std::env::consts::OS, std::env::consts::ARCH)
}

#[derive(Debug, Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    published_at: Option<String>,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Debug, Deserialize)]
struct GithubAsset {
    name: String,
    size: u64,
    browser_download_url: String,
    #[serde(default)]
    digest: Option<String>,
}

fn is_build_tag(tag: &str) -> bool {
    tag.strip_prefix('b')
        .is_some_and(|n| !n.is_empty() && n.chars().all(|c| c.is_ascii_digit()))
}

/// Picks the newest build release that has the asset for `backend`.
fn select_release(releases: &[GithubRelease], backend: RuntimeBackend) -> Result<RuntimeRelease> {
    releases
        .iter()
        .filter(|r| !r.draft && is_build_tag(&r.tag_name))
        .find_map(|release| to_release(release, backend).ok())
        .ok_or_else(|| {
            RuntimeError::NotFound(format!(
                "no recent llama.cpp release provides a {backend:?} build for {} {}",
                std::env::consts::OS,
                std::env::consts::ARCH
            ))
        })
}

fn to_release(release: &GithubRelease, backend: RuntimeBackend) -> Result<RuntimeRelease> {
    let wanted = current_asset_name(&release.tag_name, backend).ok_or_else(|| {
        RuntimeError::NotFound(format!(
            "llama.cpp has no {backend:?} build for {} {}",
            std::env::consts::OS,
            std::env::consts::ARCH
        ))
    })?;
    let asset = release
        .assets
        .iter()
        .find(|a| a.name == wanted)
        .ok_or_else(|| {
            RuntimeError::NotFound(format!("{} has no asset {wanted}", release.tag_name))
        })?;
    let sha256 = asset
        .digest
        .as_deref()
        .and_then(|d| d.strip_prefix("sha256:"))
        .filter(|d| d.len() == 64 && d.chars().all(|c| c.is_ascii_hexdigit()))
        .ok_or_else(|| {
            RuntimeError::PermissionDenied(format!(
                "{wanted} has no published SHA-256 digest; refusing an unverifiable download"
            ))
        })?;
    let expected_prefix = format!("https://github.com/{LLAMA_CPP_REPO}/releases/download/");
    if !asset.browser_download_url.starts_with(&expected_prefix) {
        return Err(RuntimeError::PermissionDenied(
            "release asset is not hosted by the official llama.cpp repository".into(),
        ));
    }
    Ok(RuntimeRelease {
        tag: release.tag_name.clone(),
        published_at: release.published_at.clone().unwrap_or_default(),
        backend,
        asset_name: asset.name.clone(),
        download_url: asset.browser_download_url.clone(),
        size_bytes: asset.size,
        sha256: sha256.to_ascii_lowercase(),
    })
}

/// Finds the newest official build for `backend`.
pub async fn find_latest(
    client: &reqwest::Client,
    backend: RuntimeBackend,
) -> Result<RuntimeRelease> {
    let url = format!(
        "https://api.github.com/repos/{LLAMA_CPP_REPO}/releases?per_page={RELEASES_TO_SCAN}"
    );
    let releases: Vec<GithubRelease> = github_json(client, &url).await?;
    select_release(&releases, backend)
}

/// Re-fetches a release by tag and checks it matches what the user approved.
pub async fn verify_approved(
    client: &reqwest::Client,
    approved: &RuntimeRelease,
) -> Result<RuntimeRelease> {
    if !is_build_tag(&approved.tag) {
        return Err(RuntimeError::InvalidInput("invalid release tag".into()));
    }
    let url = format!(
        "https://api.github.com/repos/{LLAMA_CPP_REPO}/releases/tags/{}",
        approved.tag
    );
    let release: GithubRelease = github_json(client, &url).await?;
    let current = to_release(&release, approved.backend)?;
    if current.asset_name != approved.asset_name
        || current.sha256 != approved.sha256.to_ascii_lowercase()
        || current.download_url != approved.download_url
    {
        return Err(RuntimeError::PermissionDenied(
            "the release changed since it was approved; check for updates again".into(),
        ));
    }
    Ok(current)
}

async fn github_json<T: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    url: &str,
) -> Result<T> {
    let response = client
        .get(url)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .map_err(|err| RuntimeError::Unavailable(format!("cannot reach GitHub: {err}")))?;
    if response.status() == reqwest::StatusCode::FORBIDDEN
        || response.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
    {
        return Err(RuntimeError::Unavailable(
            "GitHub rate limit reached; try again later".into(),
        ));
    }
    response
        .error_for_status()
        .map_err(|err| RuntimeError::Unavailable(err.to_string()))?
        .json()
        .await
        .map_err(|err| RuntimeError::Unavailable(format!("unexpected GitHub response: {err}")))
}

/// Parsed download URL of an approved release.
pub fn download_url(release: &RuntimeRelease) -> Result<Url> {
    Url::parse(&release.download_url).map_err(|err| RuntimeError::InvalidInput(err.to_string()))
}

/// Extracts a `.zip` or `.tar.gz` archive into `dest`. Blocking.
pub fn extract_archive(archive: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)
        .map_err(|err| RuntimeError::io("creating the runtime directory", err))?;
    let name = archive
        .file_name()
        .map(|n| n.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if name.ends_with(".zip") {
        extract_zip(archive, dest)
    } else if name.ends_with(".tar.gz") || name.ends_with(".tgz") {
        extract_tar_gz(archive, dest)
    } else {
        Err(RuntimeError::InvalidInput(format!(
            "unsupported archive {name}"
        )))
    }
}

/// Rejects absolute paths and `..` components.
fn safe_relative(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => out.push(part),
            Component::CurDir => {}
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

fn extract_zip(archive: &Path, dest: &Path) -> Result<()> {
    let file =
        std::fs::File::open(archive).map_err(|err| RuntimeError::io("opening the archive", err))?;
    let mut zip = zip::ZipArchive::new(file)
        .map_err(|err| RuntimeError::InvalidInput(format!("invalid zip archive: {err}")))?;
    if zip.len() > MAX_ARCHIVE_ENTRIES {
        return Err(RuntimeError::InvalidInput(
            "archive has too many entries".into(),
        ));
    }
    let mut written = 0u64;
    for index in 0..zip.len() {
        let mut entry = zip
            .by_index(index)
            .map_err(|err| RuntimeError::InvalidInput(format!("invalid zip entry: {err}")))?;
        let Some(relative) = entry.enclosed_name().as_deref().and_then(safe_relative) else {
            return Err(RuntimeError::PermissionDenied(format!(
                "archive entry {} points outside the target directory",
                entry.name().map(|n| n.into_owned()).unwrap_or_default()
            )));
        };
        let target = dest.join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&target).map_err(|err| RuntimeError::io("extracting", err))?;
            continue;
        }
        if entry.is_symlink() {
            return Err(RuntimeError::PermissionDenied(
                "archive contains a symbolic link".into(),
            ));
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(|err| RuntimeError::io("extracting", err))?;
        }
        let mut out =
            std::fs::File::create(&target).map_err(|err| RuntimeError::io("extracting", err))?;
        let copied = std::io::copy(
            &mut (&mut entry).take(MAX_EXTRACTED_BYTES - written + 1),
            &mut out,
        )
        .map_err(|err| RuntimeError::io("extracting", err))?;
        written += copied;
        if written > MAX_EXTRACTED_BYTES {
            return Err(RuntimeError::InvalidInput(
                "archive expands to more than 4 GiB".into(),
            ));
        }
        #[cfg(unix)]
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            let _ =
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode & 0o755));
        }
    }
    Ok(())
}

fn extract_tar_gz(archive: &Path, dest: &Path) -> Result<()> {
    let file =
        std::fs::File::open(archive).map_err(|err| RuntimeError::io("opening the archive", err))?;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(file));
    tar.set_preserve_permissions(false);
    let mut written = 0u64;
    let entries = tar
        .entries()
        .map_err(|err| RuntimeError::InvalidInput(format!("invalid tar archive: {err}")))?;
    for (count, entry) in entries.enumerate() {
        if count >= MAX_ARCHIVE_ENTRIES {
            return Err(RuntimeError::InvalidInput(
                "archive has too many entries".into(),
            ));
        }
        let mut entry =
            entry.map_err(|err| RuntimeError::InvalidInput(format!("invalid tar entry: {err}")))?;
        let path = entry
            .path()
            .map_err(|err| RuntimeError::InvalidInput(err.to_string()))?
            .into_owned();
        let Some(relative) = safe_relative(&path) else {
            return Err(RuntimeError::PermissionDenied(format!(
                "archive entry {} points outside the target directory",
                path.display()
            )));
        };
        let kind = entry.header().entry_type();
        if kind.is_symlink() || kind.is_hard_link() {
            // Versioned shared-library links (libfoo.so -> libfoo.so.1) are
            // allowed only when they stay in the same directory.
            let target = entry
                .link_name()
                .map_err(|err| RuntimeError::InvalidInput(err.to_string()))?
                .map(|t| t.into_owned());
            let local = target.as_deref().is_some_and(|t| {
                t.components().count() == 1
                    && matches!(t.components().next(), Some(Component::Normal(_)))
            });
            if !local {
                return Err(RuntimeError::PermissionDenied(format!(
                    "archive contains a link leaving its directory: {}",
                    path.display()
                )));
            }
        } else if !(kind.is_file() || kind.is_dir()) {
            continue;
        }
        written += entry.header().size().unwrap_or(0);
        if written > MAX_EXTRACTED_BYTES {
            return Err(RuntimeError::InvalidInput(
                "archive expands to more than 4 GiB".into(),
            ));
        }
        let unpacked = entry
            .unpack_in(dest)
            .map_err(|err| RuntimeError::io(format!("extracting {}", relative.display()), err))?;
        if !unpacked {
            return Err(RuntimeError::PermissionDenied(format!(
                "archive entry {} was rejected",
                path.display()
            )));
        }
    }
    Ok(())
}

/// Finds the server executable in an extracted tree (depth ≤ 4).
pub fn locate_server(root: &Path) -> Option<PathBuf> {
    fn walk(dir: &Path, depth: u32) -> Option<PathBuf> {
        let entries = std::fs::read_dir(dir).ok()?;
        let mut dirs = Vec::new();
        for entry in entries.filter_map(std::result::Result::ok) {
            let path = entry.path();
            if path.is_dir() {
                dirs.push(path);
            } else if entry.file_name() == server_executable_name() {
                return Some(path);
            }
        }
        if depth == 0 {
            return None;
        }
        dirs.sort();
        dirs.into_iter().find_map(|d| walk(&d, depth - 1))
    }
    walk(root, 4)
}

/// Guesses the backend of a llama.cpp build from the libraries next to it.
pub fn detect_backend(server: &Path) -> RuntimeBackend {
    let Some(dir) = server.parent() else {
        return RuntimeBackend::Cpu;
    };
    let names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(std::result::Result::ok)
                .map(|e| e.file_name().to_string_lossy().to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default();
    if names.iter().any(|n| n.contains("ggml-cuda")) {
        RuntimeBackend::Cuda
    } else if names.iter().any(|n| n.contains("ggml-vulkan")) {
        RuntimeBackend::Vulkan
    } else if names
        .iter()
        .any(|n| n.contains("ggml-hip") || n.contains("ggml-sycl") || n.contains("ggml-opencl"))
    {
        RuntimeBackend::Other
    } else {
        RuntimeBackend::Cpu
    }
}

/// Whether a loader may pick `name` up as a shared library.
pub fn is_shared_library(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.ends_with(".dll")
        || name.ends_with(".so")
        || name.contains(".so.")
        || name.ends_with(".dylib")
}

/// SHA-256 of the shared libraries in the directory of `server` (not
/// recursive), sorted by name. Symlinks are followed, so retargeting one
/// changes its digest.
pub async fn library_digests(server: &Path) -> Result<Vec<FileDigest>> {
    let dir = server.parent().ok_or_else(|| {
        RuntimeError::InvalidInput(format!("{} has no parent directory", server.display()))
    })?;
    let context = || format!("reading {}", dir.display());
    let mut entries = tokio::fs::read_dir(dir)
        .await
        .map_err(|err| RuntimeError::io(context(), err))?;
    let mut libraries = Vec::new();
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|err| RuntimeError::io(context(), err))?
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if is_shared_library(&name) && tokio::fs::metadata(&path).await.is_ok_and(|m| m.is_file()) {
            libraries.push((name, path));
        }
        if libraries.len() > MAX_LIBRARIES {
            return Err(RuntimeError::InvalidInput(format!(
                "{} contains more than {MAX_LIBRARIES} shared libraries; use llama-server from a llama.cpp build folder",
                dir.display()
            )));
        }
    }
    libraries.sort();
    let mut digests = Vec::with_capacity(libraries.len());
    for (name, path) in libraries {
        let sha256 = crate::download::sha256_file(&path).await?;
        digests.push(FileDigest { name, sha256 });
    }
    Ok(digests)
}

/// Runs `llama-server --version` and returns the version line. This executes
/// the binary, so it is only called after the user approved it.
pub async fn probe_version(
    server: &Path,
    adapter: &dyn jarvis_platform::PlatformAdapter,
) -> Result<String> {
    let mut command = std::process::Command::new(server);
    command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(dir) = server.parent() {
        command.current_dir(dir);
    }
    adapter.prepare_background_command(&mut command);
    let mut command = tokio::process::Command::from(command);
    command.kill_on_drop(true);
    let output = tokio::time::timeout(VERSION_TIMEOUT, command.output())
        .await
        .map_err(|_| RuntimeError::InvalidInput("llama-server --version did not finish".into()))?
        .map_err(|err| RuntimeError::io(format!("running {}", server.display()), err))?;
    let text = format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let version = text
        .lines()
        .map(str::trim)
        .find(|line| line.to_ascii_lowercase().starts_with("version"))
        .map(str::to_owned);
    match (output.status.success(), version) {
        (true, Some(version)) => Ok(version),
        _ => Err(RuntimeError::InvalidInput(format!(
            "{} is not a working llama-server (exit {})",
            server.display(),
            output.status
        ))),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use sha2::{Digest, Sha256};

    use super::*;

    #[test]
    fn shared_libraries_are_recognised() {
        for name in [
            "ggml-vulkan.dll",
            "LLAMA.DLL",
            "libllama.so",
            "libggml.so.0.1",
            "libomp.dylib",
        ] {
            assert!(is_shared_library(name), "{name}");
        }
        for name in [
            "llama-server.exe",
            "llama-server",
            "LICENSE",
            "model.gguf",
            "notes.sol",
        ] {
            assert!(!is_shared_library(name), "{name}");
        }
    }

    #[tokio::test]
    async fn digests_cover_the_libraries_next_to_the_server() {
        let dir = tempfile::tempdir().expect("tempdir");
        let server = dir.path().join("llama-server.exe");
        std::fs::write(&server, b"launcher").expect("write");
        std::fs::write(dir.path().join("llama.dll"), b"engine").expect("write");
        std::fs::write(dir.path().join("libggml.so.1"), b"ggml").expect("write");
        std::fs::write(dir.path().join("README.md"), b"docs").expect("write");
        std::fs::create_dir(dir.path().join("sub")).expect("mkdir");
        std::fs::write(dir.path().join("sub").join("other.dll"), b"x").expect("write");

        let digests = library_digests(&server).await.expect("digests");
        let names: Vec<&str> = digests.iter().map(|d| d.name.as_str()).collect();
        assert_eq!(names, ["libggml.so.1", "llama.dll"]);
        assert_eq!(
            digests[1].sha256,
            crate::download::hex(&Sha256::digest(b"engine"))
        );
    }

    #[test]
    fn builds_asset_names_for_supported_platforms() {
        assert_eq!(
            asset_name("b11734", RuntimeBackend::Vulkan, "windows", "x86_64").as_deref(),
            Some("llama-b11734-bin-win-vulkan-x64.zip")
        );
        assert_eq!(
            asset_name("b11734", RuntimeBackend::Cpu, "linux", "x86_64").as_deref(),
            Some("llama-b11734-bin-ubuntu-x64.tar.gz")
        );
        assert_eq!(
            asset_name("b11734", RuntimeBackend::Cpu, "windows", "aarch64").as_deref(),
            Some("llama-b11734-bin-win-cpu-arm64.zip")
        );
        assert_eq!(
            asset_name("b1", RuntimeBackend::Cuda, "windows", "x86_64"),
            None
        );
        assert_eq!(
            asset_name("b1", RuntimeBackend::Cpu, "macos", "aarch64"),
            None
        );
    }

    fn release(tag: &str, digest: Option<&str>) -> GithubRelease {
        let cpu = current_asset_name(tag, RuntimeBackend::Cpu).unwrap_or_default();
        GithubRelease {
            tag_name: tag.into(),
            draft: false,
            published_at: Some("2026-10-19T00:00:00Z".into()),
            assets: vec![GithubAsset {
                browser_download_url: format!(
                    "https://github.com/{LLAMA_CPP_REPO}/releases/download/{tag}/{cpu}"
                ),
                name: cpu,
                size: 20_000_000,
                digest: digest.map(str::to_owned),
            }],
        }
    }

    #[test]
    fn selects_the_newest_verifiable_build() {
        if current_asset_name("b1", RuntimeBackend::Cpu).is_none() {
            return; // Platform without official builds.
        }
        let digest = format!("sha256:{}", "ab".repeat(32));
        let releases = vec![
            GithubRelease {
                tag_name: "v0.7.0".into(),
                draft: false,
                published_at: None,
                assets: Vec::new(),
            },
            release("b200", None),
            release("b199", Some(&digest)),
        ];
        let selected = select_release(&releases, RuntimeBackend::Cpu).expect("release");
        assert_eq!(selected.tag, "b199", "builds without a digest are skipped");
        assert_eq!(selected.sha256, "ab".repeat(32));

        let mut foreign = release("b300", Some(&digest));
        foreign.assets[0].browser_download_url = "https://evil.example/llama.zip".into();
        assert!(matches!(
            to_release(&foreign, RuntimeBackend::Cpu),
            Err(RuntimeError::PermissionDenied(_))
        ));
    }

    #[test]
    fn rejects_unsafe_paths() {
        assert!(safe_relative(Path::new("build/bin/llama-server")).is_some());
        assert!(safe_relative(Path::new("../evil")).is_none());
        assert!(safe_relative(Path::new("/etc/passwd")).is_none());
        assert!(safe_relative(Path::new("a/../../b")).is_none());
    }

    #[test]
    fn extracts_zip_archives_and_finds_the_server() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = dir.path().join("llama.zip");
        {
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).expect("create"));
            let options = zip::write::SimpleFileOptions::default();
            zip.start_file(server_executable_name(), options)
                .expect("start");
            zip.write_all(b"binary").expect("write");
            zip.start_file("ggml-vulkan.dll", options).expect("start");
            zip.write_all(b"lib").expect("write");
            zip.finish().expect("finish");
        }
        let out = dir.path().join("out");
        extract_archive(&archive, &out).expect("extract");
        let server = locate_server(&out).expect("server found");
        assert_eq!(std::fs::read(&server).expect("read"), b"binary");
        assert_eq!(detect_backend(&server), RuntimeBackend::Vulkan);
    }

    #[test]
    fn rejects_zip_slip() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = dir.path().join("evil.zip");
        {
            let mut zip = zip::ZipWriter::new(std::fs::File::create(&archive).expect("create"));
            zip.start_file("../evil.txt", zip::write::SimpleFileOptions::default())
                .expect("start");
            zip.write_all(b"x").expect("write");
            zip.finish().expect("finish");
        }
        let result = extract_archive(&archive, &dir.path().join("out"));
        assert!(
            matches!(result, Err(RuntimeError::PermissionDenied(_))),
            "{result:?}"
        );
        assert!(!dir.path().join("evil.txt").exists());
    }

    #[test]
    fn extracts_tar_gz_archives_and_rejects_escaping_links() {
        let dir = tempfile::tempdir().expect("tempdir");
        let archive = dir.path().join("llama.tar.gz");
        {
            let gz = flate2::write::GzEncoder::new(
                std::fs::File::create(&archive).expect("create"),
                flate2::Compression::fast(),
            );
            let mut tar = tar::Builder::new(gz);
            let mut header = tar::Header::new_gnu();
            header.set_size(6);
            header.set_mode(0o755);
            header.set_cksum();
            tar.append_data(
                &mut header,
                format!("build/bin/{}", server_executable_name()),
                &b"binary"[..],
            )
            .expect("append");
            tar.into_inner().expect("finish").finish().expect("gzip");
        }
        let out = dir.path().join("out");
        extract_archive(&archive, &out).expect("extract");
        assert!(locate_server(&out).is_some());

        let evil = dir.path().join("evil.tar.gz");
        {
            let gz = flate2::write::GzEncoder::new(
                std::fs::File::create(&evil).expect("create"),
                flate2::Compression::fast(),
            );
            let mut tar = tar::Builder::new(gz);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_cksum();
            tar.append_link(&mut header, "lib/libevil.so", "/etc/passwd")
                .expect("append");
            tar.into_inner().expect("finish").finish().expect("gzip");
        }
        let result = extract_archive(&evil, &dir.path().join("out2"));
        assert!(
            matches!(result, Err(RuntimeError::PermissionDenied(_))),
            "{result:?}"
        );
    }
}
