//! A voice as 256 numbers: the WeSpeaker ResNet34 model on Kaldi filterbank features.

use std::path::Path;

use ort::{session::Session, value::TensorRef};

use crate::fbank::{Fbank, NUM_BINS};

pub const EMBEDDING_DIM: usize = 256;

const INPUT_NAME: &str = "feats";
const OUTPUT_NAME: &str = "embs";
/// A mask value above this keeps the frame.
const MASK_THRESHOLD: f32 = 0.5;

pub struct EmbeddingExtractor {
    session: Session,
    fbank: Fbank,
}

impl EmbeddingExtractor {
    pub fn new(model: &Path) -> Result<Self, crate::Error> {
        Ok(Self {
            session: crate::load_model(model)?,
            fbank: Fbank::new(),
        })
    }

    /// The embedding of all of `samples`, or nothing when they are too short for one.
    pub fn compute(&mut self, samples: &[f32]) -> Result<Option<Vec<f32>>, crate::Error> {
        let features = self.fbank.compute(samples);
        self.run(features)
    }

    /// The embedding of the parts of `samples` where `mask` is set, or nothing when too
    /// little is left. `mask` has one value per sample.
    pub fn compute_with_mask_optional(
        &mut self,
        samples: &[f32],
        mask: &[f32],
    ) -> Result<Option<Vec<f32>>, crate::Error> {
        if samples.len() != mask.len() {
            return Err(crate::Error::MaskLengthMismatch {
                mask_len: mask.len(),
                samples_len: samples.len(),
            });
        }

        let features = self.fbank.compute(samples);
        let frames = features.len() / NUM_BINS;
        let mut kept = Vec::new();
        for frame in 0..frames {
            if mask[frame * mask.len() / frames] > MASK_THRESHOLD {
                kept.extend_from_slice(&features[frame * NUM_BINS..(frame + 1) * NUM_BINS]);
            }
        }
        self.run(kept)
    }

    fn run(&mut self, features: Vec<f32>) -> Result<Option<Vec<f32>>, crate::Error> {
        let frames = features.len() / NUM_BINS;
        if frames == 0 {
            return Ok(None);
        }

        let input = TensorRef::from_array_view(([1, frames, NUM_BINS], features.as_slice()))?;
        let outputs = self.session.run(ort::inputs![INPUT_NAME => input])?;
        let (_, embedding) = outputs
            .get(OUTPUT_NAME)
            .ok_or(crate::Error::UnexpectedOutput)?
            .try_extract_tensor::<f32>()?;

        let finite = embedding.len() == EMBEDDING_DIM && embedding.iter().all(|value| value.is_finite());
        Ok(finite.then(|| embedding.to_vec()))
    }
}
