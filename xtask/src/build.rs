use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use thiserror::Error;

pub const BUILD_ROOT_ENV: &str = "KALEIDO_BUILD_ROOT";
pub const CARGO_TARGET_DIR_ENV: &str = "CARGO_TARGET_DIR";

#[derive(Debug, Error)]
pub enum BuildRootError {
    #[error("{CARGO_TARGET_DIR_ENV} must be an absolute path")]
    RelativeCargoTargetDir,
    #[error("{BUILD_ROOT_ENV} must be an absolute path")]
    RelativeBuildRoot,
    #[cfg(windows)]
    #[error("Windows build artifacts must be stored on the D: drive")]
    NonExternalWindowsDrive,
}

pub fn cargo_target_dir(workspace_root: &Path) -> Result<PathBuf, BuildRootError> {
    resolve_cargo_target_dir(
        workspace_root,
        std::env::var_os(CARGO_TARGET_DIR_ENV).as_deref(),
        std::env::var_os(BUILD_ROOT_ENV).as_deref(),
    )
}

pub fn resolve_cargo_target_dir(
    workspace_root: &Path,
    cargo_target_dir: Option<&OsStr>,
    build_root: Option<&OsStr>,
) -> Result<PathBuf, BuildRootError> {
    let target_dir = if let Some(target_dir) = cargo_target_dir {
        let target_dir = PathBuf::from(target_dir);
        if !target_dir.is_absolute() {
            return Err(BuildRootError::RelativeCargoTargetDir);
        }
        target_dir
    } else if let Some(build_root) = build_root {
        let build_root = PathBuf::from(build_root);
        if !build_root.is_absolute() {
            return Err(BuildRootError::RelativeBuildRoot);
        }
        build_root.join("cargo-target")
    } else {
        default_cargo_target_dir(workspace_root)
    };

    validate_external_target_dir(&target_dir)?;
    Ok(target_dir)
}

#[cfg(windows)]
fn default_cargo_target_dir(_: &Path) -> PathBuf {
    PathBuf::from(r"D:\OneKaleidoscope\build\cargo-target")
}

#[cfg(not(windows))]
fn default_cargo_target_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join("target")
}

#[cfg(windows)]
fn validate_external_target_dir(target_dir: &Path) -> Result<(), BuildRootError> {
    use std::path::{Component, Prefix};

    let is_d_drive = matches!(
        target_dir.components().next(),
        Some(Component::Prefix(prefix)) if matches!(prefix.kind(), Prefix::Disk(letter) if letter.eq_ignore_ascii_case(&b'D'))
    );
    if is_d_drive {
        Ok(())
    } else {
        Err(BuildRootError::NonExternalWindowsDrive)
    }
}

#[cfg(not(windows))]
fn validate_external_target_dir(_: &Path) -> Result<(), BuildRootError> {
    Ok(())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    use super::{resolve_cargo_target_dir, BuildRootError};

    fn allowed_target_dir() -> PathBuf {
        #[cfg(windows)]
        {
            PathBuf::from(r"D:\OneKaleidoscope\tests\cargo-target")
        }

        #[cfg(not(windows))]
        {
            std::env::temp_dir()
                .join("onekaleidoscope-tests")
                .join("cargo-target")
        }
    }

    #[test]
    fn cargo_target_dir_takes_precedence_over_build_root() {
        let target = allowed_target_dir();
        let result = resolve_cargo_target_dir(
            Path::new("C:/repo"),
            Some(target.as_os_str()),
            Some(OsStr::new("D:/other-build-root")),
        )
        .expect("absolute external cargo target should be accepted");

        assert_eq!(result, target);
    }

    #[test]
    fn build_root_appends_the_shared_cargo_target_directory() {
        let build_root = allowed_target_dir()
            .parent()
            .expect("target test path has a parent")
            .to_path_buf();
        let result =
            resolve_cargo_target_dir(Path::new("C:/repo"), None, Some(build_root.as_os_str()))
                .expect("absolute external build root should be accepted");

        assert_eq!(result, build_root.join("cargo-target"));
    }

    #[test]
    fn relative_cargo_target_directory_is_rejected() {
        let error =
            resolve_cargo_target_dir(Path::new("C:/repo"), Some(OsStr::new("target")), None)
                .expect_err("relative cargo target must fail closed");

        assert!(matches!(error, BuildRootError::RelativeCargoTargetDir));
    }

    #[cfg(windows)]
    #[test]
    fn windows_c_drive_target_directory_is_rejected() {
        let error = resolve_cargo_target_dir(
            Path::new(r"C:\repo"),
            Some(OsStr::new(r"C:\repo\target")),
            None,
        )
        .expect_err("C: target must fail closed");

        assert!(matches!(error, BuildRootError::NonExternalWindowsDrive));
    }
}
