//! Shared subprocess plumbing for every agent-CLI backend: spawn with the curated child env
//! (`agent_env`), a live stdout-line channel so a turn-scoped caller can enforce a wall-clock
//! deadline without blocking forever, stderr collection, and the SIGTERM->SIGKILL escalation
//! both the post-`finish()` EOF watchdog and a mid-turn timeout need.
//!
//! `claude-cli-runner.ts` and `codex-app-server-transport.ts` each re-derive this plumbing
//! independently in TS; the claude backend (B9a.2b) first wrote it inline, and this module is
//! that code pulled out once codex needed the identical shape a second time — proven duplication,
//! not a speculative abstraction. Protocol semantics (what to write, how to interpret a line)
//! stay in each backend; this module only owns the process itself.

use std::collections::BTreeMap;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use coducktor_contract::Runner;

use crate::agent_env::{self, BuildChildEnvOptions};

#[derive(Debug, Clone)]
pub struct SpawnConfig {
    pub program: String,
    pub args: Vec<String>,
    /// Grace period after `finish()` closes stdin before escalating to SIGTERM.
    pub eof_term_grace: Duration,
    /// Grace period after that SIGTERM before escalating to SIGKILL.
    pub eof_kill_grace: Duration,
    /// Grace period after a wall-clock timeout's SIGTERM before escalating to SIGKILL.
    pub kill_grace: Duration,
}

/// A spawned agent-CLI child process: piped stdin, a background-thread-fed stdout line channel,
/// and background-collected stderr.
pub struct ChildProcess {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout_rx: Receiver<String>,
    stderr_handle: Option<JoinHandle<String>>,
    eof_term_grace: Duration,
    eof_kill_grace: Duration,
    kill_grace: Duration,
}

pub enum NextLine {
    Line(String),
    /// stdout closed — the process exited (or crashed).
    Closed,
}

/// A wall-clock deadline elapsed while waiting for the next line. The caller decides how to
/// escalate (`escalate_after_timeout`) and what message to report — wording differs per backend
/// ("claude CLI timed out…" vs "codex app-server timed out…").
pub struct TimedOut;

impl ChildProcess {
    pub fn spawn(
        config: &SpawnConfig,
        backend: Runner,
        cwd: &Path,
        extra_env: &BTreeMap<String, String>,
        host_env: &BTreeMap<String, String>,
    ) -> io::Result<Self> {
        let child_env = agent_env::build_child_env(BuildChildEnvOptions {
            backend,
            extra_env,
            source: host_env,
        });
        let mut command = Command::new(&config.program);
        command
            .args(&config.args)
            .current_dir(cwd)
            .env_clear()
            .envs(child_env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn()?;
        let stdin = child.stdin.take().expect("stdin was piped");
        let stdout = child.stdout.take().expect("stdout was piped");
        let stderr = child.stderr.take().expect("stderr was piped");

        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let reader = BufReader::new(stdout);
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        let stderr_handle = thread::spawn(move || {
            let mut buffer = String::new();
            let mut stderr = stderr;
            let _ = stderr.read_to_string(&mut buffer);
            buffer
        });

        Ok(Self {
            child,
            stdin: Some(stdin),
            stdout_rx: rx,
            stderr_handle: Some(stderr_handle),
            eof_term_grace: config.eof_term_grace,
            eof_kill_grace: config.eof_kill_grace,
            kill_grace: config.kill_grace,
        })
    }

    pub fn write_line(&mut self, line: &str) -> Result<(), String> {
        let Some(stdin) = self.stdin.as_mut() else {
            return Err("session is closed".to_owned());
        };
        let mut out = line.to_owned();
        out.push('\n');
        stdin
            .write_all(out.as_bytes())
            .map_err(|error| format!("stdin write failed: {error}"))
    }

    /// Drop the stdin handle, delivering EOF to the child.
    pub fn close_stdin(&mut self) {
        self.stdin = None;
    }

    /// Stop caring about further stdout content — for a backend (opencode) that only needs
    /// stdout briefly at startup (to read back a bound URL) and communicates over some other
    /// channel afterward. Moves the live channel to a background thread that keeps draining it
    /// (discarding each line) so neither the channel nor the underlying OS pipe backs up over a
    /// long session; `self`'s own receiver is replaced with an already-disconnected one, so a
    /// stray later call to `next_line` returns `Closed` rather than reading stale/interleaved
    /// output.
    pub fn discard_stdout(&mut self) {
        let rx = std::mem::replace(&mut self.stdout_rx, mpsc::channel().1);
        thread::spawn(move || while rx.recv().is_ok() {});
    }

    /// Block for the next stdout line, honoring an optional deadline. `Ok(NextLine::Closed)`
    /// means the process's stdout has closed (it exited or crashed) — not a timeout.
    pub fn next_line(&mut self, deadline: Option<Instant>) -> Result<NextLine, TimedOut> {
        loop {
            match deadline {
                Some(dl) => {
                    let now = Instant::now();
                    if now >= dl {
                        return Err(TimedOut);
                    }
                    match self.stdout_rx.recv_timeout(dl - now) {
                        Ok(line) => return Ok(NextLine::Line(line)),
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => return Ok(NextLine::Closed),
                    }
                }
                None => {
                    return Ok(match self.stdout_rx.recv() {
                        Ok(line) => NextLine::Line(line),
                        Err(_) => NextLine::Closed,
                    });
                }
            }
        }
    }

    pub fn has_exited(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(Some(_)))
    }

    /// Send a graceful stop signal. On Unix this is a real SIGTERM (the CLI installs its own
    /// handler and can act on it); `std::process::Child::kill` has no SIGTERM concept off Unix,
    /// so non-Unix targets fall back to the same hard kill `signal_kill` uses — there is no
    /// softer option there.
    pub fn signal_term(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM);
        }
        #[cfg(not(unix))]
        {
            let _ = self.child.kill();
        }
    }

    pub fn signal_kill(&mut self) {
        let _ = self.child.kill();
    }

    /// Poll `try_wait` for up to `budget`, sleeping briefly between checks. Returns whether the
    /// child had exited by the time the budget elapsed.
    pub fn wait_exited_within(&mut self, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        loop {
            if self.has_exited() {
                return true;
            }
            if Instant::now() >= deadline {
                return self.has_exited();
            }
            thread::sleep(
                Duration::from_millis(20).min(deadline.saturating_duration_since(Instant::now())),
            );
        }
    }

    pub fn wait_for_exit(&mut self) -> Option<i32> {
        self.child.wait().ok().and_then(|status| status.code())
    }

    /// The last (at most) three non-empty stderr lines, joined for an error message's detail
    /// suffix. Blocks briefly on the stderr-collector thread if it hasn't finished yet — safe to
    /// call once the child has already exited (its stderr pipe is then closed too).
    pub fn take_stderr_tail(&mut self) -> String {
        let Some(handle) = self.stderr_handle.take() else {
            return String::new();
        };
        let raw = handle.join().unwrap_or_default();
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return String::new();
        }
        let lines: Vec<&str> = trimmed.lines().collect();
        lines[lines.len().saturating_sub(3)..].join(" | ")
    }

    /// The EOF SIGTERM->SIGKILL watchdog a backend's `finish()` arms after closing stdin.
    pub fn escalate_after_eof(&mut self) {
        if self.wait_exited_within(self.eof_term_grace) {
            return;
        }
        self.signal_term();
        if self.wait_exited_within(self.eof_kill_grace) {
            return;
        }
        self.signal_kill();
    }

    /// A stop sequence with no earlier EOF opportunity to wait out first: signal SIGTERM right
    /// away, wait `grace`, then escalate to SIGKILL if still alive. For a backend whose process
    /// reads nothing that would make it exit gracefully on its own — an HTTP server with no
    /// stdin protocol (opencode's `finish()`), unlike claude/codex where closing stdin itself is
    /// a signal worth waiting on first.
    pub fn escalate_immediately(&mut self, grace: Duration) {
        self.signal_term();
        if !self.wait_exited_within(grace) {
            self.signal_kill();
        }
    }

    /// The wall-clock kill switch a live turn's read loop arms when its deadline elapses.
    /// Leaves the child reaped (`wait_for_exit` already called) before returning.
    pub fn escalate_after_timeout(&mut self) {
        if !self.has_exited() {
            self.signal_term();
        }
        if !self.wait_exited_within(self.kill_grace) {
            self.signal_kill();
        }
        self.wait_for_exit();
    }
}

impl Drop for ChildProcess {
    /// A best-effort safety net, not a substitute for a backend's own `finish()`/`cancel()`: if
    /// this value is dropped while the child is still running — a panic unwinding past a normal
    /// teardown call being the main way that happens — request a hard kill so the process doesn't
    /// outlive the session that owned it. Never blocks waiting for it to actually exit.
    fn drop(&mut self) {
        if !self.has_exited() {
            self.signal_kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_and_reads_lines_over_a_real_echoing_process() {
        // -e prints stdin back to stdout — no fixture needed for this plumbing-only test.
        let dir = tempfile::tempdir().unwrap();
        let config = SpawnConfig {
            program: "node".to_owned(),
            args: vec![
                "-e".to_owned(),
                "process.stdin.pipe(process.stdout)".to_owned(),
            ],
            eof_term_grace: Duration::from_millis(50),
            eof_kill_grace: Duration::from_millis(50),
            kill_grace: Duration::from_millis(50),
        };
        let mut proc = ChildProcess::spawn(
            &config,
            Runner::Claude,
            dir.path(),
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        proc.write_line("hello").unwrap();
        match proc.next_line(None).ok().unwrap() {
            NextLine::Line(line) => assert_eq!(line, "hello"),
            NextLine::Closed => panic!("expected a line"),
        }
        proc.close_stdin();
        proc.escalate_after_eof();
        assert!(proc.has_exited());
    }

    #[test]
    fn next_line_reports_timed_out_without_touching_the_channel() {
        let dir = tempfile::tempdir().unwrap();
        let config = SpawnConfig {
            program: "node".to_owned(),
            args: vec!["-e".to_owned(), "setInterval(() => {}, 60000)".to_owned()],
            eof_term_grace: Duration::from_millis(50),
            eof_kill_grace: Duration::from_millis(50),
            kill_grace: Duration::from_millis(50),
        };
        let mut proc = ChildProcess::spawn(
            &config,
            Runner::Claude,
            dir.path(),
            &BTreeMap::new(),
            &BTreeMap::new(),
        )
        .unwrap();
        let deadline = Instant::now() + Duration::from_millis(50);
        assert!(proc.next_line(Some(deadline)).is_err());
        proc.signal_kill();
        proc.wait_for_exit();
    }
}
