//! Qwen3-ASR through a local `llama-server`: finding the model files, running the server,
//! sending audio, and cleaning up the text that comes back.

mod client;
mod error;
mod model;
mod server;
mod text;

pub use client::{Qwen3AsrClient, SAMPLE_RATE};
pub use error::Error;
pub use model::{Qwen3AsrDownload, Qwen3AsrFiles, Qwen3AsrModel};
pub use server::{
    BINARY_PATH_ENV, LlamaServer, LlamaServerConfig, SERVER_ALIAS, describe_missing_binary,
    describe_missing_model, find_llama_server, kill_stale_servers,
};
pub use text::{TimedWord, strip_asr_prefix, timed_words};
