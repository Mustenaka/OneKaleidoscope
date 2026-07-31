#![allow(clippy::expect_used, clippy::panic, clippy::unwrap_used)]

mod support;

#[cfg(target_os = "windows")]
mod windows {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::thread;
    use std::time::{Duration, SystemTime};

    use kaleido_adapter::content::ContentAccess;
    use kaleido_adapter::session::{
        ProviderRuntimeSession, RuntimeSessionError, SessionStartRequest,
    };
    use kaleido_adapter_codex::{
        CodexRuntimeConfig, CodexRuntimeSession, CodexSandboxMode, ReducerConfig,
    };
    use kaleido_proto::capability::{Capability, CapabilityState, EvidenceSource};
    use kaleido_proto::content::{ContentKind, Sensitivity};
    use kaleido_proto::effect::StateEffect;
    use kaleido_proto::host::{
        ConnectionFaultReason, ConnectionState, HostPlatform, LaunchSurface,
    };
    use kaleido_proto::session::{LiveBinding, LiveUnboundReason};
    use kaleido_proto::turn::TurnOrigin;

    use super::support::{fixture_path, MemoryContent, BASE_AT_MS};

    #[test]
    fn live_handshake_uses_observed_traffic_without_proving_steer() {
        let scratch = Scratch::new(false, true, false);
        let mut content = MemoryContent::default();
        let root_ref = store_root(&mut content, &scratch.root);
        let mut session = runtime(&scratch.executable);
        let request = start_request(&session, root_ref);

        let effects = session.start(&request, &mut content).expect("handshake");
        let live_binding = effects.iter().find_map(|effect| match effect {
            StateEffect::SessionUpserted { session } => Some(&session.live_binding),
            _ => None,
        });
        assert!(matches!(
            live_binding,
            Some(LiveBinding::Observing {
                evidence,
                ..
            }) if evidence.source == EvidenceSource::ObservedInTraffic
        ));
        assert_eq!(
            session
                .capability_probe()
                .to_capabilities()
                .state_of(&Capability::TurnSteer),
            CapabilityState::NotVerified
        );
        let prompt_ref = content
            .store(
                ContentKind::PlainText,
                Sensitivity::Sensitive,
                b"not emitted by the fake runtime",
            )
            .expect("store prompt");
        let prompt_effects = session
            .submit_prompt(&prompt_ref, &mut content)
            .expect("submit prompt");
        assert!(prompt_effects
            .iter()
            .any(|effect| matches!(effect, StateEffect::TurnUpserted { .. })));

        let closed = session.close().expect("clean close");
        assert!(closed.iter().any(|effect| matches!(
            effect,
            StateEffect::RuntimeUpserted { runtime }
                if runtime.connection == ConnectionState::Disconnected
        )));
        assert!(closed.iter().any(|effect| matches!(
            effect,
            StateEffect::SessionUpserted { session }
                if session.live_binding
                    == LiveBinding::NotBound {
                        reason: LiveUnboundReason::RuntimeExited
                    }
        )));
        assert!(!closed
            .iter()
            .any(|effect| matches!(effect, StateEffect::AttentionUpserted { .. })));
        assert!(scratch.fixture_consumed.exists());
        let descendant_pid = fs::read_to_string(&scratch.descendant_pid)
            .expect("descendant pid was recorded")
            .trim()
            .parse::<u32>()
            .expect("descendant pid");
        assert_process_exited(descendant_pid);
    }

    #[test]
    fn early_process_exit_is_reported_once_then_session_is_not_connected() {
        let scratch = Scratch::new(true, false, false);
        let mut content = MemoryContent::default();
        let root_ref = store_root(&mut content, &scratch.root);
        let mut session = runtime(&scratch.executable);
        let request = start_request(&session, root_ref);
        session.start(&request, &mut content).expect("handshake");

        let mut effects = Vec::new();
        for _ in 0..50 {
            effects = session.drain_effects(&mut content).expect("first drain");
            if !effects.is_empty() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(effects.iter().any(|effect| matches!(
            effect,
            StateEffect::RuntimeUpserted { runtime }
                if matches!(
                    runtime.connection,
                    ConnectionState::Unavailable {
                        reason: ConnectionFaultReason::ProcessExited { .. },
                        ..
                    }
                )
        )));
        assert!(effects
            .iter()
            .any(|effect| matches!(effect, StateEffect::AttentionUpserted { .. })));
        assert!(matches!(
            session.drain_effects(&mut content),
            Err(RuntimeSessionError::NotConnected)
        ));
    }

    #[test]
    fn exit_while_waiting_for_turn_response_returns_durable_fault_effects() {
        let scratch = Scratch::new(false, false, true);
        let mut content = MemoryContent::default();
        let root_ref = store_root(&mut content, &scratch.root);
        let mut session = runtime(&scratch.executable);
        let request = start_request(&session, root_ref);
        session.start(&request, &mut content).expect("handshake");
        let prompt_ref = content
            .store(
                ContentKind::PlainText,
                Sensitivity::Sensitive,
                b"fixture-driven transport probe",
            )
            .expect("store prompt");

        let effects = session
            .submit_prompt(&prompt_ref, &mut content)
            .expect("process exit is expressed as effects");
        assert!(effects.iter().any(|effect| matches!(
            effect,
            StateEffect::RuntimeUpserted { runtime }
                if matches!(
                    runtime.connection,
                    ConnectionState::Unavailable {
                        reason: ConnectionFaultReason::ProcessExited { .. },
                        ..
                    }
                )
        )));
        assert!(effects
            .iter()
            .any(|effect| matches!(effect, StateEffect::AttentionUpserted { .. })));
        assert!(matches!(
            session.drain_effects(&mut content),
            Err(RuntimeSessionError::NotConnected)
        ));
    }

    #[test]
    fn invalid_start_metadata_is_rejected_before_process_launch() {
        let scratch = Scratch::new(false, false, false);
        let mut content = MemoryContent::default();
        let root_ref = store_root(&mut content, &scratch.root);
        let mut session = runtime(Path::new("this-executable-must-not-be-launched.exe"));
        let mut request = start_request(&session, root_ref);
        request.project_id = kaleido_proto::ids::ProjectId::new("prj_wrong");

        assert!(matches!(
            session.start(&request, &mut content),
            Err(RuntimeSessionError::ProtocolViolation { .. })
        ));

        request.project_id = session.project_id().clone();
        request.project_root_ref.kind = ContentKind::PlainText;
        assert!(matches!(
            session.start(&request, &mut content),
            Err(RuntimeSessionError::ProtocolViolation { .. })
        ));
    }

    fn runtime(executable: &Path) -> CodexRuntimeSession {
        CodexRuntimeSession::new(CodexRuntimeConfig {
            executable: executable.to_path_buf(),
            reducer: ReducerConfig {
                host_display_name: "runtime-test-host".to_owned(),
                host_platform: HostPlatform::Windows,
                project_display_name: "runtime-test-project".to_owned(),
                identity_salt: "runtime-test-salt".to_owned(),
                evidence: EvidenceSource::ObservedInTraffic,
                launch_surface: LaunchSurface::BrokerLaunched,
                turn_origin: TurnOrigin::LocalSurface,
                base_at_ms: BASE_AT_MS,
                runtime_version_label: Some("0.146.0".to_owned()),
            },
            sandbox: CodexSandboxMode::WorkspaceWrite,
            request_timeout: Duration::from_secs(5),
        })
    }

    fn start_request(
        session: &CodexRuntimeSession,
        project_root_ref: kaleido_proto::content::ContentRef,
    ) -> SessionStartRequest {
        SessionStartRequest {
            project_id: session.project_id().clone(),
            project_binding_id: session.project_binding_id().clone(),
            runtime_id: session.runtime_id().clone(),
            project_root_ref,
        }
    }

    fn store_root(content: &mut MemoryContent, root: &Path) -> kaleido_proto::content::ContentRef {
        let root = root.to_string_lossy().replace('\\', "/");
        content
            .store(
                ContentKind::FilePath,
                Sensitivity::Sensitive,
                root.as_bytes(),
            )
            .expect("store root")
    }

    struct Scratch {
        root: PathBuf,
        executable: PathBuf,
        fixture_consumed: PathBuf,
        descendant_pid: PathBuf,
    }

    impl Scratch {
        fn new(exit_after_handshake: bool, spawn_descendant: bool, exit_during_turn: bool) -> Self {
            let nonce = SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .expect("clock after epoch")
                .as_nanos();
            let root = std::env::temp_dir().join(format!("kaleido-runtime-{nonce}"));
            fs::create_dir(&root).expect("create scratch");
            let executable = root.join("fake-codex.exe");
            let source_path = root.join("fake-codex.rs");
            let fixture = fixture_path("01-simple-turn.jsonl");
            let fixture_consumed = root.join("fixture-consumed");
            let descendant_pid = root.join("descendant-pid");
            let exit = if exit_after_handshake {
                "if count == 3 { std::process::exit(23); }\n"
            } else {
                ""
            };
            let exit_before_turn = if exit_during_turn {
                "if count == 4 { std::process::exit(24); }\n"
            } else {
                ""
            };
            let descendant = if spawn_descendant {
                format!(
                    "let child = std::process::Command::new(std::env::current_exe().unwrap())\n\
                     .arg(\"descendant\")\n\
                     .stdin(std::process::Stdio::null())\n\
                     .stdout(std::process::Stdio::null())\n\
                     .stderr(std::process::Stdio::null())\n\
                     .spawn().unwrap();\n\
                     std::fs::write({:?}, child.id().to_string()).unwrap();\n",
                    descendant_pid.to_string_lossy()
                )
            } else {
                String::new()
            };
            let script = format!(
                "use std::io::{{self, BufRead, Write}};\n\
                 fn payload(line: &str) -> &str {{\n\
                 let start = line.find(\"\\\"payload\\\":\").unwrap() + 10;\n\
                 &line[start..line.len() - 1]\n\
                 }}\n\
                 fn main() {{\n\
                 if std::env::args().any(|arg| arg == \"descendant\") {{\n\
                 std::thread::sleep(std::time::Duration::from_secs(60));\n\
                 return;\n\
                 }}\n\
                 let transcript = std::fs::read_to_string({fixture:?}).unwrap();\n\
                 let lines: Vec<_> = transcript.lines().collect();\n\
                 let initialize = payload(lines[1]).to_owned();\n\
                 let thread = payload(lines[5]).to_owned();\n\
                 let turn = payload(lines[13]).to_owned();\n\
                 std::fs::write({fixture_consumed:?}, \"yes\").unwrap();\n\
                 {descendant}\
                 let stdin = io::stdin();\n\
                 let mut stdout = io::stdout();\n\
                 let mut count = 0;\n\
                 for line in stdin.lock().lines() {{\n\
                 if line.is_err() {{ break; }}\n\
                 count += 1;\n\
                 if count == 1 {{ writeln!(stdout, \"{{}}\", initialize).unwrap(); }}\n\
                 if count == 3 {{ writeln!(stdout, \"{{}}\", thread).unwrap(); }}\n\
                 {exit_before_turn}\
                 if count == 4 {{ writeln!(stdout, \"{{}}\", turn).unwrap(); }}\n\
                 stdout.flush().unwrap();\n\
                 {exit}\
                 }}\n\
                 }}\n"
            );
            fs::write(&source_path, script).expect("write fake app-server source");
            let compiled = Command::new("rustc")
                .args(["--edition", "2021"])
                .arg(&source_path)
                .arg("-o")
                .arg(&executable)
                .output()
                .expect("run rustc");
            assert!(
                compiled.status.success(),
                "compile fake app-server: {}",
                String::from_utf8_lossy(&compiled.stderr)
            );
            Self {
                root,
                executable,
                fixture_consumed,
                descendant_pid,
            }
        }
    }

    fn assert_process_exited(pid: u32) {
        for _ in 0..50 {
            let output = Command::new("tasklist.exe")
                .args(["/FI", &format!("PID eq {pid}"), "/FO", "CSV", "/NH"])
                .output()
                .expect("query process list");
            let stdout = String::from_utf8_lossy(&output.stdout);
            if !stdout.contains(&format!("\"{pid}\"")) {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("descendant process survived clean session close");
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}
