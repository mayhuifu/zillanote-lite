use std::path::{Path, PathBuf};

/// Qwen3-ASR weights served by a local `llama-server`.
#[derive(
    Debug,
    Clone,
    Copy,
    serde::Serialize,
    serde::Deserialize,
    strum::Display,
    strum::EnumString,
    Eq,
    Hash,
    PartialEq,
)]
pub enum Qwen3AsrModel {
    #[serde(rename = "qwen3-asr-1.7b")]
    #[strum(serialize = "qwen3-asr-1.7b")]
    Large,
}

/// One file of a model as published on Hugging Face. The URL is pinned to a revision so
/// the size and checksum next to it stay true.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Qwen3AsrDownload {
    pub file_name: &'static str,
    pub url: &'static str,
    pub size_bytes: u64,
    /// CRC-32 (IEEE) of the whole file.
    pub crc32: u32,
}

// Sizes and checksums were taken from files whose SHA-256 matched the ones Hugging Face
// publishes for this revision.
const LARGE_DOWNLOADS: &[Qwen3AsrDownload] = &[
    Qwen3AsrDownload {
        file_name: "Qwen3-ASR-1.7B-Q8_0.gguf",
        url: "https://huggingface.co/ggml-org/Qwen3-ASR-1.7B-GGUF/resolve/36a678687ba7d07a74ca70ccb0e36902e005fb80/Qwen3-ASR-1.7B-Q8_0.gguf",
        size_bytes: 2_165_034_944,
        crc32: 2_313_806_219,
    },
    Qwen3AsrDownload {
        file_name: "mmproj-Qwen3-ASR-1.7B-bf16.gguf",
        url: "https://huggingface.co/ggml-org/Qwen3-ASR-1.7B-GGUF/resolve/36a678687ba7d07a74ca70ccb0e36902e005fb80/mmproj-Qwen3-ASR-1.7B-bf16.gguf",
        size_bytes: 641_773_984,
        crc32: 3_831_328_114,
    },
];

/// The two GGUF files one model needs: the language model and its audio encoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Qwen3AsrFiles {
    pub model: PathBuf,
    pub mmproj: PathBuf,
}

impl Qwen3AsrModel {
    const ALL: &'static [Self] = &[Self::Large];

    pub const fn all() -> &'static [Self] {
        Self::ALL
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Large => "qwen3-asr-1.7b",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Large => "Qwen3-ASR 1.7B",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Large => {
                "Multilingual batch transcription, strong on mixed English and Mandarin."
            }
        }
    }

    /// Every file the model needs, in download order.
    pub const fn downloads(self) -> &'static [Qwen3AsrDownload] {
        match self {
            Self::Large => LARGE_DOWNLOADS,
        }
    }

    pub fn size_bytes(self) -> u64 {
        self.downloads().iter().map(|file| file.size_bytes).sum()
    }

    // The 1.7B model loses accuracy below Q8_0, so no smaller quantization is offered.
    pub const fn model_file_name(self) -> &'static str {
        match self {
            Self::Large => "Qwen3-ASR-1.7B-Q8_0.gguf",
        }
    }

    pub const fn mmproj_file_name(self) -> &'static str {
        match self {
            Self::Large => "mmproj-Qwen3-ASR-1.7B-bf16.gguf",
        }
    }

    /// Folder name under a Hugging Face style `<publisher>/<repo>` models tree.
    pub const fn repo(self) -> &'static str {
        match self {
            Self::Large => "ggml-org/Qwen3-ASR-1.7B-GGUF",
        }
    }

    pub fn install_dir(self, models_base: &Path) -> PathBuf {
        models_base.join("qwen3-asr").join(self.as_str())
    }

    /// Our own models folder first, then a copy LM Studio already downloaded.
    pub fn locate_files(self, models_base: &Path) -> Option<Qwen3AsrFiles> {
        let mut dirs = vec![self.install_dir(models_base)];
        dirs.extend(
            lm_studio_models_dirs()
                .into_iter()
                .map(|dir| dir.join(self.repo())),
        );
        self.locate_files_in(&dirs)
    }

    pub fn locate_files_in(self, dirs: &[PathBuf]) -> Option<Qwen3AsrFiles> {
        dirs.iter().find_map(|dir| {
            let files = Qwen3AsrFiles {
                model: dir.join(self.model_file_name()),
                mmproj: dir.join(self.mmproj_file_name()),
            };
            (files.model.is_file() && files.mmproj.is_file()).then_some(files)
        })
    }
}

fn lm_studio_models_dirs() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    vec![
        home.join(".lmstudio").join("models"),
        home.join(".cache").join("lm-studio").join("models"),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"gguf").unwrap();
    }

    #[test]
    fn serializes_as_its_model_id() {
        assert_eq!(
            serde_json::to_string(&Qwen3AsrModel::Large).unwrap(),
            "\"qwen3-asr-1.7b\""
        );
        assert_eq!(
            "qwen3-asr-1.7b".parse::<Qwen3AsrModel>().unwrap(),
            Qwen3AsrModel::Large
        );
    }

    #[test]
    fn downloads_are_the_files_the_server_loads_from_a_pinned_revision() {
        for model in Qwen3AsrModel::all() {
            let downloads = model.downloads();
            let names = downloads
                .iter()
                .map(|file| file.file_name)
                .collect::<Vec<_>>();
            assert_eq!(names, [model.model_file_name(), model.mmproj_file_name()]);

            for file in downloads {
                let (repo_url, revision_and_name) = file.url.split_once("/resolve/").unwrap();
                let (revision, name) = revision_and_name.split_once('/').unwrap();

                assert_eq!(repo_url, format!("https://huggingface.co/{}", model.repo()));
                assert_eq!(name, file.file_name);
                // A branch name would let the file change under the checksum.
                assert_eq!(revision.len(), 40);
                assert!(revision.chars().all(|c| c.is_ascii_hexdigit()));
                assert!(file.size_bytes > 0);
            }

            assert_eq!(
                model.size_bytes(),
                downloads.iter().map(|file| file.size_bytes).sum::<u64>()
            );
        }
    }

    #[test]
    fn needs_both_files_in_the_same_folder() {
        let dir = tempfile::tempdir().unwrap();
        let model = Qwen3AsrModel::Large;
        let folder = dir.path().to_path_buf();

        touch(&folder.join(model.model_file_name()));
        assert_eq!(model.locate_files_in(std::slice::from_ref(&folder)), None);

        touch(&folder.join(model.mmproj_file_name()));
        let files = model.locate_files_in(&[folder.clone()]).unwrap();
        assert_eq!(files.model, folder.join(model.model_file_name()));
        assert_eq!(files.mmproj, folder.join(model.mmproj_file_name()));
    }

    #[test]
    fn prefers_the_first_folder_that_has_the_files() {
        let dir = tempfile::tempdir().unwrap();
        let model = Qwen3AsrModel::Large;
        let ours = dir.path().join("ours");
        let lm_studio = dir.path().join("lm-studio");
        for folder in [&ours, &lm_studio] {
            touch(&folder.join(model.model_file_name()));
            touch(&folder.join(model.mmproj_file_name()));
        }

        let files = model.locate_files_in(&[ours.clone(), lm_studio]).unwrap();
        assert!(files.model.starts_with(&ours));
    }
}
