#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("the speaker models are not in {0}")]
    ModelsMissing(String),
    #[error(transparent)]
    Ort(#[from] ort::Error),
    #[error("the model gave an output of an unexpected shape")]
    UnexpectedOutput,
    #[error("segmentation window must hold {expected} samples, got {actual}")]
    WindowLength { expected: usize, actual: usize },
    #[error("mask length ({mask_len}) must match samples length ({samples_len})")]
    MaskLengthMismatch { mask_len: usize, samples_len: usize },
    #[error("diarization cancelled")]
    Cancelled,
    #[error("audio read failed: {0}")]
    AudioRead(String),
}
