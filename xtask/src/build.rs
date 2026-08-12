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

    Ok(target_dir)
}

fn default_cargo_target_dir(workspace_root: &Path) -> PathBuf {
    workspace_root.join("target")
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    use super::{resolve_cargo_target_dir, BuildRootError};

    fn allowed_target_dir() -> PathBuf {
        std::env::temp_dir()
            .join("onekaleidoscope-tests")
            .join("cargo-target")
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

    #[test]
    fn default_target_directory_is_the_workspace_target() {
        let workspace = std::env::temp_dir().join("onekaleidoscope-workspace");
        let result = resolve_cargo_target_dir(&workspace, None, None)
            .expect("the default workspace target must be accepted");

        assert_eq!(result, workspace.join("target"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_c_drive_target_directory_is_accepted() {
        let target = Path::new(r"C:\OneKaleidoscope\build\cargo-target");
        let result =
            resolve_cargo_target_dir(Path::new(r"C:\repo"), Some(target.as_os_str()), None)
                .expect("an explicit absolute target on C: must be portable");

        assert_eq!(result, target);
    }
}
