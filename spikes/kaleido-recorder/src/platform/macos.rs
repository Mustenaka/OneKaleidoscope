use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{Candidate, CandidateStatus, DiscoveryTarget};

pub(super) fn extend_known_executable_directories(directories: &mut Vec<PathBuf>) {
    directories.extend([
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/opt/homebrew/bin"),
        PathBuf::from("/opt/local/bin"),
        PathBuf::from("/usr/bin"),
    ]);
}

pub(super) fn append_installation_evidence(
    target: DiscoveryTarget,
    home_directory: &Path,
    candidates: &mut Vec<Candidate>,
    diagnostics: &mut Vec<String>,
) {
    if !matches!(
        target,
        DiscoveryTarget::Codex | DiscoveryTarget::ClaudeCli | DiscoveryTarget::OpenCode
    ) {
        diagnostics.push(format!(
            "no documented macOS GUI distribution is enumerated for {target}"
        ));
        return;
    }

    let roots = application_roots(home_directory);
    scan_application_bundles(target, &roots, candidates, diagnostics);
}

fn application_roots(home_directory: &Path) -> [PathBuf; 2] {
    [
        PathBuf::from("/Applications"),
        home_directory.join("Applications"),
    ]
}

fn scan_application_bundles(
    target: DiscoveryTarget,
    roots: &[PathBuf],
    candidates: &mut Vec<Candidate>,
    diagnostics: &mut Vec<String>,
) {
    diagnostics.push(
        "macOS application bundle scan is static and does not infer that a bundle is a runnable \
         protocol launcher"
            .to_owned(),
    );
    for root in roots {
        let entries = match fs::read_dir(root) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                diagnostics.push(format!(
                    "known macOS application root not present={}",
                    root.to_string_lossy()
                ));
                continue;
            }
            Err(error) => {
                diagnostics.push(format!(
                    "known macOS application root inaccessible={} ({:?})",
                    root.to_string_lossy(),
                    error.kind()
                ));
                continue;
            }
        };
        let mut matches = 0_u32;
        for entry in entries {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    diagnostics.push(format!(
                        "failed to inspect an entry under macOS application root={} ({:?})",
                        root.to_string_lossy(),
                        error.kind()
                    ));
                    continue;
                }
            };
            let path = entry.path();
            if !application_bundle_name_matches(target, &path) {
                continue;
            }
            match entry.metadata() {
                Ok(metadata) if metadata.is_dir() => {
                    matches = matches.saturating_add(1);
                    push_evidence(candidates, path.clone());
                    diagnostics.push(format!(
                        "macOS GUI installation evidence={} (bundle directory only; not a \
                         runnable CLI)",
                        path.to_string_lossy()
                    ));
                }
                Ok(_) => diagnostics.push(format!(
                    "ignored matching macOS application artifact that is not a directory={}",
                    path.to_string_lossy()
                )),
                Err(error) => diagnostics.push(format!(
                    "matching macOS application bundle inaccessible={} ({:?})",
                    path.to_string_lossy(),
                    error.kind()
                )),
            }
        }
        if matches == 0 {
            diagnostics.push(format!(
                "known macOS application root scanned with no matching bundle={}",
                root.to_string_lossy()
            ));
        }
    }
    candidates.sort_by(|left, right| left.path.cmp(&right.path));
}

fn application_bundle_name_matches(target: DiscoveryTarget, path: &Path) -> bool {
    if !path
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
    {
        return false;
    }
    let Some(file_name) = path.file_name() else {
        return false;
    };
    let normalized: String = file_name
        .to_string_lossy()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect();
    match target {
        DiscoveryTarget::Codex => normalized.contains("codex"),
        DiscoveryTarget::ClaudeCli => normalized.contains("claude"),
        DiscoveryTarget::OpenCode => normalized.contains("opencode"),
        DiscoveryTarget::ClaudeAcp | DiscoveryTarget::Node => false,
    }
}

fn push_evidence(candidates: &mut Vec<Candidate>, path: PathBuf) {
    if candidates.iter().all(|candidate| candidate.path != path) {
        candidates.push(Candidate {
            path,
            status: CandidateStatus::DirectoryEvidence,
        });
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use tempfile::tempdir;

    use super::{
        append_installation_evidence, application_roots, extend_known_executable_directories,
        scan_application_bundles,
    };
    use crate::platform::{Candidate, CandidateStatus, DiscoveryTarget};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn known_executable_directories_cover_native_and_package_manager_prefixes() {
        let mut directories = Vec::new();
        let home = PathBuf::from("test-home");

        extend_known_executable_directories(&mut directories);

        assert_eq!(
            directories,
            [
                PathBuf::from("/usr/local/bin"),
                PathBuf::from("/opt/homebrew/bin"),
                PathBuf::from("/opt/local/bin"),
                PathBuf::from("/usr/bin"),
            ]
        );
        assert_eq!(
            application_roots(&home),
            [PathBuf::from("/Applications"), home.join("Applications"),]
        );
    }

    #[test]
    fn system_or_user_application_bundle_is_directory_evidence_for_each_agent() -> TestResult {
        let temporary = tempdir()?;
        let home = temporary.path().join("home");
        let applications = home.join("Applications");
        fs::create_dir_all(&applications)?;
        for bundle in ["Codex.app", "Claude.app", "OpenCode.app", "Unrelated.app"] {
            fs::create_dir(applications.join(bundle))?;
        }
        fs::write(applications.join("FakeCodex.app"), b"not a directory")?;
        fs::create_dir(applications.join("Codex.txt"))?;
        for (target, expected) in [
            (DiscoveryTarget::Codex, "Codex.app"),
            (DiscoveryTarget::ClaudeCli, "Claude.app"),
            (DiscoveryTarget::OpenCode, "OpenCode.app"),
        ] {
            let mut candidates = Vec::new();
            let mut diagnostics = Vec::new();
            append_installation_evidence(target, &home, &mut candidates, &mut diagnostics);

            assert_eq!(
                candidates,
                [Candidate {
                    path: applications.join(expected),
                    status: CandidateStatus::DirectoryEvidence,
                }]
            );
            assert!(diagnostics
                .iter()
                .any(|diagnostic| diagnostic.contains("not a runnable CLI")));
        }
        Ok(())
    }

    #[test]
    fn wrong_kind_or_inaccessible_application_root_never_becomes_evidence() -> TestResult {
        let temporary = tempdir()?;
        let root_file = temporary.path().join("not-a-directory");
        fs::write(&root_file, b"not a directory")?;
        let applications = temporary.path().join("Applications");
        fs::create_dir(&applications)?;
        fs::write(applications.join("Claude.app"), b"not an app directory")?;
        let roots = [root_file, applications];
        let mut candidates = Vec::new();
        let mut diagnostics = Vec::new();

        scan_application_bundles(
            DiscoveryTarget::ClaudeCli,
            &roots,
            &mut candidates,
            &mut diagnostics,
        );

        assert!(candidates.is_empty());
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("root inaccessible")));
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("not a directory")));
        Ok(())
    }
}
