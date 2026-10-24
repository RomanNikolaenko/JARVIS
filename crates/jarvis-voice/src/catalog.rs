//! Voice packages offered for download: engines, recognition models and
//! voices. Every file is pinned to a release tag or repository commit with
//! its SHA-256, so what the user approves is exactly what gets installed.

use crate::types::{VoicePackage, VoicePackageKind};

const WHISPER_CPP_TAG: &str = "b5454";
const PIPER_TAG: &str = "2023.11.14-2";
/// ggerganov/whisper.cpp model repository commit.
const WHISPER_MODELS_REVISION: &str = "5359861c739e955e79d9a303bcbc70fb988958b1";
/// rhasspy/piper-voices commit.
const PIPER_VOICES_REVISION: &str = "5041a7a49ae5b3a0f30e6837769eb03fdbb231bd";

/// Engine ids (one of each kind per platform).
pub const WHISPER_ENGINE: &str = "whisper.cpp";
pub const PIPER_ENGINE: &str = "piper";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileSpec {
    pub url: String,
    /// File name on disk.
    pub name: String,
    pub sha256: &'static str,
    pub size: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageSpec {
    pub id: &'static str,
    pub kind: VoicePackageKind,
    pub display_name: &'static str,
    pub description: &'static str,
    pub language: Option<&'static str>,
    pub source: String,
    pub license: &'static str,
    pub speakers: &'static [&'static str],
    pub recommended: bool,
    pub files: Vec<FileSpec>,
    /// Engines: the program inside the extracted archive.
    pub executable: Option<&'static str>,
}

impl PackageSpec {
    pub fn size(&self) -> u64 {
        self.files.iter().map(|f| f.size).sum()
    }

    pub fn is_engine(&self) -> bool {
        matches!(
            self.kind,
            VoicePackageKind::SpeechEngine | VoicePackageKind::VoiceEngine
        )
    }

    pub fn describe(&self, installed: bool) -> VoicePackage {
        VoicePackage {
            id: self.id.to_owned(),
            kind: self.kind,
            display_name: self.display_name.to_owned(),
            description: self.description.to_owned(),
            language: self.language.map(str::to_owned),
            source: self.source.clone(),
            size_bytes: self.size(),
            license: self.license.to_owned(),
            speakers: self.speakers.iter().map(|s| (*s).to_owned()).collect(),
            recommended: self.recommended,
            installed,
        }
    }
}

/// Why voice is not offered on this platform, if it is not.
pub fn unsupported_reason() -> Option<&'static str> {
    if engine_files(WHISPER_ENGINE).is_none() || engine_files(PIPER_ENGINE).is_none() {
        Some("Voice engines are available for Windows and Linux on x86-64 only")
    } else {
        None
    }
}

/// All packages for the current platform.
pub fn packages() -> Vec<PackageSpec> {
    let mut all = Vec::new();
    if let Some(file) = engine_files(WHISPER_ENGINE) {
        all.push(PackageSpec {
            id: WHISPER_ENGINE,
            kind: VoicePackageKind::SpeechEngine,
            display_name: "whisper.cpp (speech recognition)",
            description: "Official CPU build of the whisper.cpp server. Runs only on this computer.",
            language: None,
            source: format!("github.com/ggml-org/whisper.cpp, build {WHISPER_CPP_TAG}"),
            license: "MIT",
            speakers: &[],
            recommended: true,
            files: vec![file],
            executable: Some(if cfg!(windows) { "whisper-server.exe" } else { "whisper-server" }),
        });
    }
    if let Some(file) = engine_files(PIPER_ENGINE) {
        all.push(PackageSpec {
            id: PIPER_ENGINE,
            kind: VoicePackageKind::VoiceEngine,
            display_name: "Piper (speech synthesis)",
            description: "Official Piper build. Speaks on this computer, without the internet.",
            language: None,
            source: format!("github.com/rhasspy/piper, release {PIPER_TAG}"),
            license: "MIT (bundles espeak-ng, GPL-3.0)",
            speakers: &[],
            recommended: true,
            files: vec![file],
            executable: Some(if cfg!(windows) { "piper.exe" } else { "piper" }),
        });
    }
    if all.is_empty() {
        return all;
    }
    all.extend(whisper_models());
    all.extend(voices());
    all
}

pub fn find(id: &str) -> Option<PackageSpec> {
    packages().into_iter().find(|p| p.id == id)
}

fn engine_files(id: &str) -> Option<FileSpec> {
    let github = |repo: &str, tag: &str, asset: &str, sha256: &'static str, size: u64| FileSpec {
        url: format!("https://github.com/{repo}/releases/download/{tag}/{asset}"),
        name: asset.to_owned(),
        sha256,
        size,
    };
    match (id, std::env::consts::OS, std::env::consts::ARCH) {
        (WHISPER_ENGINE, "windows", "x86_64") => Some(github(
            "ggml-org/whisper.cpp",
            WHISPER_CPP_TAG,
            "whisper-bin-x64.zip",
            "6ba69e3482d7826214f90a6a9c84ca07782aec1e1d0c6a7c30c994fd5d816ccb",
            8_928_640,
        )),
        (WHISPER_ENGINE, "linux", "x86_64") => Some(github(
            "ggml-org/whisper.cpp",
            WHISPER_CPP_TAG,
            "whisper-bin-ubuntu-x64.tar.gz",
            "a72becf15d7917f990f6313867a52638b82b7f9ef237fb0c980dac56a135781c",
            10_364_195,
        )),
        (PIPER_ENGINE, "windows", "x86_64") => Some(github(
            "rhasspy/piper",
            PIPER_TAG,
            "piper_windows_amd64.zip",
            // GitHub publishes no digest for this 2023 release; computed from
            // the official archive on 2026-10-21.
            "f3c58906402b24f3a96d92145f58acba6d86c9b5db896d207f78dc80811efcea",
            22_477_236,
        )),
        (PIPER_ENGINE, "linux", "x86_64") => Some(github(
            "rhasspy/piper",
            PIPER_TAG,
            "piper_linux_x86_64.tar.gz",
            // GitHub publishes no digest for this 2023 release; this one is
            // recorded in the Arch Linux AUR package piper-tts-bin.
            "a50cb45f355b7af1f6d758c1b360717877ba0a398cc8cbe6d2a7a3a26e225992",
            26_460_462,
        )),
        _ => None,
    }
}

fn whisper_models() -> Vec<PackageSpec> {
    let model = |id: &'static str,
                 name: &'static str,
                 description: &'static str,
                 file: &str,
                 sha256: &'static str,
                 size: u64,
                 recommended: bool| PackageSpec {
        id,
        kind: VoicePackageKind::SpeechModel,
        display_name: name,
        description,
        language: None,
        source: format!(
            "huggingface.co/ggerganov/whisper.cpp @ {}",
            &WHISPER_MODELS_REVISION[..12]
        ),
        license: "MIT",
        speakers: &[],
        recommended,
        files: vec![FileSpec {
            url: format!(
                "https://huggingface.co/ggerganov/whisper.cpp/resolve/{WHISPER_MODELS_REVISION}/{file}"
            ),
            name: file.to_owned(),
            sha256,
            size,
        }],
        executable: None,
    };
    vec![
        model(
            "whisper-base-q5_1",
            "Whisper Base",
            "Fast. Good for English, often wrong in Ukrainian.",
            "ggml-base-q5_1.bin",
            "422f1ae452ade6f30a004d7e5c6a43195e4433bc370bf23fac9cc591f01a8898",
            59_707_625,
            false,
        ),
        model(
            "whisper-small-q5_1",
            "Whisper Small",
            "Recommended: understands Ukrainian well and runs on ordinary laptops.",
            "ggml-small-q5_1.bin",
            "ae85e4a935d7a567bd102fe55afc16bb595bdb618e11b2fc7591bc08120411bb",
            190_085_487,
            true,
        ),
        model(
            "whisper-large-v3-turbo-q5_0",
            "Whisper Large v3 Turbo",
            "Most accurate. Needs a fast processor; several seconds per phrase on laptops.",
            "ggml-large-v3-turbo-q5_0.bin",
            "394221709cd5ad1f40c46e6031ca61bce88931e6e088c188294c6d5a55ffa7e2",
            574_041_195,
            false,
        ),
    ]
}

fn voices() -> Vec<PackageSpec> {
    #[allow(clippy::too_many_arguments)]
    fn voice(
        id: &'static str,
        name: &'static str,
        description: &'static str,
        language: &'static str,
        path: &str,
        onnx: (&'static str, u64),
        json: (&'static str, u64),
        license: &'static str,
        speakers: &'static [&'static str],
        recommended: bool,
    ) -> PackageSpec {
        let file = path.rsplit('/').next().unwrap_or(path);
        let url = |name: &str| {
            format!(
                "https://huggingface.co/rhasspy/piper-voices/resolve/{PIPER_VOICES_REVISION}/{path}{name}"
            )
        };
        PackageSpec {
            id,
            kind: VoicePackageKind::Voice,
            display_name: name,
            description,
            language: Some(language),
            source: format!(
                "huggingface.co/rhasspy/piper-voices @ {}",
                &PIPER_VOICES_REVISION[..12]
            ),
            license,
            speakers,
            recommended,
            files: vec![
                FileSpec {
                    url: url(".onnx"),
                    name: format!("{file}.onnx"),
                    sha256: onnx.0,
                    size: onnx.1,
                },
                FileSpec {
                    url: url(".onnx.json"),
                    name: format!("{file}.onnx.json"),
                    sha256: json.0,
                    size: json.1,
                },
            ],
            executable: None,
        }
    }
    vec![
        voice(
            "piper-uk_UA-ukrainian_tts-medium",
            "Українська — Лада, Микита, Тетяна",
            "Three Ukrainian voices in one model, natural quality.",
            "uk",
            "uk/uk_UA/ukrainian_tts/medium/uk_UA-ukrainian_tts-medium",
            (
                "7920419ac5f6fd8b6450520f24b52ed5a319cb53dd018fbcd71c9e079cbac84f",
                76_735_663,
            ),
            (
                "4e96e72917ca9b94edc77d6ccfee03a73f450ba2fc1ca93c2e562bc014e5aa55",
                2_002,
            ),
            "CC0 (dataset)",
            &["Лада", "Микита", "Тетяна"],
            true,
        ),
        voice(
            "piper-uk_UA-lada-x_low",
            "Українська — Лада (компактна)",
            "Small and fast Ukrainian voice with lower quality.",
            "uk",
            "uk/uk_UA/lada/x_low/uk_UA-lada-x_low",
            (
                "f8d015d3fc19ea6bd8ac3ca1fd0c0fbac5054c609599ee73799235fd2cf5c786",
                20_628_813,
            ),
            (
                "2df74b622e78930e8a62432d1cc8380f79f34f02c8c58ebd68e6ccbc857d80aa",
                4_186,
            ),
            "Apache-2.0 (dataset)",
            &[],
            false,
        ),
        voice(
            "piper-en_GB-alan-medium",
            "English (UK) — Alan",
            "British English male voice.",
            "en",
            "en/en_GB/alan/medium/en_GB-alan-medium",
            (
                "0a309668932205e762801f1efc2736cd4b0120329622adf62be09e56339d3330",
                63_201_294,
            ),
            (
                "c0f0d124e5895c00e7c03b35dcc8287f319a6998a365b182deb5c8e752ee8c1e",
                4_888,
            ),
            "See the model card (mimic3-voices dataset)",
            &[],
            true,
        ),
        voice(
            "piper-en_US-amy-medium",
            "English (US) — Amy",
            "American English female voice.",
            "en",
            "en/en_US/amy/medium/en_US-amy-medium",
            (
                "b3a6e47b57b8c7fbe6a0ce2518161a50f59a9cdd8a50835c02cb02bdd6206c18",
                63_201_294,
            ),
            (
                "95a23eb4d42909d38df73bb9ac7f45f597dbfcde2d1bf9526fdeaf5466977d77",
                4_882,
            ),
            "See the model card (mimic3-voices dataset)",
            &[],
            false,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packages_are_pinned_and_unique() {
        let all = packages();
        let mut ids: Vec<_> = all.iter().map(|p| p.id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), all.len());
        for package in &all {
            assert!(!package.files.is_empty(), "{}", package.id);
            for file in &package.files {
                assert_eq!(file.sha256.len(), 64, "{}", file.url);
                assert!(
                    file.sha256.chars().all(|c| c.is_ascii_hexdigit()),
                    "{}",
                    file.url
                );
                assert!(
                    file.url.starts_with("https://github.com/")
                        || file.url.starts_with("https://huggingface.co/")
                );
                assert!(!file.url.contains("/main/"), "{} is not pinned", file.url);
                assert!(file.size > 0);
            }
            assert_eq!(
                package.is_engine(),
                package.executable.is_some(),
                "{}",
                package.id
            );
        }
        if cfg!(all(
            any(windows, target_os = "linux"),
            target_arch = "x86_64"
        )) {
            assert!(unsupported_reason().is_none());
            assert!(find(WHISPER_ENGINE).is_some() && find(PIPER_ENGINE).is_some());
            assert!(
                all.iter()
                    .any(|p| p.kind == VoicePackageKind::Voice && p.language == Some("uk"))
            );
        } else {
            assert!(all.is_empty());
            assert!(unsupported_reason().is_some());
        }
    }
}
