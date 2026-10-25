//! Turning a plugin's launch settings into a program to run, and the
//! fingerprint the user approves.

use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};

use jarvis_security::is_plain_program_name;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::error::{PluginError, Result};
use crate::tool::hex;

/// Files larger than this are not read for the fingerprint.
const MAX_HASHED_FILE: u64 = 512 * 1024 * 1024;
/// Programs that download and run packages when they start.
const LAUNCHERS: &[&str] = &[
    "npx", "npm", "pnpm", "pnpx", "yarn", "bunx", "uvx", "pipx", "docker", "podman",
];
/// Programs that run code they load from elsewhere.
const INTERPRETERS: &[&str] = &[
    "node",
    "deno",
    "bun",
    "python",
    "python3",
    "py",
    "pythonw",
    "uv",
    "ruby",
    "php",
    "java",
    "dotnet",
    "pwsh",
    "powershell",
    "perl",
];

/// How a plugin's server is started, as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct LaunchConfig {
    pub program: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Working folder; the plugin folder, or a folder JARVIS chooses, when
    /// unset.
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

/// Launch settings with the program found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResolvedLaunch {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: BTreeMap<String, String>,
    pub warnings: Vec<String>,
}

fn stem(program: &Path) -> String {
    program
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default()
}

fn find_in_path(program: &str) -> Result<PathBuf> {
    jarvis_tools::resolve_program(program).ok_or_else(|| {
        PluginError::Invalid(format!("the program \"{program}\" was not found in PATH"))
    })
}

/// Finds the program and the working folder. `folder` is the plugin folder
/// (manifest plugins): a file there is used first, then a program of that
/// name in `PATH`. `default_cwd` is used for servers without a folder.
pub(crate) fn resolve(
    config: &LaunchConfig,
    folder: Option<&Path>,
    default_cwd: &Path,
) -> Result<ResolvedLaunch> {
    let program = if let Some(folder) = folder {
        let relative = Path::new(&config.program);
        if relative.is_absolute() {
            return Err(PluginError::Invalid(
                "server.command must be a program name or a path inside the plugin folder".into(),
            ));
        }
        let path = folder.join(relative);
        if path.is_file() {
            let inside = match (path.canonicalize(), folder.canonicalize()) {
                (Ok(path), Ok(folder)) => path.starts_with(folder),
                _ => false,
            };
            if !inside {
                return Err(PluginError::Invalid(format!(
                    "server.command \"{}\" is outside the plugin folder",
                    config.program
                )));
            }
            path
        } else if is_plain_program_name(&config.program) {
            find_in_path(&config.program)?
        } else {
            return Err(PluginError::Invalid(format!(
                "server.command \"{}\" is not a file inside the plugin folder",
                config.program
            )));
        }
    } else if is_plain_program_name(&config.program) {
        find_in_path(&config.program)?
    } else {
        let path = PathBuf::from(&config.program);
        if !path.is_absolute() {
            return Err(PluginError::Invalid(
                "the program must be a name found in PATH or an absolute path".into(),
            ));
        }
        if !path.is_file() {
            return Err(PluginError::Invalid(format!(
                "the program {} does not exist",
                path.display()
            )));
        }
        path
    };

    let cwd = match (folder, &config.cwd) {
        (Some(folder), _) => folder.to_path_buf(),
        (None, Some(cwd)) => {
            let cwd = PathBuf::from(cwd);
            if !cwd.is_absolute() || !cwd.is_dir() {
                return Err(PluginError::Invalid(format!(
                    "the working folder {} does not exist",
                    cwd.display()
                )));
            }
            cwd
        }
        (None, None) => default_cwd.to_path_buf(),
    };

    let mut warnings = Vec::new();
    let name = stem(&program);
    if LAUNCHERS.contains(&name.as_str()) {
        warnings.push(format!(
            "{name} downloads and runs packages when the server starts; JARVIS cannot check what it runs."
        ));
    }
    Ok(ResolvedLaunch {
        program,
        args: config.args.clone(),
        cwd,
        env: config.env.clone(),
        warnings,
    })
}

fn hash_file(path: &Path) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0; 256 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

/// SHA-256 over the launch settings, the program file, the manifest and
/// every argument that names a file. Adds a warning when an interpreter
/// runs code that no argument names. Reads files: call it off the async
/// runtime.
pub(crate) fn fingerprint(
    launch: &mut ResolvedLaunch,
    secrets: &[String],
    manifest: Option<&[u8]>,
) -> Result<String> {
    let mut hasher = Sha256::new();
    hasher.update(b"jarvis-plugin-fingerprint-v1\0");
    let settings = serde_json::json!({
        "program": launch.program.to_string_lossy(),
        "args": launch.args,
        "cwd": launch.cwd.to_string_lossy(),
        "env": launch.env,
        "secrets": secrets,
    });
    hasher.update(settings.to_string().as_bytes());
    let program = hash_file(&launch.program)
        .map_err(|err| PluginError::io(format!("reading {}", launch.program.display()), err))?;
    hasher.update(b"\0program\0");
    hasher.update(program.as_bytes());
    if let Some(manifest) = manifest {
        hasher.update(b"\0manifest\0");
        hasher.update(Sha256::digest(manifest));
    }
    let mut files = 0;
    for arg in &launch.args {
        let path = Path::new(arg);
        let path = if path.is_absolute() {
            path.to_path_buf()
        } else {
            launch.cwd.join(path)
        };
        let Ok(metadata) = std::fs::metadata(&path) else {
            continue;
        };
        if !metadata.is_file() || metadata.len() > MAX_HASHED_FILE {
            continue;
        }
        let hash = hash_file(&path)
            .map_err(|err| PluginError::io(format!("reading {}", path.display()), err))?;
        hasher.update(b"\0file\0");
        hasher.update(arg.as_bytes());
        hasher.update(hash.as_bytes());
        files += 1;
    }
    let name = stem(&launch.program);
    if files == 0 && INTERPRETERS.contains(&name.as_str()) {
        launch.warnings.push(format!(
            "The fingerprint covers {name} itself but not the code it loads (no argument names a file)."
        ));
    }
    Ok(hex(&hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(program: &str, args: &[&str]) -> LaunchConfig {
        LaunchConfig {
            program: program.to_owned(),
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            cwd: None,
            env: BTreeMap::new(),
        }
    }

    #[test]
    fn programs_stay_inside_the_plugin_folder() {
        let dir = tempfile::tempdir().ok();
        let Some(dir) = dir else { return };
        let plugin = dir.path().join("plugin");
        std::fs::create_dir_all(plugin.join("bin")).ok();
        std::fs::write(plugin.join("bin").join("server"), b"x").ok();
        std::fs::write(dir.path().join("outside"), b"x").ok();
        let inside = resolve(&config("bin/server", &[]), Some(&plugin), dir.path());
        assert_eq!(inside.ok().map(|l| l.cwd), Some(plugin.clone()));
        assert!(resolve(&config("../outside", &[]), Some(&plugin), dir.path()).is_err());
        let absolute = dir.path().join("outside");
        let absolute = absolute.to_string_lossy();
        assert!(resolve(&config(&absolute, &[]), Some(&plugin), dir.path()).is_err());
        // Without a plugin folder, absolute paths are fine, relative ones not.
        assert!(resolve(&config(&absolute, &[]), None, dir.path()).is_ok());
        assert!(resolve(&config("bin/server", &[]), None, dir.path()).is_err());
    }

    #[test]
    fn the_fingerprint_follows_files_and_settings() {
        let dir = tempfile::tempdir().ok();
        let Some(dir) = dir else { return };
        let program = dir.path().join("server");
        let script = dir.path().join("main.js");
        std::fs::write(&program, b"program").ok();
        std::fs::write(&script, b"script v1").ok();
        let launch =
            |args: &[&str]| resolve(&config(&program.to_string_lossy(), args), None, dir.path());
        let fingerprint_of = |args: &[&str], secrets: &[String]| {
            launch(args)
                .ok()
                .and_then(|mut l| fingerprint(&mut l, secrets, None).ok())
        };
        let first = fingerprint_of(&["main.js"], &[]);
        assert!(first.is_some());
        assert_eq!(first, fingerprint_of(&["main.js"], &[]));
        std::fs::write(&script, b"script v2").ok();
        let changed_script = fingerprint_of(&["main.js"], &[]);
        assert_ne!(first, changed_script);
        assert_ne!(changed_script, fingerprint_of(&["main.js", "--flag"], &[]));
        assert_ne!(
            changed_script,
            fingerprint_of(&["main.js"], &["API_KEY".to_owned()])
        );
        std::fs::write(&program, b"program v2").ok();
        assert_ne!(changed_script, fingerprint_of(&["main.js"], &[]));
    }
}
