//! The whole product minus the window: record, chunk, transcribe, write minutes, store.

pub mod audio;
pub mod chunker;
pub mod download;
pub mod email;
pub mod minutes;
pub mod mixdown;
pub mod pipeline;
pub mod recorder;
pub mod store;
pub mod system_audio;
pub mod templates;
pub mod transcript;
pub mod turns;
pub mod voices;
