use std::collections::VecDeque;
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use coducktor_client::Engine;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::time::sleep;

const LOG_CAPACITY: usize = 2_000;

/// Lifecycle state surfaced to the status bar and later toast layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Disabled,
    Starting,
    Adopted,
    Ready,
    Failed,
    Stopped,
}

/// Configuration for a supervised local service.
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    pub command: PathBuf,
    pub args: Vec<String>,
    pub log_path: PathBuf,
    pub health_timeout: Duration,
    pub poll_interval: Duration,
    pub max_restarts: u32,
}

impl ServiceConfig {
    pub fn new(command: impl Into<PathBuf>, log_path: impl Into<PathBuf>) -> Self {
        Self {
            command: command.into(),
            args: Vec::new(),
            log_path: log_path.into(),
            health_timeout: Duration::from_secs(15),
            poll_interval: Duration::from_millis(100),
            max_restarts: 2,
        }
    }
}

/// A bounded in-memory view of the captured child logs.
#[derive(Debug, Clone, Default)]
pub struct LogRing {
    lines: VecDeque<String>,
}

impl LogRing {
    pub fn push(&mut self, line: String) {
        if self.lines.len() == LOG_CAPACITY {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
    }

    pub fn lines(&self) -> Vec<String> {
        self.lines.iter().cloned().collect()
    }
}

/// Domain-shaped supervisor failure; child output remains in `logs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceError {
    Spawn(String),
    Unavailable(String),
    Io(String),
}

impl fmt::Display for ServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn(reason) => write!(formatter, "service spawn failed: {reason}"),
            Self::Unavailable(reason) => write!(formatter, "service unavailable: {reason}"),
            Self::Io(reason) => write!(formatter, "service log failure: {reason}"),
        }
    }
}

impl std::error::Error for ServiceError {}

/// Supervises a child without inheriting either output stream.
///
/// C2 no longer constructs this compatibility type; C3 removes the old service-shaped
/// lifecycle and log overlay entirely. Keeping the generic seam here avoids making that
/// independent cleanup part of the engine switch.
pub struct ServiceSupervisor<E: Engine> {
    config: ServiceConfig,
    engine: E,
    child: Option<Child>,
    logs: Arc<Mutex<LogRing>>,
    state: ServiceState,
}

impl<E: Engine> ServiceSupervisor<E> {
    pub fn new(config: ServiceConfig, engine: E) -> Self {
        Self {
            config,
            engine,
            child: None,
            logs: Arc::new(Mutex::new(LogRing::default())),
            state: ServiceState::Stopped,
        }
    }

    pub fn state(&self) -> ServiceState {
        self.state
    }

    pub fn engine(&self) -> &E {
        &self.engine
    }

    pub fn logs(&self) -> Vec<String> {
        self.logs
            .lock()
            .map(|ring| ring.lines())
            .unwrap_or_default()
    }

    /// Adopt an already-running service, or spawn and health-poll a child.
    pub async fn start(&mut self) -> Result<ServiceState, ServiceError> {
        if self.engine.health().await.is_ok() {
            self.state = ServiceState::Adopted;
            return Ok(self.state);
        }

        self.state = ServiceState::Starting;
        for attempt in 0..=self.config.max_restarts {
            self.spawn_child().await?;
            let deadline = Instant::now() + self.config.health_timeout;
            loop {
                if self.engine.health().await.is_ok() {
                    self.state = ServiceState::Ready;
                    return Ok(self.state);
                }
                if let Some(child) = self.child.as_mut() {
                    match child.try_wait() {
                        Ok(Some(_)) => break,
                        Ok(None) => {}
                        Err(error) => return Err(ServiceError::Io(error.to_string())),
                    }
                }
                if Instant::now() >= deadline {
                    break;
                }
                sleep(self.config.poll_interval).await;
            }
            self.kill_child().await;
            if attempt < self.config.max_restarts {
                self.state = ServiceState::Starting;
            }
        }

        self.state = ServiceState::Failed;
        Err(ServiceError::Unavailable(
            "health check did not become ready".to_owned(),
        ))
    }

    /// Check for a crash and restart it while restart budget remains.
    pub async fn monitor_once(&mut self) -> Result<ServiceState, ServiceError> {
        let Some(child) = self.child.as_mut() else {
            return Ok(self.state);
        };
        if child
            .try_wait()
            .map_err(|error| ServiceError::Io(error.to_string()))?
            .is_none()
        {
            return Ok(self.state);
        }
        self.child = None;
        self.start().await
    }

    /// Stop only a child owned by this supervisor; adopted services are left alone.
    pub async fn shutdown(&mut self) {
        self.kill_child().await;
        self.state = ServiceState::Stopped;
    }

    async fn spawn_child(&mut self) -> Result<(), ServiceError> {
        if let Some(parent) = self.config.log_path.parent() {
            fs::create_dir_all(parent).map_err(|error| ServiceError::Io(error.to_string()))?;
        }
        let mut command = Command::new(&self.config.command);
        command
            .args(&self.config.args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut child = command
            .spawn()
            .map_err(|error| ServiceError::Spawn(error.to_string()))?;
        if let Some(stdout) = child.stdout.take() {
            spawn_capture(
                stdout,
                "stdout",
                self.logs.clone(),
                self.config.log_path.clone(),
            );
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_capture(
                stderr,
                "stderr",
                self.logs.clone(),
                self.config.log_path.clone(),
            );
        }
        self.child = Some(child);
        Ok(())
    }

    async fn kill_child(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill().await;
            let _ = child.wait().await;
        }
    }
}

fn spawn_capture<R>(reader: R, stream_name: &'static str, logs: Arc<Mutex<LogRing>>, path: PathBuf)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(reader).lines();
        let mut file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await
            .ok();
        while let Ok(Some(line)) = lines.next_line().await {
            let line = format!("[{stream_name}] {line}");
            if let Ok(mut ring) = logs.lock() {
                ring.push(line.clone());
            }
            if let Some(log_file) = file.as_mut() {
                let _ = log_file.write_all(format!("{line}\n").as_bytes()).await;
            }
        }
    });
}
