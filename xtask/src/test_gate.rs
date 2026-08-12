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

const XTASK_LIBRARY_TEST: TestInvocation = TestInvocation {
    cargo_arguments: &["test", "--package", "xtask", "--lib"],
    notice: "test: xtask library tests run separately to avoid replacing the running xtask binary",
};

pub const fn test_gate_plan() -> [TestInvocation; 2] {
    [WORKSPACE_TEST, XTASK_LIBRARY_TEST]
}

#[cfg(test)]
mod tests {
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
    fn xtask_library_tests_still_run_in_the_test_gate() {
        let [_, xtask_library_test] = test_gate_plan();
        assert_eq!(
            xtask_library_test,
            TestInvocation {
                cargo_arguments: &["test", "--package", "xtask", "--lib"],
                notice: "test: xtask library tests run separately to avoid replacing the running xtask binary",
            }
        );
    }
}
