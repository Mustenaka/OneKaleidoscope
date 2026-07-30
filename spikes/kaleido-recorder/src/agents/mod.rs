use std::error::Error;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;
use thiserror::Error;

use crate::platform::ProcessCleanupDiagnostic;

pub mod acp;
pub mod codex;
pub mod opencode;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CleanupIssue {
    pub unconfirmed_pids: Vec<u32>,
    pub error: String,
}

impl CleanupIssue {
    pub fn from_error<E>(error: &E) -> Self
    where
        E: Error + ProcessCleanupDiagnostic + 'static,
    {
        Self {
            unconfirmed_pids: error.unconfirmed_pids(),
            error: error.to_string(),
        }
    }
}

#[derive(Debug)]
pub struct CompletedRecording<T> {
    pub outcome: T,
    pub cleanup_issues: Vec<CleanupIssue>,
}

impl<T> CompletedRecording<T> {
    pub fn clean(outcome: T) -> Self {
        Self {
            outcome,
            cleanup_issues: Vec::new(),
        }
    }

    pub fn add_cleanup_error<E>(&mut self, error: &E)
    where
        E: Error + ProcessCleanupDiagnostic + 'static,
    {
        self.cleanup_issues.push(CleanupIssue::from_error(error));
    }

    pub fn with_cleanup_result<E>(outcome: T, cleanup: Result<(), E>) -> Self
    where
        E: Error + ProcessCleanupDiagnostic + 'static,
    {
        let mut recording = Self::clean(outcome);
        recording.add_cleanup_result(cleanup);
        recording
    }

    pub fn add_cleanup_result<E>(&mut self, cleanup: Result<(), E>)
    where
        E: Error + ProcessCleanupDiagnostic + 'static,
    {
        if let Err(error) = cleanup {
            self.add_cleanup_error(&error);
        }
    }
}

#[derive(Debug, Error)]
pub enum PermissionScopeError {
    #[error("permission request did not contain a structurally safe command")]
    UnsafeCommand,
    #[error("permission request contained a path outside the canonical fixture sandbox")]
    UnsafePath,
    #[error("permission request contained a path whose scope could not be proven")]
    UnprovablePath,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PermissionCommand {
    Run,
    Wait,
    Fail,
}

pub fn validate_permission_command(command: &str) -> Result<(), PermissionScopeError> {
    validate_permission_command_as(command, PermissionCommand::Run)
}

pub fn validate_permission_command_as(
    command: &str,
    expected: PermissionCommand,
) -> Result<(), PermissionScopeError> {
    let arguments = tokenize_permission_command(command)?;
    validate_permission_argv_as(&arguments, expected)
}

pub fn validate_permission_argv(arguments: &[String]) -> Result<(), PermissionScopeError> {
    validate_permission_argv_as(arguments, PermissionCommand::Run)
}

pub fn validate_permission_argv_as(
    arguments: &[String],
    expected: PermissionCommand,
) -> Result<(), PermissionScopeError> {
    let normalized_program = arguments
        .first()
        .map(|program| program.to_ascii_lowercase())
        .ok_or(PermissionScopeError::UnsafeCommand)?;
    let safe_program = matches!(normalized_program.as_str(), "cargo" | "cargo.exe");
    let safe_arguments = match expected {
        PermissionCommand::Run => {
            matches!(
                arguments,
                [_, run] if run == "run"
            ) || matches!(
                arguments,
                [_, run, separator] if run == "run" && separator == "--"
            )
        }
        PermissionCommand::Wait => matches!(
            arguments,
            [_, run, separator, subcommand]
                if run == "run" && separator == "--" && subcommand == "wait"
        ),
        PermissionCommand::Fail => matches!(
            arguments,
            [_, run, separator, subcommand]
                if run == "run" && separator == "--" && subcommand == "fail"
        ),
    };
    if safe_program && safe_arguments {
        Ok(())
    } else {
        Err(PermissionScopeError::UnsafeCommand)
    }
}

fn tokenize_permission_command(command: &str) -> Result<Vec<String>, PermissionScopeError> {
    if command.is_empty()
        || command.contains("<OUTSIDE_PATH>")
        || command.chars().any(|character| {
            matches!(
                character,
                '\r' | '\n'
                    | '\0'
                    | '&'
                    | '|'
                    | ';'
                    | '<'
                    | '>'
                    | '`'
                    | '$'
                    | '%'
                    | '!'
                    | '^'
                    | '('
                    | ')'
                    | '{'
                    | '}'
            )
        })
    {
        return Err(PermissionScopeError::UnsafeCommand);
    }

    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for character in command.chars() {
        match quote {
            Some(expected) if character == expected => quote = None,
            Some(_) => current.push(character),
            None if matches!(character, '\'' | '"') => quote = Some(character),
            None if character.is_whitespace() => {
                if !current.is_empty() {
                    arguments.push(std::mem::take(&mut current));
                }
            }
            None => current.push(character),
        }
    }
    if quote.is_some() {
        return Err(PermissionScopeError::UnsafeCommand);
    }
    if !current.is_empty() {
        arguments.push(current);
    }
    Ok(arguments)
}

pub fn validate_exact_permission_cwd(
    sandbox: &Path,
    cwd: &str,
) -> Result<(), PermissionScopeError> {
    let canonical_sandbox = canonical_sandbox(sandbox)?;
    let canonical_cwd = canonicalize_scope_path(Path::new(cwd))?;
    if canonical_cwd == canonical_sandbox
        && !crate::platform::path_is_link_or_reparse(Path::new(cwd))
            .map_err(|_| PermissionScopeError::UnprovablePath)?
    {
        Ok(())
    } else {
        Err(PermissionScopeError::UnsafePath)
    }
}

pub fn validate_permission_path(sandbox: &Path, raw: &str) -> Result<(), PermissionScopeError> {
    if raw.trim().is_empty()
        || raw.contains("<OUTSIDE_PATH>")
        || raw.starts_with('~')
        || raw.contains("://")
        || raw.contains('*')
        || raw.contains('?')
    {
        return Err(PermissionScopeError::UnprovablePath);
    }
    let supplied = Path::new(raw);
    let supplied_is_absolute = supplied.is_absolute();
    if supplied.components().any(|component| {
        matches!(component, Component::ParentDir)
            || (!supplied_is_absolute
                && matches!(component, Component::Prefix(_) | Component::RootDir))
    }) {
        return Err(PermissionScopeError::UnsafePath);
    }

    let canonical_sandbox = canonical_sandbox(sandbox)?;
    let candidate = if supplied_is_absolute {
        supplied.to_path_buf()
    } else {
        canonical_sandbox.join(supplied)
    };
    validate_candidate_path(&canonical_sandbox, &candidate)
}

pub fn validate_exact_permission_path(
    sandbox: &Path,
    raw: &str,
    expected_relative: &Path,
) -> Result<(), PermissionScopeError> {
    if expected_relative.is_absolute()
        || expected_relative.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return Err(PermissionScopeError::UnsafePath);
    }
    validate_permission_path(sandbox, raw)?;
    let canonical_sandbox = canonical_sandbox(sandbox)?;
    let expected = canonical_sandbox.join(expected_relative);
    validate_candidate_path(&canonical_sandbox, &expected)?;
    let expected = canonicalize_scope_path(&expected)?;
    let supplied = Path::new(raw);
    let candidate = if supplied.is_absolute() {
        supplied.to_path_buf()
    } else {
        canonical_sandbox.join(supplied)
    };
    let candidate = canonicalize_scope_path(&candidate)?;
    if candidate == expected {
        Ok(())
    } else {
        Err(PermissionScopeError::UnsafePath)
    }
}

fn canonical_sandbox(sandbox: &Path) -> Result<PathBuf, PermissionScopeError> {
    if crate::platform::path_is_link_or_reparse(sandbox)
        .map_err(|_| PermissionScopeError::UnprovablePath)?
    {
        return Err(PermissionScopeError::UnsafePath);
    }
    canonicalize_scope_path(sandbox)
}

fn canonicalize_scope_path(path: &Path) -> Result<PathBuf, PermissionScopeError> {
    crate::platform::canonical_permission_path(path)
        .map_err(|_| PermissionScopeError::UnprovablePath)
}

fn validate_candidate_path(
    canonical_sandbox: &Path,
    candidate: &Path,
) -> Result<(), PermissionScopeError> {
    let relative = candidate
        .strip_prefix(canonical_sandbox)
        .map_err(|_| PermissionScopeError::UnsafePath)?;
    let mut current = canonical_sandbox.to_path_buf();
    for component in relative.components() {
        match component {
            Component::Normal(segment) => current.push(segment),
            Component::CurDir => continue,
            Component::ParentDir | Component::Prefix(_) | Component::RootDir => {
                return Err(PermissionScopeError::UnsafePath);
            }
        }
        match fs::symlink_metadata(&current) {
            Ok(_) => {
                if crate::platform::path_is_link_or_reparse(&current)
                    .map_err(|_| PermissionScopeError::UnprovablePath)?
                {
                    return Err(PermissionScopeError::UnsafePath);
                }
                let canonical = canonicalize_scope_path(&current)?;
                if !canonical.starts_with(canonical_sandbox) {
                    return Err(PermissionScopeError::UnsafePath);
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(_) => return Err(PermissionScopeError::UnprovablePath),
        }
    }
    Ok(())
}

pub fn validate_path_array(sandbox: &Path, value: &Value) -> Result<(), PermissionScopeError> {
    let values = value
        .as_array()
        .filter(|values| !values.is_empty())
        .ok_or(PermissionScopeError::UnprovablePath)?;
    for value in values {
        let path = value.as_str().ok_or(PermissionScopeError::UnprovablePath)?;
        validate_permission_path(sandbox, path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::fs;
    use std::path::Path;

    use super::acp::{self, AcpError};
    use super::{
        validate_exact_permission_path, validate_permission_command,
        validate_permission_command_as, validate_permission_path, CleanupIssue, PermissionCommand,
        PermissionScopeError,
    };
    use crate::platform;

    #[test]
    fn cleanup_issue_preserves_nested_unconfirmed_process_ids() {
        let error = platform::ProcessError::CombinedCleanup {
            root: Box::new(platform::ProcessError::Terminate(std::io::Error::other(
                "forced root cleanup failure",
            ))),
            descendants: Box::new(platform::ProcessError::IncompleteCleanup {
                root_pid: 7,
                unconfirmed_pids: vec![43, 42, 43],
                detail: "forced descendant cleanup failure".to_owned(),
            }),
        };

        let issue = CleanupIssue::from_error(&error);

        assert_eq!(issue.unconfirmed_pids, [42, 43]);
        assert!(issue.error.contains("forced root cleanup failure"));
        assert!(issue.error.contains("forced descendant cleanup failure"));
    }

    #[test]
    fn acp_sandbox_validation_rejects_a_linked_expected_root() -> Result<(), Box<dyn Error>> {
        let temporary = tempfile::tempdir()?;
        let target = temporary.path().join("real-project");
        let expected = temporary.path().join("tests/fixtures/sandbox");
        fs::create_dir_all(&target)?;
        fs::create_dir_all(
            expected
                .parent()
                .ok_or("linked sandbox must have a parent")?,
        )?;
        platform::create_test_directory_link(&target, &expected)?;

        assert!(matches!(
            acp::validate_fixture_sandbox_against(&target, &expected),
            Err(AcpError::InvalidSandbox)
        ));
        Ok(())
    }

    #[test]
    fn permission_scope_accepts_only_the_structured_recorder_command() {
        assert!(validate_permission_command("cargo run").is_ok());
        assert!(validate_permission_command("cargo run --").is_ok());
        for unsafe_command in [
            "cargo run -- fail",
            r"C:\outside\cargo.exe run",
            "/outside/cargo run",
            "cargo run & type C:\\private.txt",
            "cargo run; cat /private",
            "powershell -Command Get-Content ..\\private.txt",
            "<OUTSIDE_PATH>",
        ] {
            assert!(matches!(
                validate_permission_command(unsafe_command),
                Err(PermissionScopeError::UnsafeCommand)
            ));
        }
    }

    #[test]
    fn permission_scope_accepts_only_the_selected_recorder_subcommand() {
        assert!(
            validate_permission_command_as("cargo run -- wait", PermissionCommand::Wait).is_ok()
        );
        assert!(
            validate_permission_command_as("cargo.exe run -- fail", PermissionCommand::Fail)
                .is_ok()
        );
        for (command, expected) in [
            ("cargo run -- fail", PermissionCommand::Wait),
            ("cargo run -- wait", PermissionCommand::Fail),
            ("cargo run -- wait extra", PermissionCommand::Wait),
            (r"C:\outside\cargo.exe run -- wait", PermissionCommand::Wait),
        ] {
            assert!(matches!(
                validate_permission_command_as(command, expected),
                Err(PermissionScopeError::UnsafeCommand)
            ));
        }
    }

    #[test]
    fn permission_scope_rejects_traversal_absolute_outside_and_placeholders(
    ) -> Result<(), Box<dyn Error>> {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        fs::create_dir(&sandbox)?;
        fs::write(sandbox.join("inside.txt"), b"inside")?;
        let outside = temporary.path().join("outside.txt");
        fs::write(&outside, b"outside")?;

        assert!(validate_permission_path(&sandbox, "inside.txt").is_ok());
        for unsafe_path in [
            "..\\outside.txt".to_owned(),
            outside.to_string_lossy().into_owned(),
            "<OUTSIDE_PATH>".to_owned(),
        ] {
            assert!(validate_permission_path(&sandbox, &unsafe_path).is_err());
        }
        Ok(())
    }

    #[test]
    fn permission_scope_rejects_a_link_or_junction_before_a_missing_leaf(
    ) -> Result<(), Box<dyn Error>> {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        let outside = temporary.path().join("outside");
        let linked = sandbox.join("linked");
        fs::create_dir(&sandbox)?;
        fs::create_dir(&outside)?;
        platform::create_test_directory_link(&outside, &linked)?;

        assert!(validate_permission_path(&sandbox, "linked/new.txt").is_err());
        Ok(())
    }

    #[test]
    fn exact_permission_path_rejects_a_different_safe_sandbox_file() -> Result<(), Box<dyn Error>> {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        fs::create_dir(&sandbox)?;
        fs::write(sandbox.join("notes.txt"), b"notes")?;
        fs::write(sandbox.join("other.txt"), b"other")?;

        assert!(
            validate_exact_permission_path(&sandbox, "notes.txt", Path::new("notes.txt")).is_ok()
        );
        assert!(matches!(
            validate_exact_permission_path(&sandbox, "other.txt", Path::new("notes.txt")),
            Err(PermissionScopeError::UnsafePath)
        ));
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn permission_scope_rejects_windows_drive_and_unc_paths_outside_sandbox(
    ) -> Result<(), Box<dyn Error>> {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        fs::create_dir(&sandbox)?;

        for outside in [
            r"C:\Windows\win.ini",
            r"C:drive-relative.txt",
            r"\\server\share\secret.txt",
        ] {
            assert!(validate_permission_path(&sandbox, outside).is_err());
        }
        Ok(())
    }
}
