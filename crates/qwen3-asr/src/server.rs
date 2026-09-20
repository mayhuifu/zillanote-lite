use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};

use crate::{Error, Qwen3AsrFiles};

/// Passed as `--alias` so a server we launched can be told apart from a `llama-server`
/// the user runs themselves; only processes carrying it are ever killed.
pub const SERVER_ALIAS: &str = "zillanote-lite-qwen3-asr";
pub const BINARY_PATH_ENV: &str = "ZILLANOTE_LLAMA_SERVER";

const BINARY_NAME: &str = if cfg!(windows) {
    "llama-server.exe"
} else {
    "llama-server"
};
/// Left to its defaults the server reserves the model's whole context window for several
/// slots plus an 8 GB prompt cache: about 9 GB of memory. We send one chunk of at most
/// 25 s at a time and never repeat a prompt, so a single small slot with no prompt cache
/// does the same work in about 3 GB. A 25 s chunk uses a few hundred tokens.
const MEMORY_LIMIT_ARGS: [&str; 6] = ["--ctx-size", "4096", "--parallel", "1", "--cache-ram", "0"];
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(500);
const DEFAULT_STARTUP_TIMEOUT: Duration = Duration::from_secs(180);
const KEPT_LOG_LINES: usize = 20;

#[derive(Debug, Clone)]
pub struct LlamaServerConfig {
    pub binary: PathBuf,
    pub files: Qwen3AsrFiles,
    pub startup_timeout: Duration,
}

impl LlamaServerConfig {
    pub fn new(binary: PathBuf, files: Qwen3AsrFiles) -> Self {
        Self {
            binary,
            files,
            startup_timeout: DEFAULT_STARTUP_TIMEOUT,
        }
    }
}

/// A running `llama-server` that is killed when this value is dropped.
#[derive(Debug)]
pub struct LlamaServer {
    child: Child,
    port: u16,
}

impl LlamaServer {
    pub async fn start(config: LlamaServerConfig) -> Result<Self, Error> {
        let port = free_loopback_port()?;
        let mut child = Command::new(&config.binary)
            .arg("--model")
            .arg(&config.files.model)
            .arg("--mmproj")
            .arg(&config.files.mmproj)
            .args(["--host", "127.0.0.1"])
            .args(["--port", &port.to_string()])
            .args(["--alias", SERVER_ALIAS])
            .args(MEMORY_LIMIT_ARGS)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| Error::ServerStart(format!("{}: {e}", config.binary.display())))?;

        let recent_log = Arc::new(Mutex::new(VecDeque::with_capacity(KEPT_LOG_LINES)));
        if let Some(stdout) = child.stdout.take() {
            drain_log(stdout, recent_log.clone());
        }
        if let Some(stderr) = child.stderr.take() {
            drain_log(stderr, recent_log.clone());
        }

        let mut server = Self { child, port };
        match server.wait_until_healthy(config.startup_timeout).await {
            Ok(()) => Ok(server),
            Err(error) => {
                server.stop().await;
                let log = recent_log
                    .lock()
                    .map(|lines| lines.iter().cloned().collect::<Vec<_>>().join("\n"))
                    .unwrap_or_default();
                Err(match error {
                    Error::ServerStart(message) if !log.is_empty() => {
                        Error::ServerStart(format!("{message}\n{log}"))
                    }
                    other => other,
                })
            }
        }
    }

    pub fn port(&self) -> u16 {
        self.port
    }

    /// OpenAI-style root, the form `Qwen3AsrClient::new` expects.
    pub fn base_url(&self) -> String {
        format!("http://127.0.0.1:{}/v1", self.port)
    }

    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    pub async fn stop(&mut self) {
        let _ = self.child.kill().await;
    }

    async fn wait_until_healthy(&mut self, timeout: Duration) -> Result<(), Error> {
        let http = reqwest::Client::builder()
            .no_proxy()
            .timeout(Duration::from_secs(2))
            .build()
            .map_err(|e| Error::ServerStart(e.to_string()))?;
        let health_url = format!("http://127.0.0.1:{}/health", self.port);
        let deadline = Instant::now() + timeout;

        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                return Err(Error::ServerStart(format!(
                    "llama-server exited during startup ({status})"
                )));
            }
            // It answers 503 while the model is still loading.
            if let Ok(response) = http.get(&health_url).send().await
                && response.status().is_success()
            {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(Error::ServerStart(format!(
                    "llama-server was not ready after {} seconds",
                    timeout.as_secs()
                )));
            }
            tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
        }
    }
}

fn drain_log<R>(reader: R, recent: Arc<Mutex<VecDeque<String>>>)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        while let Ok(Some(line)) = lines.next_line().await {
            tracing::debug!(target: "llama_server", "{line}");
            if let Ok(mut recent) = recent.lock() {
                if recent.len() == KEPT_LOG_LINES {
                    recent.pop_front();
                }
                recent.push_back(line);
            }
        }
    });
}

fn free_loopback_port() -> Result<u16, Error> {
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .map_err(|e| Error::ServerStart(format!("no free local port: {e}")))?;
    listener
        .local_addr()
        .map(|addr| addr.port())
        .map_err(|e| Error::ServerStart(e.to_string()))
}

/// Finds `llama-server`: an explicit override, then folders the app ships binaries in,
/// then `PATH`, then the usual install locations (apps started from Finder do not get the
/// shell's `PATH`).
pub fn find_llama_server(bundled_dirs: &[PathBuf]) -> Option<PathBuf> {
    let override_path = std::env::var_os(BINARY_PATH_ENV).map(PathBuf::from);
    let path_dirs = std::env::var_os("PATH")
        .map(|path| std::env::split_paths(&path).collect::<Vec<_>>())
        .unwrap_or_default();
    find_llama_server_in(
        override_path,
        bundled_dirs,
        &path_dirs,
        &common_install_dirs(),
    )
}

fn find_llama_server_in(
    override_path: Option<PathBuf>,
    bundled_dirs: &[PathBuf],
    path_dirs: &[PathBuf],
    common_dirs: &[PathBuf],
) -> Option<PathBuf> {
    if let Some(path) = override_path {
        return path.is_file().then_some(path);
    }

    bundled_dirs
        .iter()
        .chain(path_dirs)
        .chain(common_dirs)
        .map(|dir| dir.join(BINARY_NAME))
        .find(|candidate| candidate.is_file())
}

fn common_install_dirs() -> Vec<PathBuf> {
    if cfg!(windows) {
        return Vec::new();
    }
    ["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"]
        .iter()
        .map(PathBuf::from)
        .collect()
}

/// Kills servers left behind by a run of the app that could not clean up, and returns how
/// many were killed. A `llama-server` the user started themselves is never touched.
pub fn kill_stale_servers() -> usize {
    let mut system = sysinfo::System::new();
    // The default refresh leaves command lines out, and the alias is only found there.
    system.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::All,
        true,
        sysinfo::ProcessRefreshKind::nothing().with_cmd(sysinfo::UpdateKind::Always),
    );

    system
        .processes()
        .values()
        .filter(|process| is_our_server(process.name().to_string_lossy().as_ref(), process.cmd()))
        .filter(|process| process.kill())
        .count()
}

fn is_our_server(name: &str, cmd: &[std::ffi::OsString]) -> bool {
    name.contains("llama-server")
        && cmd
            .windows(2)
            .any(|pair| pair[0] == "--alias" && pair[1] == SERVER_ALIAS)
}

pub fn describe_missing_binary() -> String {
    format!(
        "llama-server was not found. Install llama.cpp (for example `brew install llama.cpp`) \
         or set {BINARY_PATH_ENV} to the llama-server binary."
    )
}

pub fn describe_missing_model(models_dir: &Path, repo: &str) -> String {
    format!(
        "Qwen3-ASR model files were not found. Download {repo} in LM Studio, or place the \
         model and mmproj GGUF files in {}.",
        models_dir.display()
    )
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::*;

    fn touch(path: &Path) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, b"").unwrap();
    }

    #[test]
    fn override_wins_and_a_wrong_override_is_not_silently_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let bundled = dir.path().join("bundled");
        touch(&bundled.join(BINARY_NAME));
        let custom = dir.path().join("custom").join("my-llama");
        touch(&custom);

        assert_eq!(
            find_llama_server_in(Some(custom.clone()), &[bundled.clone()], &[], &[]),
            Some(custom)
        );
        assert_eq!(
            find_llama_server_in(Some(dir.path().join("missing")), &[bundled], &[], &[]),
            None
        );
    }

    #[test]
    fn bundled_binary_is_preferred_over_path_and_common_folders() {
        let dir = tempfile::tempdir().unwrap();
        let bundled = dir.path().join("bundled");
        let on_path = dir.path().join("path");
        let common = dir.path().join("common");
        for folder in [&on_path, &common] {
            touch(&folder.join(BINARY_NAME));
        }

        assert_eq!(
            find_llama_server_in(
                None,
                &[bundled.clone()],
                &[on_path.clone()],
                &[common.clone()]
            ),
            Some(on_path.join(BINARY_NAME))
        );

        touch(&bundled.join(BINARY_NAME));
        assert_eq!(
            find_llama_server_in(None, &[bundled.clone()], &[on_path], &[common]),
            Some(bundled.join(BINARY_NAME))
        );
    }

    #[test]
    fn only_servers_we_launched_count_as_ours() {
        let ours = ["llama-server", "--port", "8123", "--alias", SERVER_ALIAS]
            .map(OsString::from)
            .to_vec();
        let users = ["llama-server", "--port", "8080", "--alias", "my-model"]
            .map(OsString::from)
            .to_vec();

        assert!(is_our_server("llama-server", &ours));
        assert!(!is_our_server("llama-server", &users));
        assert!(!is_our_server(
            "llama-server",
            &[OsString::from("llama-server")]
        ));
        assert!(!is_our_server("python", &ours));
    }

    #[tokio::test]
    async fn a_binary_that_exits_early_is_reported_with_its_output() {
        if cfg!(windows) {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("llama-server");
        std::fs::write(
            &script,
            "#!/bin/sh\necho 'failed to load model' >&2\nexit 3\n",
        )
        .unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }

        let error = LlamaServer::start(LlamaServerConfig {
            binary: script,
            files: Qwen3AsrFiles {
                model: dir.path().join("model.gguf"),
                mmproj: dir.path().join("mmproj.gguf"),
            },
            startup_timeout: Duration::from_secs(10),
        })
        .await
        .unwrap_err();

        let message = error.to_string();
        assert!(message.contains("exited during startup"), "{message}");
        assert!(message.contains("failed to load model"), "{message}");
    }
}
