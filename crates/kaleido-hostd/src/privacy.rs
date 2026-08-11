//! Product diagnostic redaction for remote paths.
//!
//! Callers canonicalize at their filesystem boundary, then retain only this
//! closed class. No relative path or basename is ever returned.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafePathClass {
    Sandbox,
    Home,
    Other,
}

impl SafePathClass {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Sandbox => "<SANDBOX>",
            Self::Home => "<HOME>",
            Self::Other => "<PATH>",
        }
    }
}

#[derive(Clone)]
pub struct SafePathClassifier {
    canonical_home: PathBuf,
    canonical_sandbox: PathBuf,
}

impl std::fmt::Debug for SafePathClassifier {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SafePathClassifier")
            .field("canonical_home", &"<HOME>")
            .field("canonical_sandbox", &"<SANDBOX>")
            .finish()
    }
}

impl SafePathClassifier {
    /// Both roots and every classified input must already be canonicalized by
    /// the owning filesystem boundary. Relative inputs deliberately collapse
    /// to `<PATH>` rather than acquiring ambient-current-directory meaning.
    pub fn new(canonical_home: PathBuf, canonical_sandbox: PathBuf) -> Self {
        Self {
            canonical_home,
            canonical_sandbox,
        }
    }

    pub fn classify(&self, canonical_path: &Path) -> SafePathClass {
        if !canonical_path.is_absolute() {
            return SafePathClass::Other;
        }
        if canonical_path.starts_with(&self.canonical_sandbox) {
            SafePathClass::Sandbox
        } else if canonical_path.starts_with(&self.canonical_home) {
            SafePathClass::Home
        } else {
            SafePathClass::Other
        }
    }

    pub fn label(&self, canonical_path: &Path) -> &'static str {
        self.classify(canonical_path).label()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{SafePathClass, SafePathClassifier};

    #[cfg(windows)]
    fn roots() -> (PathBuf, PathBuf, PathBuf) {
        let home = PathBuf::from(r"C:\Users\private-user");
        let sandbox = home.join(r"work\secret-project");
        let outside = PathBuf::from(r"D:\shared\outside.txt");
        (home, sandbox, outside)
    }

    #[cfg(not(windows))]
    fn roots() -> (PathBuf, PathBuf, PathBuf) {
        let home = PathBuf::from("/home/private-user");
        let sandbox = home.join("work/secret-project");
        let outside = PathBuf::from("/srv/shared/outside.txt");
        (home, sandbox, outside)
    }

    #[test]
    fn the_more_specific_sandbox_wins_without_leaking_any_path_component() {
        let (home, sandbox, outside) = roots();
        let classifier = SafePathClassifier::new(home.clone(), sandbox.clone());

        assert_eq!(
            classifier.classify(&sandbox.join("src/private-file.rs")),
            SafePathClass::Sandbox
        );
        assert_eq!(
            classifier.classify(&home.join("sibling/private-note.txt")),
            SafePathClass::Home
        );
        assert_eq!(classifier.classify(&outside), SafePathClass::Other);
        assert_eq!(
            classifier.classify(&PathBuf::from("relative.txt")),
            SafePathClass::Other
        );

        let emitted = format!(
            "{} {} {} {classifier:?}",
            classifier.label(&sandbox.join("src/private-file.rs")),
            classifier.label(&home.join("sibling/private-note.txt")),
            classifier.label(&outside),
        );
        assert_eq!(
            emitted,
            "<SANDBOX> <HOME> <PATH> SafePathClassifier { canonical_home: \"<HOME>\", canonical_sandbox: \"<SANDBOX>\" }"
        );
        for forbidden in [
            "private-user",
            "secret-project",
            "private-file",
            "outside.txt",
        ] {
            assert!(!emitted.contains(forbidden));
        }
    }
}
