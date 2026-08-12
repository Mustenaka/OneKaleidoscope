#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TestInvocation {
    pub cargo_arguments: &'static [&'static str],
    pub notice: &'static str,
}

const WORKSPACE_TEST: TestInvocation = TestInvocation {
    cargo_arguments: &[
        "test",
        "--workspace",
        "--exclude",
        "kaleido-recorder",
        "--exclude",
        "xtask",
    ],
    notice: "test: kaleido-recorder and xtask binary excluded from workspace test",
};

const XTASK_TESTS: TestInvocation = TestInvocation {
    cargo_arguments: &[
        "test",
        "--package",
        "xtask",
        "--lib",
        "--test",
        "antipattern",
        "--test",
        "claude_sidecar",
        "--test",
        "deps",
        "--test",
        "fixtures",
        "--test",
        "schema_diff",
    ],
    notice:
        "test: xtask library and integration tests run without rebuilding the active xtask binary",
};

pub const fn test_gate_plan() -> [TestInvocation; 2] {
    [WORKSPACE_TEST, XTASK_TESTS]
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::collections::BTreeSet;
    use std::fs;
    use std::path::Path;

    use super::{test_gate_plan, TestInvocation};

    #[test]
    fn workspace_test_excludes_the_recorder_and_running_xtask_binary() {
        let [workspace_test, _] = test_gate_plan();
        assert_eq!(
            workspace_test,
            TestInvocation {
                cargo_arguments: &[
                    "test",
                    "--workspace",
                    "--exclude",
                    "kaleido-recorder",
                    "--exclude",
                    "xtask",
                ],
                notice: "test: kaleido-recorder and xtask binary excluded from workspace test",
            }
        );
    }

    #[test]
    fn xtask_library_and_integration_tests_still_run_in_the_test_gate() {
        let [_, xtask_tests] = test_gate_plan();
        assert_eq!(
            xtask_tests,
            TestInvocation {
                cargo_arguments: &[
                    "test",
                    "--package",
                    "xtask",
                    "--lib",
                    "--test",
                    "antipattern",
                    "--test",
                    "claude_sidecar",
                    "--test",
                    "deps",
                    "--test",
                    "fixtures",
                    "--test",
                    "schema_diff",
                ],
                notice: "test: xtask library and integration tests run without rebuilding the active xtask binary",
            }
        );
    }

    #[test]
    fn every_xtask_integration_target_is_in_the_test_gate() {
        let [_, xtask_tests] = test_gate_plan();
        let planned = xtask_tests
            .cargo_arguments
            .windows(2)
            .filter_map(|pair| match pair {
                ["--test", target] => Some(*target),
                _ => None,
            })
            .collect::<BTreeSet<_>>();
        let tests_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
        let discovered = fs::read_dir(tests_dir)
            .expect("xtask integration test directory must exist")
            .map(|entry| {
                entry
                    .expect("xtask integration test entry must be readable")
                    .path()
            })
            .filter(|path| path.extension().is_some_and(|extension| extension == "rs"))
            .map(|path| {
                path.file_stem()
                    .expect("integration test file must have a stem")
                    .to_string_lossy()
                    .into_owned()
            })
            .collect::<BTreeSet<_>>();
        let planned = planned
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();

        assert_eq!(planned, discovered);
    }
}
