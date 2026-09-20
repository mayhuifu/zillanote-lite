//! Who spoke when, and whether it is a voice heard before.
//!
//! Sliding-window segmentation (pyannote segmentation-3.0), a speaker embedding per local
//! speaker (WeSpeaker ResNet34), agglomerative clustering, then reconstruction of the turns:
//! the structure of the pyannote.audio 3.1 pipeline, tuned for one pass on the CPU after a
//! recording ends. Voices the user has named steer the clustering and name the result.
//!
//! `clustering`, `matching`, `pipeline` and `segmentation` come from the MIT-licensed
//! community layer of fastrepl/anarlog (`crates/pyannote-local`, `crates/voiceprint`,
//! `crates/embedding`), see `LICENSE-ANARLOG`. The models are loaded from files instead of
//! being compiled in, and the filterbank features are computed here instead of by
//! kaldi-native-fbank.

pub mod clustering;
pub mod embedding;
pub mod fbank;
pub mod matching;
pub mod pipeline;
pub mod segmentation;

mod error;

use std::path::{Path, PathBuf};

pub use clustering::SpeakerBounds;
pub use error::Error;
pub use matching::cosine_similarity;
pub use pipeline::{
    AudioSource, Diarization, DiarizationConfig, DiarizeRequest, DiarizedSpeaker, Diarizer, KnownSpeaker,
    SpeakerIdentity, SpeakerSegment,
};
pub use segmentation::SAMPLE_RATE;

/// The two model files, by the names they are downloaded under.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpeakerModels {
    pub segmentation: PathBuf,
    pub embedding: PathBuf,
}

impl SpeakerModels {
    pub const SEGMENTATION_FILE: &str = "segmentation.onnx";
    pub const EMBEDDING_FILE: &str = "embedding.onnx";

    /// The models in `dir`, if both are there.
    pub fn locate(dir: &Path) -> Option<Self> {
        let models = Self {
            segmentation: dir.join(Self::SEGMENTATION_FILE),
            embedding: dir.join(Self::EMBEDDING_FILE),
        };
        (models.segmentation.is_file() && models.embedding.is_file()).then_some(models)
    }
}

/// A few threads: the work runs in the background next to whatever the user is doing.
const THREADS: usize = 4;

fn load_model(path: &Path) -> Result<ort::session::Session, Error> {
    use ort::session::{Session, builder::GraphOptimizationLevel};

    if !path.is_file() {
        return Err(Error::ModelsMissing(path.display().to_string()));
    }
    Ok(Session::builder()?
        .with_intra_threads(THREADS)?
        .with_optimization_level(GraphOptimizationLevel::Level3)?
        .commit_from_file(path)?)
}
