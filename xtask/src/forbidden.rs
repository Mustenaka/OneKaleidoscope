use std::ffi::OsStr;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Violation {
    pub path: PathBuf,
    pub line: usize,
    pub pattern: String,
}

impl fmt::Display for Violation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{}:{}: {}",
            self.path.to_string_lossy().replace('\\', "/"),
            self.line,
            self.pattern
        )
    }
}

#[derive(Debug)]
struct ForbiddenPattern {
    needle: Vec<u8>,
    label: String,
}

pub fn scan_repository(root: &Path) -> io::Result<Vec<Violation>> {
    let patterns = forbidden_patterns();
    let mut violations = Vec::new();

    for scope in ["crates", "spikes", "xtask"] {
        let scope_path = root.join(scope);
        match fs::metadata(&scope_path) {
            Ok(metadata) if metadata.is_dir() => {
                visit_directory(root, &scope_path, &patterns, &mut violations)?;
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }

    violations.sort();
    violations.dedup();
    Ok(violations)
}

fn visit_directory(
    root: &Path,
    directory: &Path,
    patterns: &[ForbiddenPattern],
    violations: &mut Vec<Violation>,
) -> io::Result<()> {
    let mut entries = fs::read_dir(directory)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(fs::DirEntry::file_name);

    for entry in entries {
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }

        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|_| io::Error::other("scanner path escaped repository root"))?;
        if excluded(relative) {
            continue;
        }

        if file_type.is_dir() {
            visit_directory(root, &path, patterns, violations)?;
        } else if file_type.is_file() && path.extension().is_some_and(|extension| extension == "rs")
        {
            scan_file(relative, &path, patterns, violations)?;
        }
    }

    Ok(())
}

fn excluded(relative: &Path) -> bool {
    let mut previous_was_tests = false;

    for component in relative.components() {
        let Component::Normal(name) = component else {
            previous_was_tests = false;
            continue;
        };

        if name == OsStr::new("target") || name == OsStr::new("schemas") {
            return true;
        }
        if previous_was_tests && name == OsStr::new("fixtures") {
            return true;
        }
        previous_was_tests = name == OsStr::new("tests");
    }

    false
}

fn scan_file(
    relative: &Path,
    absolute: &Path,
    patterns: &[ForbiddenPattern],
    violations: &mut Vec<Violation>,
) -> io::Result<()> {
    let contents = fs::read(absolute)?;

    for pattern in patterns {
        if pattern.needle.is_empty() {
            continue;
        }
        for (offset, window) in contents.windows(pattern.needle.len()).enumerate() {
            if window == pattern.needle {
                let line = contents
                    .iter()
                    .take(offset)
                    .filter(|byte| **byte == b'\n')
                    .count()
                    + 1;
                violations.push(Violation {
                    path: relative.to_path_buf(),
                    line,
                    pattern: pattern.label.clone(),
                });
            }
        }
    }

    Ok(())
}

fn forbidden_patterns() -> Vec<ForbiddenPattern> {
    let escaped_hex = ["\\x1", "b["].concat();

    vec![
        text_pattern(&["to", "do!"]),
        text_pattern(&["unimple", "mented!"]),
        text_pattern(&["// TO", "DO"]),
        text_pattern(&["// FIX", "ME"]),
        text_pattern(&["#[ig", "nore]"]),
        ForbiddenPattern {
            needle: vec![0x1b, b'['],
            label: escaped_hex.clone(),
        },
        ForbiddenPattern {
            needle: escaped_hex.as_bytes().to_vec(),
            label: escaped_hex,
        },
        text_pattern(&["\\u{1", "b}["]),
        text_pattern(&["strip_", "ansi"]),
        text_pattern(&["v", "te"]),
        text_pattern(&["ansi_", "parser"]),
    ]
}

fn text_pattern(parts: &[&str]) -> ForbiddenPattern {
    let label = parts.concat();
    ForbiddenPattern {
        needle: label.as_bytes().to_vec(),
        label,
    }
}
