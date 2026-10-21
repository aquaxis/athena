//! The real process driver (P3-02's `spawn`/`reap` line, design §5.4).
//!
//! `tokio::process` with cwd = the Instance Workspace; the pid is the OS pid, the exit
//! code is what `try_wait` reaped. `terminate` sends the graceful signal (SIGTERM on
//! unix); `force_kill` is the `timeout.stop` overrun's SIGKILL (`ATH-FAIL-015`) and is
//! *called by the manager*, never by the same step as the graceful signal.

use crate::adapter::{ProcessDriver, SpawnedProcess};
use athena_runtime::RuntimeError;
use athena_types::RuntimeInstanceId;
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::process::Child;
use tokio::process::Command;

struct Entry {
    child: Option<Child>,
    exit_code: Option<i32>,
}

#[derive(Default)]
struct Table {
    entries: HashMap<u32, Entry>,
}

/// The real `agent-cli` process driver.
pub struct TokioDriver {
    table: Mutex<Table>,
}

impl Default for TokioDriver {
    fn default() -> Self {
        Self {
            table: Mutex::new(Table::default()),
        }
    }
}

impl TokioDriver {
    /// Reap through `try_wait` and record the exit code; `Some(true)` means still
    /// running, `Some(false)` terminated, `None` the pid is not owned by this table.
    fn reap_by_entry(&self, pid: u32) -> Option<bool> {
        let mut table = self.table.lock().expect("driver table");
        let entry = table.entries.get_mut(&pid)?;
        let Some(child) = entry.child.as_mut() else {
            return Some(false);
        };
        match child.try_wait() {
            Ok(Some(status)) => {
                entry.exit_code = status.code();
                entry.child = None;
                Some(false)
            }
            Ok(None) => Some(true),
            Err(_) => Some(true),
        }
    }

    /// The OS liveness probe for a pid this process does not own (signal 0).
    fn os_probe(pid: u32) -> bool {
        #[cfg(unix)]
        {
            matches!(
                nix::sys::signal::kill(nix::unistd::Pid::from_raw(pid as i32), None),
                Ok(()) | Err(nix::errno::Errno::EPERM)
            )
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
            false
        }
    }

    /// Reap through `try_wait` and record the exit code; `true` means still running.
    fn reap(&self, pid: u32) -> bool {
        let mut table = self.table.lock().expect("driver table");
        let Some(entry) = table.entries.get_mut(&pid) else {
            return false;
        };
        let Some(child) = entry.child.as_mut() else {
            return false;
        };
        match child.try_wait() {
            Ok(Some(status)) => {
                entry.exit_code = status.code();
                entry.child = None;
                false
            }
            Ok(None) => true,
            Err(_) => true,
        }
    }
}

impl ProcessDriver for TokioDriver {
    fn spawn(
        &self,
        workspace: &std::path::Path,
        command: &str,
    ) -> Result<SpawnedProcess, RuntimeError> {
        let mut words = command.split_whitespace();
        let program = words
            .next()
            .ok_or_else(|| RuntimeError::Validation("empty spawn command".into()))?;
        let args: Vec<String> = words.map(String::from).collect();
        // The *spawn launcher* (`agent-cli spawn …`) exits once the detached child has
        // self-registered; its stdout is where the peer agent-id is printed. Everything
        // else (the foreground `serve`) is long-lived and born null-piped.
        let launcher = args.contains(&"spawn".to_string());
        if !launcher {
            let mut cmd = Command::new(program);
            cmd.args(&args);
            cmd.current_dir(workspace);
            cmd.stdin(std::process::Stdio::null());
            cmd.stdout(std::process::Stdio::null());
            cmd.stderr(std::process::Stdio::null());
            let child = cmd.spawn().map_err(|error| {
                RuntimeError::Failure(format!("agent-cli spawn failed: {error}"))
            })?;
            let pid = child
                .id()
                .ok_or_else(|| RuntimeError::Failure("agent-cli spawn reported no pid".into()))?;
            if pid == 0 {
                // Pre-registration semantics: without an OS pid there is nothing to
                // supervise, so this is spawn-class failure, never a recorded instance.
                return Err(RuntimeError::Failure(
                    "agent-cli spawn reported pid 0".into(),
                ));
            }
            self.table.lock().expect("driver table").entries.insert(
                pid,
                Entry {
                    child: Some(child),
                    exit_code: None,
                },
            );
            return Ok(SpawnedProcess {
                pid,
                runtime_instance_id: RuntimeInstanceId::new(),
                peer_id: None,
            });
        }
        // The spawn launcher's exit is the registration window's own signal: run it with
        // std::process (short-lived; CI never reaches this line, ATH-TEST-022) and
        // capture where the peer agent-id is printed.
        {
            let mut std_cmd = std::process::Command::new(program);
            std_cmd
                .args(&args)
                .current_dir(workspace)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            let out = std_cmd.output().map_err(|error| {
                RuntimeError::Failure(format!("spawn launcher failed: {error}"))
            })?;
            std::fs::write(
                std::env::temp_dir().join("athena-launcher-debug.txt"),
                format!(
                    "cmd={};  args={:?};  status={:?};\\nstdout={:?}\\nstderr={:?}",
                    program, args, out.status, out.stdout, out.stderr
                ),
            )
            .ok();
            let stdout = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            if !out.status.success() {
                let stderr = String::from_utf8_lossy(&out.stderr);
                return Err(RuntimeError::Failure(format!(
                    "the spawn launcher did not report a self-registered peer: {}",
                    stderr.trim()
                )));
            }
            // The line reads `id=agent-… name=… pid=<os> socket=…`: the detached
            // child's own pid is what supervision tracks, the agent-id is what the
            // IPC transport addresses.
            let tokens_of = |prefix: &str| {
                stdout
                    .lines()
                    .filter_map(|line| {
                        line.split_whitespace()
                            .find_map(|word| word.strip_prefix(prefix).map(str::to_string))
                    })
                    .next()
            };
            let peer_id = tokens_of("id=");
            let pid = tokens_of("pid=")
                .and_then(|word| word.parse::<u32>().ok())
                .filter(|pid| *pid != 0)
                .ok_or_else(|| {
                    RuntimeError::Failure("the spawn launcher reported no detached pid".into())
                })?;
            Ok(SpawnedProcess {
                pid,
                runtime_instance_id: RuntimeInstanceId::new(),
                peer_id,
            })
        }
    }

    fn is_alive(&self, pid: u32) -> bool {
        // A detached (spawn-launcher) child is not an owned tokio Child: the truth is
        // the OS probe. A recycled pid is the residual risk, carried by agent-cli's
        // pid-based supervision; ATHENA's own states gate on Verification anyway.
        match self.reap_by_entry(pid) {
            Some(alive) => alive,
            None => TokioDriver::os_probe(pid),
        }
    }

    fn exit_code(&self, pid: u32) -> Option<i32> {
        self.reap(pid);
        self.table
            .lock()
            .expect("driver table")
            .entries
            .get(&pid)
            .and_then(|entry| entry.exit_code)
    }

    fn terminate(&self, pid: u32) -> Result<(), RuntimeError> {
        #[cfg(unix)]
        {
            let outcome = nix::sys::signal::kill(
                nix::unistd::Pid::from_raw(pid as i32),
                nix::sys::signal::Signal::SIGTERM,
            );
            if let Err(nix::errno::Errno::ESRCH) = outcome {
                // Already gone: terminal, not an error.
                self.reap(pid);
                return Ok(());
            }
            outcome.map_err(|error| RuntimeError::Failure(format!("SIGTERM failed: {error}")))?;
        }
        #[cfg(not(unix))]
        {
            let _ = pid;
        }
        Ok(())
    }

    fn force_kill(&self, pid: u32) -> Result<(), RuntimeError> {
        let mut table = self.table.lock().expect("driver table");
        if let Some(entry) = table.entries.get_mut(&pid)
            && let Some(child) = entry.child.as_mut()
        {
            child
                .start_kill()
                .map_err(|error| RuntimeError::Failure(format!("kill failed: {error}")))?;
            entry.child = None;
        }
        Ok(())
    }
}
