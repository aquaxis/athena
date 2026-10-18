//! ATH-TEST-014: one shared Runtime contract suite; the stub Runtime passes it.
//!
//! The `agent-cli` Adapter passes the same suite through the same runner (P3-01), over a
//! driver double and a scratch workspace root — the checks are the shared ones, not
//! process-manager ones.

use athena_runtime::{StubConfig, StubRuntime, assert_contract};

/// Every shared contract check holds for the stub Runtime.
#[test]
fn the_stub_runtime_passes_the_shared_contract() {
    let runtime = StubRuntime::new(StubConfig::default(), athena_runtime::StubScript::default());
    futures::executor::block_on(async {
        let failures = athena_runtime::run_contract(&runtime).await;
        assert!(failures.is_empty(), "contract failures: {failures:?}");
        // And the aggregate runner agrees.
        assert_contract(&runtime)
            .await
            .expect("the stub passes the contract");
    });
}

mod agent_cli {
    use athena_runtime::{RuntimeError, run_contract_at_workspace};
    use athena_runtime_agent_cli::{AgentCliAdapter, ProcessDriver, SpawnedProcess};
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    /// The driver double: spawns virtual pids, keeps them alive, terminates on request.
    #[derive(Default)]
    struct FakeDriver {
        counter: AtomicU64,
        dead: std::sync::Mutex<Vec<u32>>,
    }

    impl ProcessDriver for FakeDriver {
        fn spawn(&self, workspace: &Path, _command: &str) -> Result<SpawnedProcess, RuntimeError> {
            std::fs::create_dir_all(workspace).map_err(|error| {
                RuntimeError::Validation(format!("workspace create failed: {error}"))
            })?;
            let pid = u32::try_from(self.counter.fetch_add(1, Ordering::SeqCst) + 1000)
                .unwrap_or(u32::MAX);
            Ok(SpawnedProcess {
                pid,
                runtime_instance_id: athena_types::RuntimeInstanceId::new(),
                peer_id: None,
            })
        }

        fn is_alive(&self, pid: u32) -> bool {
            !self.dead.lock().expect("dead").contains(&pid)
        }

        fn exit_code(&self, _pid: u32) -> Option<i32> {
            None
        }

        fn terminate(&self, pid: u32) -> Result<(), RuntimeError> {
            self.dead.lock().expect("dead").push(pid);
            Ok(())
        }

        fn force_kill(&self, pid: u32) -> Result<(), RuntimeError> {
            self.dead.lock().expect("dead").push(pid);
            Ok(())
        }
    }

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "athena-contract-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or_else(|_| 0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).expect("scratch");
        dir
    }

    /// Every shared contract check holds for the `agent-cli` Adapter (P3-01) — the same
    /// runner the stub uses, with the fixture Workspaces mapped under a scratch root.
    /// The template origin is seeded with the aiprj operating-model procedures
    /// (`commands/ai.md`, `next_ai.md`, `close_ai.md`), because the Adapter's send
    /// direction (P3-17, `ATH-ACLI-017`) requires the files it points at to be placed.
    #[test]
    fn the_agent_cli_adapter_passes_the_shared_contract() {
        let scratch = scratch();
        let commands = scratch.join("template").join("commands");
        std::fs::create_dir_all(&commands).expect("template commands");
        for name in ["ai.md", "next_ai.md", "close_ai.md"] {
            std::fs::write(commands.join(name), format!("# {name} origin copy\n"))
                .expect("procedure file");
        }
        let adapter = AgentCliAdapter::new(
            FakeDriver::default(),
            scratch.join("template"),
            4,
            16,
            vec![],
        );
        futures::executor::block_on(async {
            let failures = run_contract_at_workspace(&adapter, &scratch).await;
            assert!(failures.is_empty(), "contract failures: {failures:?}");
        });
        std::fs::remove_dir_all(&scratch).ok();
    }
}
