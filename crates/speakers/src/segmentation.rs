use std::path::Path;

use ort::{session::Session, value::TensorRef};

pub const SAMPLE_RATE: u32 = 16_000;
/// The model is trained on 10 s windows and only accepts that length.
pub const WINDOW_SAMPLES: usize = SAMPLE_RATE as usize * 10;
/// Each output frame summarises this many input samples.
pub const FRAME_SIZE: usize = 270;
/// Receptive-field offset of the first output frame.
pub const FRAME_START: usize = 721;
/// The model separates at most this many concurrent speakers inside a window.
pub const LOCAL_SPEAKERS: usize = 3;

// segmentation-3.0 predicts a powerset over three local speakers, ordered by
// cardinality: silence, the three singletons, then the three pairs.
const POWERSET: [[bool; LOCAL_SPEAKERS]; 7] = [
    [false, false, false],
    [true, false, false],
    [false, true, false],
    [false, false, true],
    [true, true, false],
    [true, false, true],
    [false, true, true],
];

/// Per-frame activity of the local speakers inside one 10 s window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowActivity {
    pub frames: Vec<[bool; LOCAL_SPEAKERS]>,
}

impl WindowActivity {
    pub fn active_frames(&self, speaker: usize) -> usize {
        self.frames.iter().filter(|frame| frame[speaker]).count()
    }

    /// Frames where `speaker` talks alone; overlapped frames contaminate
    /// speaker embeddings.
    pub fn clean_frames(&self, speaker: usize) -> usize {
        self.frames
            .iter()
            .filter(|frame| frame[speaker] && frame.iter().filter(|active| **active).count() == 1)
            .count()
    }

    pub fn speech_frames(&self) -> usize {
        self.frames
            .iter()
            .filter(|frame| frame.iter().any(|active| *active))
            .count()
    }
}

pub fn frame_start_sample(frame: usize) -> usize {
    FRAME_START + frame * FRAME_SIZE
}

/// Number of output frames fully covered by `samples` valid input samples.
pub fn frames_for_samples(samples: usize) -> usize {
    samples.saturating_sub(FRAME_START) / FRAME_SIZE
}

pub struct Segmenter {
    session: Session,
}

impl Segmenter {
    pub fn new(model: &Path) -> Result<Self, crate::Error> {
        Ok(Self {
            session: crate::load_model(model)?,
        })
    }

    /// Runs one window. `window` must hold exactly [`WINDOW_SAMPLES`] samples in
    /// `[-1, 1]`; the model normalises the waveform so scale does not matter.
    pub fn run_window(&mut self, window: &[f32]) -> Result<WindowActivity, crate::Error> {
        if window.len() != WINDOW_SAMPLES {
            return Err(crate::Error::WindowLength {
                expected: WINDOW_SAMPLES,
                actual: window.len(),
            });
        }

        let input = TensorRef::from_array_view(([1, 1, WINDOW_SAMPLES], window))?;
        let outputs = self.session.run(ort::inputs![input])?;
        let output = outputs.values().next().ok_or(crate::Error::UnexpectedOutput)?;
        let (shape, scores) = output.try_extract_tensor::<f32>()?;
        // (batch, frames, classes)
        let classes = shape.last().map_or(0, |classes| *classes as usize);
        if classes == 0 {
            return Err(crate::Error::UnexpectedOutput);
        }

        let frames = scores
            .chunks_exact(classes)
            .map(|row| {
                let class = (0..classes).max_by(|a, b| row[*a].total_cmp(&row[*b])).unwrap_or(0);
                POWERSET[class.min(POWERSET.len() - 1)]
            })
            .collect();

        Ok(WindowActivity { frames })
    }
}
