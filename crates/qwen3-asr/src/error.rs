#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not prepare audio for Qwen3-ASR: {0}")]
    Audio(String),
    #[error("the speech recognizer could not be reached: {0}")]
    Request(String),
    #[error("the speech recognizer answered {status}: {body}")]
    Server { status: u16, body: String },
    #[error("could not start llama-server: {0}")]
    ServerStart(String),
}

impl Error {
    /// Worth another try a little later: no answer, too many requests, or a server trouble.
    /// A refused key or an unknown model is not.
    pub fn is_passing(&self) -> bool {
        match self {
            Self::Request(_) => true,
            Self::Server { status, .. } => *status == 408 || *status == 429 || *status >= 500,
            Self::Audio(_) | Self::ServerStart(_) => false,
        }
    }
}
