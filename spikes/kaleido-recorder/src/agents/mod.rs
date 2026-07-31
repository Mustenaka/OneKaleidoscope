use std::error::Error;
use std::ffi::OsString;
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
    {
        return Err(PermissionScopeError::UnprovablePath);
    }
    let supplied = Path::new(raw);
    let supplied_is_absolute = supplied.is_absolute();
    if supplied.components().any(|component| match component {
        Component::Normal(segment) => segment
            .to_string_lossy()
            .chars()
            .any(|character| matches!(character, '*' | '?')),
        Component::Prefix(_) | Component::RootDir if !supplied_is_absolute => true,
        Component::Prefix(_) | Component::RootDir | Component::CurDir | Component::ParentDir => {
            false
        }
    }) {
        return Err(PermissionScopeError::UnsafePath);
    }

    let canonical_sandbox = canonical_sandbox(sandbox)?;
    let candidate = if supplied_is_absolute {
        supplied.to_path_buf()
    } else {
        canonical_sandbox.join(supplied)
    };
    validate_candidate_path(&canonical_sandbox, &candidate).map(|_| ())
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
    let expected = validate_candidate_path(&canonical_sandbox, &expected)?;
    let supplied = Path::new(raw);
    let candidate = if supplied.is_absolute() {
        supplied.to_path_buf()
    } else {
        canonical_sandbox.join(supplied)
    };
    let candidate = validate_candidate_path(&canonical_sandbox, &candidate)?;
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
) -> Result<PathBuf, PermissionScopeError> {
    reject_existing_link_components(candidate)?;
    let normalized = lexically_normalize_absolute(candidate)?;
    let resolved = canonicalize_with_missing_tail(&normalized)?;
    if resolved.starts_with(canonical_sandbox) {
        Ok(resolved)
    } else {
        Err(PermissionScopeError::UnsafePath)
    }
}

fn reject_existing_link_components(path: &Path) -> Result<(), PermissionScopeError> {
    let mut current = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => current.push(prefix.as_os_str()),
            Component::RootDir => current.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !current.pop() {
                    return Err(PermissionScopeError::UnsafePath);
                }
            }
            Component::Normal(segment) => {
                current.push(segment);
                match fs::symlink_metadata(&current) {
                    Ok(_) => {
                        if crate::platform::path_is_link_or_reparse(&current)
                            .map_err(|_| PermissionScopeError::UnprovablePath)?
                        {
                            return Err(PermissionScopeError::UnsafePath);
                        }
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                    Err(_) => return Err(PermissionScopeError::UnprovablePath),
                }
            }
        }
    }
    Ok(())
}

fn lexically_normalize_absolute(path: &Path) -> Result<PathBuf, PermissionScopeError> {
    if !path.is_absolute() {
        return Err(PermissionScopeError::UnprovablePath);
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(PermissionScopeError::UnsafePath);
                }
            }
            Component::Normal(segment) => normalized.push(segment),
        }
    }
    if normalized.is_absolute() {
        Ok(normalized)
    } else {
        Err(PermissionScopeError::UnprovablePath)
    }
}

fn canonicalize_with_missing_tail(path: &Path) -> Result<PathBuf, PermissionScopeError> {
    let mut ancestor = path.to_path_buf();
    let mut missing_tail = Vec::<OsString>::new();
    loop {
        match crate::platform::canonical_permission_path(&ancestor) {
            Ok(mut canonical) => {
                for segment in missing_tail.iter().rev() {
                    canonical.push(segment);
                }
                return Ok(canonical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let segment = ancestor
                    .file_name()
                    .map(OsString::from)
                    .ok_or(PermissionScopeError::UnprovablePath)?;
                missing_tail.push(segment);
                if !ancestor.pop() {
                    return Err(PermissionScopeError::UnprovablePath);
                }
            }
            Err(_) => return Err(PermissionScopeError::UnprovablePath),
        }
    }
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
    #[cfg(windows)]
    use super::validate_exact_permission_cwd;
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
    fn exact_permission_path_accepts_a_windows_verbatim_prefix() -> Result<(), Box<dyn Error>> {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        fs::create_dir(&sandbox)?;
        fs::write(sandbox.join("inside.txt"), b"inside")?;
        let canonical = platform::permission_path_pattern(&sandbox.join("inside.txt"))?;
        let verbatim = if let Some(unc) = canonical.strip_prefix(r"\\") {
            format!(r"\\?\UNC\{unc}")
        } else {
            format!(r"\\?\{canonical}")
        };

        assert!(
            validate_exact_permission_path(&sandbox, &verbatim, Path::new("inside.txt")).is_ok()
        );
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn exact_permission_path_accepts_a_different_drive_letter_case() -> Result<(), Box<dyn Error>> {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        fs::create_dir(&sandbox)?;
        fs::write(sandbox.join("inside.txt"), b"inside")?;
        let canonical = platform::permission_path_pattern(&sandbox.join("inside.txt"))?;
        let drive = canonical
            .chars()
            .next()
            .filter(char::is_ascii_alphabetic)
            .ok_or("temporary path did not start with a drive letter")?;
        let changed_drive = if drive.is_ascii_uppercase() {
            drive.to_ascii_lowercase()
        } else {
            drive.to_ascii_uppercase()
        };
        let different_case = format!("{changed_drive}{}", &canonical[drive.len_utf8()..]);

        assert!(
            validate_exact_permission_path(&sandbox, &different_case, Path::new("inside.txt"))
                .is_ok()
        );
        Ok(())
    }

    #[cfg(windows)]
    #[test]
    fn exact_permission_cwd_accepts_a_trailing_separator() -> Result<(), Box<dyn Error>> {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        fs::create_dir(&sandbox)?;
        let canonical = platform::permission_path_pattern(&sandbox)?;
        let with_trailing_separator = format!("{canonical}\\");

        assert!(validate_exact_permission_cwd(&sandbox, &with_trailing_separator).is_ok());
        Ok(())
    }

    #[test]
    fn exact_permission_path_accepts_safe_dotdot_normalization() -> Result<(), Box<dyn Error>> {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        fs::create_dir(&sandbox)?;
        fs::create_dir(sandbox.join("nested"))?;
        fs::write(sandbox.join("inside.txt"), b"inside")?;
        let equivalent = sandbox
            .join("nested")
            .join("..")
            .join("inside.txt")
            .to_string_lossy()
            .into_owned();

        assert!(
            validate_exact_permission_path(&sandbox, &equivalent, Path::new("inside.txt")).is_ok()
        );
        Ok(())
    }

    #[test]
    fn exact_permission_path_rejects_dotdot_that_escapes_sandbox() -> Result<(), Box<dyn Error>> {
        let temporary = tempfile::tempdir()?;
        let sandbox = temporary.path().join("sandbox");
        fs::create_dir(&sandbox)?;
        fs::write(sandbox.join("inside.txt"), b"inside")?;
        fs::write(temporary.path().join("outside.txt"), b"outside")?;
        let escaping = sandbox
            .join("nested")
            .join("..")
            .join("..")
            .join("outside.txt")
            .to_string_lossy()
            .into_owned();

        assert!(matches!(
            validate_exact_permission_path(&sandbox, &escaping, Path::new("inside.txt")),
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
