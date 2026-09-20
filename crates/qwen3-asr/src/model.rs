use std::path::{Path, PathBuf};

/// Qwen3-ASR weights served by a local `llama-server`: two sizes, each at two precisions.
/// The big one at 8 bits is what the app was built and tested with; the others trade some
/// accuracy for a smaller download and less memory.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    serde::Serialize,
    serde::Deserialize,
    strum::Display,
    strum::EnumString,
    Eq,
    Hash,
    PartialEq,
)]
pub enum Qwen3AsrModel {
    #[default]
    #[serde(rename = "qwen3-asr-1.7b")]
    #[strum(serialize = "qwen3-asr-1.7b")]
    Large,
    #[serde(rename = "qwen3-asr-1.7b-q4")]
    #[strum(serialize = "qwen3-asr-1.7b-q4")]
    LargeQ4,
    #[serde(rename = "qwen3-asr-0.6b")]
    #[strum(serialize = "qwen3-asr-0.6b")]
    Small,
    #[serde(rename = "qwen3-asr-0.6b-q4")]
    #[strum(serialize = "qwen3-asr-0.6b-q4")]
    SmallQ4,
}

/// One file of a model as published on Hugging Face. The URL is pinned to a revision so
/// the size and checksum next to it stay true.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Qwen3AsrDownload {
    pub file_name: &'static str,
    pub url: &'static str,
    pub size_bytes: u64,
    /// SHA-256 of the whole file, as Hugging Face publishes it for this revision. A mirror
    /// may serve the file; nothing but these bytes is accepted from it.
    pub sha256: &'static str,
}

const LARGE_Q8: Qwen3AsrDownload = Qwen3AsrDownload {
    file_name: "Qwen3-ASR-1.7B-Q8_0.gguf",
    url: "https://huggingface.co/ggml-org/Qwen3-ASR-1.7B-GGUF/resolve/36a678687ba7d07a74ca70ccb0e36902e005fb80/Qwen3-ASR-1.7B-Q8_0.gguf",
    size_bytes: 2_165_034_944,
    sha256: "58e22d0532d4eacaf034cfac17a6fed159f37c41390c710186783be439d1fc57",
};
const LARGE_Q4: Qwen3AsrDownload = Qwen3AsrDownload {
    file_name: "Qwen3-ASR-1.7B.Q4_K_M.gguf",
    url: "https://huggingface.co/mradermacher/Qwen3-ASR-1.7B-GGUF/resolve/cc946c78d3804752f7ba1bc42720c0f7aaf3d1ad/Qwen3-ASR-1.7B.Q4_K_M.gguf",
    size_bytes: 1_282_435_552,
    sha256: "3893b8926065bbff3da7586d21d8711a9b4fa4fa8f12cd0cefad58e31b2660b6",
};
const LARGE_MMPROJ: Qwen3AsrDownload = Qwen3AsrDownload {
    file_name: "mmproj-Qwen3-ASR-1.7B-bf16.gguf",
    url: "https://huggingface.co/ggml-org/Qwen3-ASR-1.7B-GGUF/resolve/36a678687ba7d07a74ca70ccb0e36902e005fb80/mmproj-Qwen3-ASR-1.7B-bf16.gguf",
    size_bytes: 641_773_984,
    sha256: "8882e9ddab3186f9aa71b1417c847177913e1466655ac944cf86e9b846735d62",
};
const SMALL_Q8: Qwen3AsrDownload = Qwen3AsrDownload {
    file_name: "Qwen3-ASR-0.6B-Q8_0.gguf",
    url: "https://huggingface.co/ggml-org/Qwen3-ASR-0.6B-GGUF/resolve/928ab958557df9aa2ef1c93e0e83c7ad0933fae2/Qwen3-ASR-0.6B-Q8_0.gguf",
    size_bytes: 804_749_248,
    sha256: "bca259818b50ca7c4c05e9bdb35a5dc04fa039653a6d6f3f0f331f96f6aa1971",
};
const SMALL_Q4: Qwen3AsrDownload = Qwen3AsrDownload {
    file_name: "Qwen3-ASR-0.6B.Q4_K_M.gguf",
    url: "https://huggingface.co/mradermacher/Qwen3-ASR-0.6B-GGUF/resolve/6cf166e9f1c5bd2521108e3410ee5358de5eabab/Qwen3-ASR-0.6B.Q4_K_M.gguf",
    size_bytes: 484_216_288,
    sha256: "40d27969c614b4492f330baa41fcc8d00e25264aebbe3f16eaf5e4bd5af35cd5",
};
const SMALL_MMPROJ: Qwen3AsrDownload = Qwen3AsrDownload {
    file_name: "mmproj-Qwen3-ASR-0.6B-bf16.gguf",
    url: "https://huggingface.co/ggml-org/Qwen3-ASR-0.6B-GGUF/resolve/928ab958557df9aa2ef1c93e0e83c7ad0933fae2/mmproj-Qwen3-ASR-0.6B-bf16.gguf",
    size_bytes: 378_575_520,
    sha256: "dae36c855f9a82a8916bea2238b24bda69a39d8da8b2f46dee7c103775656039",
};

/// The two GGUF files one model needs: the language model and its audio encoder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Qwen3AsrFiles {
    pub model: PathBuf,
    pub mmproj: PathBuf,
}

impl Qwen3AsrModel {
    const ALL: &'static [Self] = &[Self::Large, Self::LargeQ4, Self::Small, Self::SmallQ4];

    /// Best first.
    pub const fn all() -> &'static [Self] {
        Self::ALL
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Large => "qwen3-asr-1.7b",
            Self::LargeQ4 => "qwen3-asr-1.7b-q4",
            Self::Small => "qwen3-asr-0.6b",
            Self::SmallQ4 => "qwen3-asr-0.6b-q4",
        }
    }

    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Large => "Qwen3-ASR 1.7B, 8-bit",
            Self::LargeQ4 => "Qwen3-ASR 1.7B, 4-bit",
            Self::Small => "Qwen3-ASR 0.6B, 8-bit",
            Self::SmallQ4 => "Qwen3-ASR 0.6B, 4-bit",
        }
    }

    pub const fn description(self) -> &'static str {
        match self {
            Self::Large => "The most accurate, and the one to take for meetings that mix English and Mandarin.",
            Self::LargeQ4 => "The big model in two thirds of the space. In our test its transcripts matched the 8-bit model's to 98%, mixed English and Mandarin included.",
            Self::Small => "Less than half the download and half the memory, and twice as fast. As good on clear English (98%), weaker on a meeting that mixes English and Mandarin (93%).",
            Self::SmallQ4 => "The smallest: for a Mac that is short of memory, or a slow line. 98% on clear English, 92% on a mixed meeting.",
        }
    }

    /// The language model and its audio encoder, in download order.
    pub const fn downloads(self) -> [Qwen3AsrDownload; 2] {
        match self {
            Self::Large => [LARGE_Q8, LARGE_MMPROJ],
            Self::LargeQ4 => [LARGE_Q4, LARGE_MMPROJ],
            Self::Small => [SMALL_Q8, SMALL_MMPROJ],
            Self::SmallQ4 => [SMALL_Q4, SMALL_MMPROJ],
        }
    }

    pub fn size_bytes(self) -> u64 {
        self.downloads().iter().map(|file| file.size_bytes).sum()
    }

    /// Memory `llama-server` holds at its peak while it transcribes with this model (4096
    /// tokens of context, one slot), measured on an M4 Pro over a three-minute stretch of a
    /// meeting. The Mac needs this much to spare, for that time only.
    pub const fn memory_bytes(self) -> u64 {
        match self {
            Self::Large => 3_500_000_000,
            Self::LargeQ4 => 2_600_000_000,
            Self::Small => 1_850_000_000,
            Self::SmallQ4 => 1_550_000_000,
        }
    }

    /// Both precisions of a size share a folder, and the audio encoder in it.
    pub fn install_dir(self, models_base: &Path) -> PathBuf {
        let family = match self {
            Self::Large | Self::LargeQ4 => "qwen3-asr-1.7b",
            Self::Small | Self::SmallQ4 => "qwen3-asr-0.6b",
        };
        models_base.join("qwen3-asr").join(family)
    }

    /// Where a file of this model may already be: our own folder first, then the folders
    /// LM Studio keeps the same files in, under their publishers' names.
    fn search_dirs(self, models_base: &Path) -> Vec<PathBuf> {
        let size = match self {
            Self::Large | Self::LargeQ4 => "Qwen3-ASR-1.7B-GGUF",
            Self::Small | Self::SmallQ4 => "Qwen3-ASR-0.6B-GGUF",
        };
        let mut dirs = vec![self.install_dir(models_base)];
        for lm_studio in lm_studio_models_dirs() {
            dirs.push(lm_studio.join("ggml-org").join(size));
            dirs.push(lm_studio.join("mradermacher").join(size));
        }
        dirs
    }

    pub fn locate_files(self, models_base: &Path) -> Option<Qwen3AsrFiles> {
        self.locate_files_in(&self.search_dirs(models_base))
    }

    /// The two files need not be in the same folder: the audio encoder LM Studio downloaded
    /// serves a model file of ours just as well.
    pub fn locate_files_in(self, dirs: &[PathBuf]) -> Option<Qwen3AsrFiles> {
        let find = |name: &str| dirs.iter().map(|dir| dir.join(name)).find(|path| path.is_file());
        let [model, mmproj] = self.downloads();
        Some(Qwen3AsrFiles {
            model: find(model.file_name)?,
            mmproj: find(mmproj.file_name)?,
        })
    }

    /// What still has to be fetched for this model.
    pub fn missing_downloads(self, models_base: &Path) -> Vec<Qwen3AsrDownload> {
        let dirs = self.search_dirs(models_base);
        let downloads = self.downloads().into_iter();
        downloads.filter(|file| !dirs.iter().any(|dir| dir.join(file.file_name).is_file())).collect()
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
    fn serializes_as_its_model_id_and_the_big_one_is_the_default() {
        for model in Qwen3AsrModel::all() {
            assert_eq!(serde_json::to_string(model).unwrap(), format!("\"{}\"", model.as_str()));
            assert_eq!(model.to_string().parse::<Qwen3AsrModel>().unwrap(), *model);
        }
        assert_eq!(Qwen3AsrModel::default(), Qwen3AsrModel::Large);
        assert_eq!(Qwen3AsrModel::Large.as_str(), "qwen3-asr-1.7b", "transcripts and settings carry this id");
    }

    #[test]
    fn every_file_is_pinned_to_a_revision_with_its_size_and_checksum() {
        for model in Qwen3AsrModel::all() {
            for file in model.downloads() {
                let revision = file.url.split("/resolve/").nth(1).unwrap().split('/').next().unwrap();
                assert_eq!(revision.len(), 40, "{}", file.url);
                assert!(file.url.ends_with(file.file_name) && file.url.starts_with("https://huggingface.co/"));
                assert!(file.size_bytes > 100_000_000);
                assert!(file.sha256.len() == 64 && file.sha256.chars().all(|c| c.is_ascii_hexdigit()));
            }
        }
    }

    #[test]
    fn smaller_choices_are_smaller_in_every_way() {
        let models = Qwen3AsrModel::all();
        assert!(models.windows(2).all(|pair| pair[0].size_bytes() > pair[1].size_bytes()));
        assert!(models.windows(2).all(|pair| pair[0].memory_bytes() > pair[1].memory_bytes()));
    }

    #[test]
    fn both_precisions_of_a_size_share_the_audio_encoder() {
        let dir = tempfile::tempdir().unwrap();
        let folder = Qwen3AsrModel::LargeQ4.install_dir(dir.path());
        assert_eq!(folder, Qwen3AsrModel::Large.install_dir(dir.path()));
        let [model, mmproj] = Qwen3AsrModel::LargeQ4.downloads();
        let dirs = [folder.clone()];

        touch(&folder.join(mmproj.file_name));
        assert_eq!(Qwen3AsrModel::LargeQ4.locate_files_in(&dirs), None);
        touch(&folder.join(model.file_name));

        let files = Qwen3AsrModel::LargeQ4.locate_files_in(&dirs).unwrap();
        assert_eq!((files.model, files.mmproj), (folder.join(model.file_name), folder.join(mmproj.file_name)));
        assert_eq!(Qwen3AsrModel::Large.locate_files_in(&dirs), None, "its own model file is not there");
    }

    #[test]
    fn a_file_found_anywhere_is_not_asked_for_again() {
        let dir = tempfile::tempdir().unwrap();
        let (ours, lm_studio) = (dir.path().join("ours"), dir.path().join("lm-studio"));
        let [model, mmproj] = Qwen3AsrModel::Small.downloads();
        touch(&lm_studio.join(mmproj.file_name));
        touch(&ours.join(model.file_name));

        let files = Qwen3AsrModel::Small.locate_files_in(&[ours.clone(), lm_studio.clone()]).unwrap();

        assert_eq!((files.model, files.mmproj), (ours.join(model.file_name), lm_studio.join(mmproj.file_name)));
    }
}
