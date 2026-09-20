//! Needs `llama-server` and the Qwen3-ASR GGUF files on this machine:
//!
//! cargo test -p qwen3-asr --test live -- --ignored --nocapture

use qwen3_asr::{LlamaServer, LlamaServerConfig, Qwen3AsrModel, find_llama_server, kill_stale_servers};

fn bundled_dir() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../app/resources/llama-server")
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs llama-server and the Qwen3-ASR GGUF files"]
async fn a_server_left_behind_by_a_crash_is_found_and_killed() {
    let files = Qwen3AsrModel::Large
        .locate_files(&std::env::temp_dir().join("zillanote-no-models"))
        .expect("model files in the LM Studio folder");
    let binary = find_llama_server(&[bundled_dir()]).expect("llama-server");
    let server = LlamaServer::start(LlamaServerConfig::new(binary, files))
        .await
        .expect("server starts");
    let port = server.port();
    // What a crash does: the handle is gone and nothing ever stops the process.
    std::mem::forget(server);

    let killed = tokio::task::spawn_blocking(kill_stale_servers).await.unwrap();

    assert!(killed >= 1, "the orphaned server was not recognised");
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    assert!(
        std::net::TcpStream::connect(("127.0.0.1", port)).is_err(),
        "the server still answers on port {port}"
    );
}
