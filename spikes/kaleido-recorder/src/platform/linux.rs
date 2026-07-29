use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use super::{Candidate, CandidateStatus, DiscoveryTarget};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum EvidenceRootKind {
    NamedDirectory,
    DesktopEntry,
    GuiExecutable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct EvidenceRoot {
    path: PathBuf,
    kind: EvidenceRootKind,
}

pub(super) fn extend_known_executable_directories(directories: &mut Vec<PathBuf>) {
    directories.extend([
        PathBuf::from("/usr/local/bin"),
        PathBuf::from("/usr/bin"),
        PathBuf::from("/snap/bin"),
        PathBuf::from("/home/linuxbrew/.linuxbrew/bin"),
    ]);
}

pub(super) fn append_installation_evidence(
    target: DiscoveryTarget,
    data_directory: &Path,
    executable_directories: &[PathBuf],
    candidates: &mut Vec<Candidate>,
    diagnostics: &mut Vec<String>,
) {
    if !matches!(
        target,
        DiscoveryTarget::ClaudeCli | DiscoveryTarget::OpenCode
    ) {
        diagnostics.push(format!(
            "no documented Linux GUI distribution is enumerated for {target}"
        ));
        return;
    }

    let roots = installation_roots(data_directory, executable_directories);
    if target == DiscoveryTarget::OpenCode {
        diagnostics.push(
            "OpenCode AppImage has no mandated installation directory; only statically known \
             executable directories are inspected"
                .to_owned(),
        );
    }
    scan_installation_roots(target, &roots, candidates, diagnostics);
}

fn installation_roots(
    data_directory: &Path,
    executable_directories: &[PathBuf],
) -> Vec<EvidenceRoot> {
    let mut roots = vec![
        EvidenceRoot {
            path: PathBuf::from("/opt"),
            kind: EvidenceRootKind::NamedDirectory,
        },
        EvidenceRoot {
            path: data_directory.join("applications"),
            kind: EvidenceRootKind::DesktopEntry,
        },
        EvidenceRoot {
            path: PathBuf::from("/usr/local/share/applications"),
            kind: EvidenceRootKind::DesktopEntry,
        },
        EvidenceRoot {
            path: PathBuf::from("/usr/share/applications"),
            kind: EvidenceRootKind::DesktopEntry,
        },
    ];
    roots.extend(
        executable_directories
            .iter()
            .cloned()
            .map(|path| EvidenceRoot {
                path,
                kind: EvidenceRootKind::GuiExecutable,
            }),
    );
    roots.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| evidence_kind_order(left.kind).cmp(&evidence_kind_order(right.kind)))
    });
    roots.dedup();
    roots
}

const fn evidence_kind_order(kind: EvidenceRootKind) -> u8 {
    match kind {
        EvidenceRootKind::NamedDirectory => 0,
        EvidenceRootKind::DesktopEntry => 1,
        EvidenceRootKind::GuiExecutable => 2,
    }
}

fn scan_installation_roots(
    target: DiscoveryTarget,
    roots: &[EvidenceRoot],
    candidates: &mut Vec<Candidate>,
    diagnostics: &mut Vec<String>,
) {
    diagnostics.push(
        "Linux GUI evidence scan is static and does not infer that an installation directory or \
         metadata file is a runnable protocol launcher"
            .to_owned(),
    );
    for root in roots {
        let entries = match fs::read_dir(&root.path) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                diagnostics.push(format!(
                    "known Linux GUI/installation root not present={}",
                    root.path.to_string_lossy()
                ));
                continue;
            }
            Err(error) => {
                diagnostics.push(format!(
                    "known Linux GUI/installation root inaccessible={} ({:?})",
                    root.path.to_string_lossy(),
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
                        "failed to inspect an entry under Linux GUI/installation root={} ({:?})",
                        root.path.to_string_lossy(),
                        error.kind()
                    ));
                    continue;
                }
            };
            let path = entry.path();
            if !installation_name_matches(target, root.kind, &path) {
                continue;
            }
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(error) => {
                    diagnostics.push(format!(
                        "matching Linux GUI/installation artifact inaccessible={} ({:?})",
                        path.to_string_lossy(),
                        error.kind()
                    ));
                    continue;
                }
            };
            let status = match root.kind {
                EvidenceRootKind::NamedDirectory if metadata.is_dir() => {
                    CandidateStatus::DirectoryEvidence
                }
                EvidenceRootKind::DesktopEntry | EvidenceRootKind::GuiExecutable
                    if metadata.is_file() =>
                {
                    CandidateStatus::InstallationArtifactEvidence
                }
                _ => {
                    diagnostics.push(format!(
                        "ignored matching Linux GUI/installation artifact with the wrong file \
                         kind={}",
                        path.to_string_lossy()
                    ));
                    continue;
                }
            };
            matches = matches.saturating_add(1);
            push_evidence(candidates, path.clone(), status);
            diagnostics.push(format!(
                "Linux GUI/installation evidence={} (evidence only; not a runnable CLI)",
                path.to_string_lossy()
            ));
        }
        if matches == 0 {
            diagnostics.push(format!(
                "known Linux GUI/installation root scanned with no matching artifact={}",
                root.path.to_string_lossy()
            ));
        }
    }
    candidates.sort_by(|left, right| left.path.cmp(&right.path));
}

fn installation_name_matches(target: DiscoveryTarget, kind: EvidenceRootKind, path: &Path) -> bool {
    let Some(file_name) = path.file_name() else {
        return false;
    };
    let normalized: String = file_name
        .to_string_lossy()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .flat_map(char::to_lowercase)
        .collect();
    let agent_matches = match target {
        DiscoveryTarget::ClaudeCli => normalized.contains("claude"),
        DiscoveryTarget::OpenCode => normalized.contains("opencode"),
        DiscoveryTarget::Codex | DiscoveryTarget::ClaudeAcp | DiscoveryTarget::Node => false,
    };
    if !agent_matches {
        return false;
    }
    match kind {
        EvidenceRootKind::NamedDirectory => true,
        EvidenceRootKind::DesktopEntry => path
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("desktop")),
        EvidenceRootKind::GuiExecutable => match target {
            DiscoveryTarget::ClaudeCli => normalized.contains("claudedesktop"),
            DiscoveryTarget::OpenCode => {
                normalized.contains("opencodedesktop")
                    || path
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("appimage"))
            }
            DiscoveryTarget::Codex | DiscoveryTarget::ClaudeAcp | DiscoveryTarget::Node => false,
        },
    }
}

fn push_evidence(candidates: &mut Vec<Candidate>, path: PathBuf, status: CandidateStatus) {
    if candidates.iter().all(|candidate| candidate.path != path) {
        candidates.push(Candidate { path, status });
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use tempfile::tempdir;

    use super::{
        append_installation_evidence, extend_known_executable_directories, installation_roots,
        scan_installation_roots, EvidenceRoot, EvidenceRootKind,
    };
    use crate::platform::{Candidate, CandidateStatus, DiscoveryTarget};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn known_executable_directories_cover_fhs_snap_and_linuxbrew() {
        let mut directories = Vec::new();
        let data = PathBuf::from("test-data");

        extend_known_executable_directories(&mut directories);

        assert_eq!(
            directories,
            [
                PathBuf::from("/usr/local/bin"),
                PathBuf::from("/usr/bin"),
                PathBuf::from("/snap/bin"),
                PathBuf::from("/home/linuxbrew/.linuxbrew/bin"),
            ]
        );
        let roots = installation_roots(&data, &directories);
        for (path, kind) in [
            (PathBuf::from("/opt"), EvidenceRootKind::NamedDirectory),
            (data.join("applications"), EvidenceRootKind::DesktopEntry),
            (
                PathBuf::from("/usr/local/share/applications"),
                EvidenceRootKind::DesktopEntry,
            ),
            (
                PathBuf::from("/usr/share/applications"),
                EvidenceRootKind::DesktopEntry,
            ),
            (
                PathBuf::from("/usr/local/bin"),
                EvidenceRootKind::GuiExecutable,
            ),
        ] {
            assert!(roots
                .iter()
                .any(|root| root.path == path && root.kind == kind));
        }
    }

    #[test]
    fn desktop_package_and_appimage_artifacts_are_non_runnable_evidence() -> TestResult {
        let temporary = tempdir()?;
        let opt = temporary.path().join("opt");
        let applications = temporary.path().join("applications");
        let bin = temporary.path().join("bin");
        fs::create_dir(&opt)?;
        fs::create_dir(&applications)?;
        fs::create_dir(&bin)?;
        for directory in ["Claude", "OpenCode", "Codex"] {
            fs::create_dir(opt.join(directory))?;
        }
        for entry in [
            "claude-desktop.desktop",
            "opencode.desktop",
            "codex.desktop",
        ] {
            fs::write(applications.join(entry), b"[Desktop Entry]\n")?;
        }
        for executable in [
            "claude-desktop",
            "opencode-desktop",
            "OpenCode-x86_64.AppImage",
            "codex-desktop",
        ] {
            fs::write(bin.join(executable), b"evidence only")?;
        }
        let roots = [
            EvidenceRoot {
                path: opt,
                kind: EvidenceRootKind::NamedDirectory,
            },
            EvidenceRoot {
                path: applications,
                kind: EvidenceRootKind::DesktopEntry,
            },
            EvidenceRoot {
                path: bin,
                kind: EvidenceRootKind::GuiExecutable,
            },
        ];

        let claude = evidence_for(DiscoveryTarget::ClaudeCli, &roots);
        assert_eq!(
            statuses(&claude),
            [
                CandidateStatus::InstallationArtifactEvidence,
                CandidateStatus::InstallationArtifactEvidence,
                CandidateStatus::DirectoryEvidence,
            ]
        );
        assert!(claude
            .iter()
            .any(|candidate| candidate.path.ends_with("claude-desktop")));

        let opencode = evidence_for(DiscoveryTarget::OpenCode, &roots);
        assert_eq!(
            statuses(&opencode),
            [
                CandidateStatus::InstallationArtifactEvidence,
                CandidateStatus::InstallationArtifactEvidence,
                CandidateStatus::InstallationArtifactEvidence,
                CandidateStatus::DirectoryEvidence,
            ]
        );
        assert!(opencode
            .iter()
            .any(|candidate| candidate.path.ends_with("OpenCode-x86_64.AppImage")));
        Ok(())
    }

    #[test]
    fn undocumented_linux_codex_gui_is_not_inferred_from_matching_names() -> TestResult {
        let temporary = tempdir()?;
        let opt = temporary.path().join("opt");
        fs::create_dir(&opt)?;
        fs::create_dir(opt.join("Codex"))?;
        let roots = [EvidenceRoot {
            path: opt,
            kind: EvidenceRootKind::NamedDirectory,
        }];
        let direct_candidates = evidence_for(DiscoveryTarget::Codex, &roots);
        let mut candidates = Vec::new();
        let mut diagnostics = Vec::new();

        append_installation_evidence(
            DiscoveryTarget::Codex,
            temporary.path(),
            &[],
            &mut candidates,
            &mut diagnostics,
        );

        assert!(direct_candidates.is_empty());
        assert!(candidates.is_empty());
        assert!(diagnostics
            .iter()
            .any(|diagnostic| diagnostic.contains("no documented Linux GUI distribution")));
        Ok(())
    }

    #[test]
    fn wrong_kind_or_inaccessible_root_never_becomes_evidence() -> TestResult {
        let temporary = tempdir()?;
        let root_file = temporary.path().join("not-a-directory");
        fs::write(&root_file, b"not a directory")?;
        let applications = temporary.path().join("applications");
        fs::create_dir(&applications)?;
        fs::create_dir(applications.join("claude-desktop.desktop"))?;
        let roots = [
            EvidenceRoot {
                path: root_file,
                kind: EvidenceRootKind::DesktopEntry,
            },
            EvidenceRoot {
                path: applications,
                kind: EvidenceRootKind::DesktopEntry,
            },
        ];
        let mut candidates = Vec::new();
        let mut diagnostics = Vec::new();

        scan_installation_roots(
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
            .any(|diagnostic| diagnostic.contains("wrong file kind")));
        Ok(())
    }

    fn evidence_for(target: DiscoveryTarget, roots: &[EvidenceRoot]) -> Vec<Candidate> {
        let mut candidates = Vec::new();
        let mut diagnostics = Vec::new();
        scan_installation_roots(target, roots, &mut candidates, &mut diagnostics);
        candidates
    }

    fn statuses(candidates: &[Candidate]) -> Vec<CandidateStatus> {
        candidates
            .iter()
            .map(|candidate| candidate.status.clone())
            .collect()
    }
}
