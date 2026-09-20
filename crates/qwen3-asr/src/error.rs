#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("could not prepare audio for Qwen3-ASR: {0}")]
    Audio(String),
    #[error("Qwen3-ASR request failed: {0}")]
    Request(String),
    #[error("Qwen3-ASR server returned {status}: {body}")]
    Server { status: u16, body: String },
    #[error("could not start llama-server: {0}")]
    ServerStart(String),
}
