#[path = "common/mod.rs"]
pub mod common;

fn json_array(value: &serde_json::Value) -> &Vec<serde_json::Value> {
    value.as_array().expect("expected JSON array")
}

#[path = "mcp/harness/cache.rs"]
mod cache_fixture;
#[path = "mcp/cache_isolation.rs"]
mod cache_isolation;
pub use common::cli;
#[path = "mcp/cli_contract.rs"]
mod cli_contract;
#[path = "mcp/cli_roots.rs"]
mod cli_roots;

#[path = "mcp/harness/analysis.rs"]
mod analysis_fixture;
#[path = "mcp/harness.rs"]
mod harness;
#[path = "mcp/harness/navigation.rs"]
mod navigation;
#[path = "mcp/harness/routing.rs"]
mod routing_fixture;
#[path = "mcp/harness/server_failures.rs"]
mod server_failures;
#[path = "mcp/harness/sqlite.rs"]
mod sqlite_fixture;

#[path = "mcp/context.rs"]
mod context;
#[path = "mcp/fastmcp_registration.rs"]
mod fastmcp_registration;
#[path = "mcp/freshness.rs"]
mod freshness;
#[path = "mcp/harness_contract.rs"]
mod harness_contract;
#[cfg(unix)]
#[path = "mcp/installer.rs"]
mod installer;
#[path = "mcp/lint_surface.rs"]
mod lint_surface;
#[path = "mcp/lint_tools.rs"]
mod lint_tools;
#[path = "mcp/query_ranking.rs"]
mod query_ranking;
#[path = "mcp/test_read.rs"]
mod test_read;
#[path = "mcp/tool_evolution.rs"]
mod tool_evolution;

#[path = "mcp/transport.rs"]
mod transport;
#[cfg(unix)]
#[path = "mcp/harness/transport.rs"]
mod transport_fixture;

mod binary_identity_guard {
    use acdd_core::fs::{ExecutableDrift, ExecutableIdentity};

    #[test]
    fn executable_identity_digest_seam_baseline() {
        let exe = std::env::current_exe().expect("current test executable path");
        let bytes = std::fs::read(&exe).expect("read current executable bytes");
        let digest = acdd_core::format::sha256_bytes(&bytes);
        assert_eq!(digest.len(), 32);
        assert_eq!(digest, acdd_core::format::sha256_bytes(&bytes));
    }

    #[cfg(unix)]
    fn copy_isolated_server_binary(workspace: &crate::common::Workspace) -> std::path::PathBuf {
        use std::os::unix::fs::PermissionsExt;

        let binary = workspace.path(".acdd/server");
        std::fs::create_dir_all(binary.parent().unwrap()).unwrap();
        std::fs::copy(env!("CARGO_BIN_EXE_acdd"), &binary).unwrap();
        let mut perms = std::fs::metadata(&binary).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&binary, perms).unwrap();
        binary
    }

    #[cfg(unix)]
    #[test]
    fn live_server_refuses_cold_and_warm_delta_when_binary_missing() {
        use crate::common::{mcp_client::StdioClient, Workspace};
        use serde_json::json;
        use std::time::Duration;

        let workspace = Workspace::with_files(&[("src/lib.rs", "pub fn before_drift() {}\n")]);
        let binary = copy_isolated_server_binary(&workspace);
        let backup = workspace.path(".acdd/server.bak");

        // Cold rebuild blocked when startup binary disappears.
        let mut cold_client = StdioClient::with_binary(&workspace, &binary).unwrap();
        std::fs::rename(&binary, &backup).unwrap();
        let cold_reply = cold_client
            .request_value(
                "tools/call",
                json!({ "name": "code_map_overview", "arguments": {} }),
            )
            .unwrap();
        let cold_text = cold_reply.to_string();
        assert!(
            cold_text.contains("BINARY_DRIFT_UNAVAILABLE"),
            "expected BINARY_DRIFT_UNAVAILABLE on cold missing binary, got: {cold_text}"
        );
        assert!(
            !workspace.db().exists(),
            "stale server must not create code-map.sqlite after binary goes missing"
        );
        drop(cold_client);

        // Warmed delta blocked after TTL expires when startup binary disappears.
        std::fs::rename(&backup, &binary).unwrap();
        let mut warm_client = StdioClient::with_binary(&workspace, &binary).unwrap();
        let warm_reply = warm_client
            .request_value(
                "tools/call",
                json!({ "name": "code_map_overview", "arguments": {} }),
            )
            .unwrap();
        assert_eq!(
            warm_reply["result"]["isError"],
            json!(false),
            "{warm_reply}"
        );

        workspace.write(
            "src/lib.rs",
            "pub fn before_drift() {}\npub fn after_drift() {}\n",
        );
        std::thread::sleep(Duration::from_millis(5100));
        std::fs::rename(&binary, &backup).unwrap();

        let delta_reply = warm_client
            .request_value(
                "tools/call",
                json!({ "name": "code_map_search", "arguments": { "query": "after_drift" } }),
            )
            .unwrap();
        let delta_text = delta_reply.to_string();
        assert!(
            delta_text.contains("BINARY_DRIFT_UNAVAILABLE"),
            "expected BINARY_DRIFT_UNAVAILABLE on warm delta missing binary, got: {delta_text}"
        );
        drop(warm_client);

        // Confirm the SQLite index never received `after_drift`.
        let conn = rusqlite::Connection::open(workspace.db()).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM nodes WHERE name = 'after_drift'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            count, 0,
            "stale server must not publish delta after binary drift"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn live_server_reexecs_replaced_binary_to_stable_path_on_warm_delta() {
        use crate::common::{mcp_client::StdioClient, Workspace};
        use serde_json::json;
        use std::os::unix::fs::PermissionsExt;
        use std::time::{Duration, Instant};

        let workspace = Workspace::with_files(&[("src/lib.rs", "pub fn before_drift() {}\n")]);
        let binary = copy_isolated_server_binary(&workspace);
        let backup = workspace.path(".acdd/server.bak");

        let mut client = StdioClient::with_binary(&workspace, &binary).unwrap();
        let pid = client.child().id();
        let warm_reply = client
            .request_value(
                "tools/call",
                json!({ "name": "code_map_overview", "arguments": {} }),
            )
            .unwrap();
        assert_eq!(
            warm_reply["result"]["isError"],
            json!(false),
            "{warm_reply}"
        );

        workspace.write(
            "src/lib.rs",
            "pub fn before_drift() {}\npub fn after_drift() {}\n",
        );
        std::thread::sleep(Duration::from_millis(5100));

        // Move running image to `.bak` and place a distinct valid binary at `.acdd/server`.
        std::fs::rename(&binary, &backup).unwrap();
        let replacement = workspace.path(".acdd/server-next");
        let mut bytes = std::fs::read(env!("CARGO_BIN_EXE_acdd")).unwrap();
        bytes.extend_from_slice(b"\0acdd-v2");
        std::fs::write(&replacement, &bytes).unwrap();
        let mut perms = std::fs::metadata(&replacement).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&replacement, perms).unwrap();
        std::fs::rename(&replacement, &binary).unwrap();

        let reply = client
            .request_value(
                "tools/call",
                json!({ "name": "code_map_search", "arguments": { "query": "after_drift" } }),
            )
            .expect("JSON-RPC BINARY_DRIFT_RELOAD response must flush before re-exec");
        assert!(
            reply.to_string().contains("BINARY_DRIFT_RELOAD"),
            "unexpected reply during binary drift reload: {reply}"
        );

        #[cfg(target_os = "linux")]
        {
            use std::os::unix::fs::MetadataExt;
            let new_inode = std::fs::metadata(&binary).unwrap().ino();
            let backup_inode = std::fs::metadata(&backup).unwrap().ino();
            assert_ne!(new_inode, backup_inode);
            let proc_exe = std::path::PathBuf::from(format!("/proc/{pid}/exe"));
            let deadline = Instant::now() + Duration::from_secs(10);
            let observed = loop {
                let observed = std::fs::metadata(&proc_exe).unwrap().ino();
                if observed == new_inode || Instant::now() >= deadline {
                    break observed;
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            assert_eq!(
                observed, new_inode,
                "server must re-exec stable path `.acdd/server`, not `.bak`"
            );
        }
        assert_eq!(client.child().id(), pid);

        let deadline = Instant::now() + Duration::from_secs(10);
        let post_reload = loop {
            match client.request_value(
                "tools/call",
                json!({ "name": "code_map_search", "arguments": { "query": "after_drift" } }),
            ) {
                Ok(resp) if !resp.to_string().contains("BINARY_DRIFT_RELOAD") => break resp,
                Ok(_) | Err(_) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(err) => panic!("re-exec'd server failed after reload: {err}"),
                Ok(resp) => break resp,
            }
        };
        let post_text = post_reload.to_string();
        assert!(
            post_text.contains("after_drift") && post_reload["result"]["isError"] == json!(false),
            "re-exec'd server should index and serve after_drift, got: {post_text}"
        );
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn startup_replacement_before_identity_capture_blocks_stale_db_write() {
        use contextunity_acdd::mcp::server::Server;
        use rmcp::{
            model::{CallToolRequestParams, CallToolResponse, NumberOrString},
            service::{serve_directly, RequestContext, RoleServer},
            ServerHandler,
        };
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::fs::PermissionsExt;
        use std::process::{Command, Stdio};

        if std::env::var_os("ACDD_STARTUP_RACE_CHILD").is_some() {
            let workspace_root =
                std::path::PathBuf::from(std::env::var_os("ACDD_STARTUP_RACE_WORKSPACE").unwrap());
            let db_path =
                std::path::PathBuf::from(std::env::var_os("ACDD_STARTUP_RACE_DB").unwrap());
            let expected_bin =
                std::path::PathBuf::from(std::env::var_os("ACDD_STARTUP_RACE_BIN").unwrap());
            let expected_digest = std::env::var("ACDD_STARTUP_RACE_DIGEST").unwrap();

            println!("STARTUP_RACE_READY");
            std::io::stdout().flush().unwrap();

            let mut go = String::new();
            std::io::stdin().read_line(&mut go).unwrap();
            assert_eq!(go.trim(), "GO");

            let mode = std::env::var("ACDD_STARTUP_RACE_MODE").unwrap_or_default();
            if mode == "rename_to_backup" || cfg!(target_os = "macos") {
                assert!(
                    ExecutableIdentity::current().is_err(),
                    "ExecutableIdentity::current must fail closed when binary changed before capture ({mode})"
                );
            } else {
                let identity =
                    ExecutableIdentity::current().expect("pinned /proc/self/exe capture");
                let actual_digest: String =
                    identity.digest.iter().map(|b| format!("{b:02x}")).collect();
                assert_eq!(
                    actual_digest, expected_digest,
                    "ExecutableIdentity::current must hash pinned running image, not replaced file"
                );
                assert_eq!(identity.path, expected_bin);
                assert_eq!(
                    identity.check_current(None).unwrap(),
                    ExecutableDrift::Replaced {
                        live_path: expected_bin,
                    }
                );
            }

            let server = Server::new(workspace_root, db_path.clone());
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            let res = rt.block_on(async {
                let (_c, tr) = tokio::io::duplex(4096);
                let running = serve_directly(
                    Server::new(Default::default(), Default::default()),
                    tokio::io::split(tr),
                    None,
                );
                let ctx = RequestContext::<RoleServer>::new(
                    NumberOrString::Number(1),
                    running.peer().clone(),
                );
                let req = CallToolRequestParams::new("code_map_overview".to_owned())
                    .with_arguments(serde_json::Map::new());
                let CallToolResponse::Complete(res) = server.call_tool(req, ctx).await.unwrap()
                else {
                    panic!("incomplete");
                };
                res
            });
            let text = res
                .content
                .first()
                .and_then(|c| c.as_text())
                .map(|t| t.text.as_str())
                .unwrap_or("");
            assert_eq!(res.is_error, Some(true), "expected drift rejection: {text}");
            if mode == "rename_to_backup" || cfg!(target_os = "macos") {
                assert!(
                    text.contains("BINARY_DRIFT_UNAVAILABLE"),
                    "expected fail-closed BINARY_DRIFT_UNAVAILABLE, got: {text}"
                );
            } else {
                assert!(
                    text.contains("BINARY_DRIFT_RELOAD"),
                    "expected BINARY_DRIFT_RELOAD, got: {text}"
                );
            }
            assert!(
                !db_path.exists(),
                "stale server must not publish code-map.sqlite after pre-capture replacement"
            );
            return;
        }

        for mode in ["overwrite", "rename_to_backup"] {
            let workspace = crate::common::Workspace::with_files(&[(
                "src/lib.rs",
                "pub fn startup_race() {}\n",
            )]);
            let runner_bin = workspace.path(".acdd/test-runner-bin");
            std::fs::create_dir_all(runner_bin.parent().unwrap()).unwrap();
            std::fs::copy(std::env::current_exe().unwrap(), &runner_bin).unwrap();
            let mut perms = std::fs::metadata(&runner_bin).unwrap().permissions();
            perms.set_mode(0o755);
            std::fs::set_permissions(&runner_bin, perms).unwrap();

            let v1_bytes = std::fs::read(&runner_bin).unwrap();
            let v1_digest: String = acdd_core::format::sha256_bytes(&v1_bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();

            let mut child = Command::new(&runner_bin)
                .args([
                    "--exact",
                    "binary_identity_guard::startup_replacement_before_identity_capture_blocks_stale_db_write",
                    "--nocapture",
                ])
                .env("ACDD_STARTUP_RACE_CHILD", "1")
                .env("ACDD_STARTUP_RACE_WORKSPACE", workspace.root())
                .env("ACDD_STARTUP_RACE_DB", workspace.db())
                .env("ACDD_STARTUP_RACE_BIN", &runner_bin)
                .env("ACDD_STARTUP_RACE_DIGEST", &v1_digest)
                .env("ACDD_STARTUP_RACE_MODE", mode)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();

            let stdout = child.stdout.take().unwrap();
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            loop {
                line.clear();
                let read = reader.read_line(&mut line).unwrap();
                assert!(read > 0, "child exited before STARTUP_RACE_READY ({mode})");
                if line.trim() == "STARTUP_RACE_READY" {
                    break;
                }
            }

            // Replace the binary on disk AFTER child execve loaded v1 and BEFORE identity capture.
            let replacement = workspace.path(".acdd/test-runner-bin.next");
            if mode == "rename_to_backup" {
                std::fs::rename(&runner_bin, workspace.path(".acdd/test-runner-bin.bak")).unwrap();
            }
            let mut v2_bytes = v1_bytes;
            v2_bytes.extend_from_slice(b"\0v2-replacement");
            std::fs::write(&replacement, &v2_bytes).unwrap();
            let mut repl_perms = std::fs::metadata(&replacement).unwrap().permissions();
            repl_perms.set_mode(0o755);
            std::fs::set_permissions(&replacement, repl_perms).unwrap();
            std::fs::rename(&replacement, &runner_bin).unwrap();

            let mut stdin = child.stdin.take().unwrap();
            stdin.write_all(b"GO\n").unwrap();
            stdin.flush().unwrap();
            drop(stdin);

            let output = child.wait_with_output().unwrap();
            assert!(
                output.status.success(),
                "startup race child ({mode}) failed: stderr={}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(!workspace.db().exists());
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cold_staged_publication_aborts_under_generation_lock_on_binary_drift() {
        use contextunity_acdd::engine::pipeline::{
            build_with_selection_and_guard, hooks::PipelineTestHooks,
        };
        use std::os::unix::fs::PermissionsExt;
        use std::process::Command;

        if std::env::var_os("ACDD_COLD_TOCTOU_CHILD").is_some() {
            let workspace_root =
                std::path::PathBuf::from(std::env::var_os("ACDD_COLD_TOCTOU_WORKSPACE").unwrap());
            let db_path =
                std::path::PathBuf::from(std::env::var_os("ACDD_COLD_TOCTOU_DB").unwrap());
            let runner_bin =
                std::path::PathBuf::from(std::env::var_os("ACDD_COLD_TOCTOU_BIN").unwrap());
            let identity = ExecutableIdentity::current().unwrap();
            assert_eq!(
                identity.check_current(None).unwrap(),
                ExecutableDrift::Current
            );

            let bin_for_hook = runner_bin.clone();
            let hooks = PipelineTestHooks::before_generation_lock(move |_| {
                let mut bytes = std::fs::read(&bin_for_hook).unwrap();
                bytes.extend_from_slice(b"\0v2-during-staging");
                let next = bin_for_hook.with_extension("next");
                std::fs::write(&next, &bytes).unwrap();
                let mut perms = std::fs::metadata(&next).unwrap().permissions();
                perms.set_mode(0o755);
                std::fs::set_permissions(&next, perms).unwrap();
                std::fs::rename(&next, &bin_for_hook).unwrap();
            });
            let guard = || -> anyhow::Result<()> {
                match identity.check_current(None)? {
                    ExecutableDrift::Current => Ok(()),
                    ExecutableDrift::Replaced { live_path } => {
                        anyhow::bail!("BINARY_DRIFT_RELOAD: changed at {}", live_path.display())
                    }
                    ExecutableDrift::Missing { live_path } => {
                        anyhow::bail!(
                            "BINARY_DRIFT_UNAVAILABLE: missing at {}",
                            live_path.display()
                        )
                    }
                }
            };
            let err = build_with_selection_and_guard(
                &workspace_root,
                &db_path,
                None,
                None,
                Some(hooks),
                Some(&guard),
            )
            .unwrap_err();
            assert!(
                err.to_string().contains("BINARY_DRIFT_RELOAD"),
                "expected BINARY_DRIFT_RELOAD from cold generation-lock guard, got: {err:#}"
            );
            return;
        }

        let workspace =
            crate::common::Workspace::with_files(&[("src/lib.rs", "pub fn before_stage() {}\n")]);
        workspace.build();
        let receipt_path = workspace.db().with_extension("sqlite.verified.json");
        let db_before = std::fs::read(workspace.db()).unwrap();
        let receipt_before = std::fs::read(&receipt_path).unwrap();
        workspace.write(
            "src/lib.rs",
            "pub fn before_stage() {}\npub fn after_stage() {}\n",
        );

        let runner_bin = workspace.path(".acdd/cold-toctou-runner");
        std::fs::create_dir_all(runner_bin.parent().unwrap()).unwrap();
        std::fs::copy(std::env::current_exe().unwrap(), &runner_bin).unwrap();
        let mut perms = std::fs::metadata(&runner_bin).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&runner_bin, perms).unwrap();

        let output = Command::new(&runner_bin)
            .args([
                "--exact",
                "binary_identity_guard::cold_staged_publication_aborts_under_generation_lock_on_binary_drift",
            ])
            .env("ACDD_COLD_TOCTOU_CHILD", "1")
            .env("ACDD_COLD_TOCTOU_WORKSPACE", workspace.root())
            .env("ACDD_COLD_TOCTOU_DB", workspace.db())
            .env("ACDD_COLD_TOCTOU_BIN", &runner_bin)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "cold toctou child failed: stderr={}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(std::fs::read(workspace.db()).unwrap(), db_before);
        assert_eq!(std::fs::read(&receipt_path).unwrap(), receipt_before);
    }
}
