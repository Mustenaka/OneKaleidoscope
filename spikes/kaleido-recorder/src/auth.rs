use std::env;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

use directories::BaseDirs;
use serde_json::Value;

use crate::platform::{self, DiscoveryTarget, ResolvedExecutable, RuntimeProbe};
use crate::redact::opencode_credential_environment_names;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AuthenticationState {
    Authenticated,
    Unauthenticated,
    CredentialSourceObserved,
    NotApplicable,
    Inconclusive,
}

impl std::fmt::Display for AuthenticationState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Authenticated => formatter.write_str("authenticated"),
            Self::Unauthenticated => formatter.write_str("not-authenticated"),
            Self::CredentialSourceObserved => {
                formatter.write_str("credential-source-observed-not-validated")
            }
            Self::NotApplicable => formatter.write_str("not-applicable"),
            Self::Inconclusive => formatter.write_str("inconclusive"),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthenticationReport {
    pub state: AuthenticationState,
    pub evidence: &'static str,
}

impl AuthenticationReport {
    const fn new(state: AuthenticationState, evidence: &'static str) -> Self {
        Self { state, evidence }
    }
}

pub fn probe(
    target: DiscoveryTarget,
    executable: Option<&ResolvedExecutable>,
    bundled_claude: Option<&ResolvedExecutable>,
    timeout: Duration,
) -> AuthenticationReport {
    match target {
        DiscoveryTarget::Codex => executable.map_or_else(
            || {
                AuthenticationReport::new(
                    AuthenticationState::Inconclusive,
                    "no runnable Codex candidate was available for `login status`",
                )
            },
            |executable| probe_codex(executable, timeout),
        ),
        DiscoveryTarget::ClaudeAcp => bundled_claude.map_or_else(
            || {
                AuthenticationReport::new(
                    AuthenticationState::Inconclusive,
                    "the hostd-bundled Claude Code binary was not configured",
                )
            },
            |executable| probe_claude(executable, timeout),
        ),
        DiscoveryTarget::ClaudeCli => executable.map_or_else(
            || {
                AuthenticationReport::new(
                    AuthenticationState::Inconclusive,
                    "no runnable Claude CLI candidate was available for `auth status --json`",
                )
            },
            |executable| probe_claude(executable, timeout),
        ),
        DiscoveryTarget::OpenCode => {
            if executable.is_none() {
                AuthenticationReport::new(
                    AuthenticationState::Inconclusive,
                    "no runnable OpenCode candidate was available",
                )
            } else {
                probe_opencode()
            }
        }
        DiscoveryTarget::Node => AuthenticationReport::new(
            AuthenticationState::NotApplicable,
            "Node.js is a runtime prerequisite and has no agent login",
        ),
    }
}

fn probe_codex(executable: &ResolvedExecutable, timeout: Duration) -> AuthenticationReport {
    let arguments = [OsString::from("login"), OsString::from("status")];
    match platform::probe_command(executable, &arguments, timeout) {
        RuntimeProbe::Runnable { .. } => AuthenticationReport::new(
            AuthenticationState::Authenticated,
            "`codex login status` exited successfully",
        ),
        RuntimeProbe::NonZero { stdout, stderr, .. }
            if reports_not_logged_in(&stdout) || reports_not_logged_in(&stderr) =>
        {
            AuthenticationReport::new(
                AuthenticationState::Unauthenticated,
                "`codex login status` returned its explicit not-logged-in result",
            )
        }
        RuntimeProbe::NonZero { .. } => AuthenticationReport::new(
            AuthenticationState::Inconclusive,
            "`codex login status` failed without a recognized authentication result",
        ),
        RuntimeProbe::SpawnFailed(_) => AuthenticationReport::new(
            AuthenticationState::Inconclusive,
            "`codex login status` could not be started",
        ),
        RuntimeProbe::TimedOut => AuthenticationReport::new(
            AuthenticationState::Inconclusive,
            "`codex login status` timed out",
        ),
        RuntimeProbe::NotResolved => AuthenticationReport::new(
            AuthenticationState::Inconclusive,
            "Codex executable was not resolved",
        ),
    }
}

fn reports_not_logged_in(value: &str) -> bool {
    value.trim().eq_ignore_ascii_case("not logged in")
}

fn probe_claude(executable: &ResolvedExecutable, timeout: Duration) -> AuthenticationReport {
    let arguments = [
        OsString::from("auth"),
        OsString::from("status"),
        OsString::from("--json"),
    ];
    match platform::probe_command(executable, &arguments, timeout) {
        RuntimeProbe::Runnable { stdout, .. } => claude_json_report(&stdout),
        RuntimeProbe::NonZero { .. } => AuthenticationReport::new(
            AuthenticationState::Inconclusive,
            "`claude auth status --json` exited unsuccessfully",
        ),
        RuntimeProbe::SpawnFailed(_) => AuthenticationReport::new(
            AuthenticationState::Inconclusive,
            "`claude auth status --json` could not be started",
        ),
        RuntimeProbe::TimedOut => AuthenticationReport::new(
            AuthenticationState::Inconclusive,
            "`claude auth status --json` timed out",
        ),
        RuntimeProbe::NotResolved => AuthenticationReport::new(
            AuthenticationState::Inconclusive,
            "Claude executable was not resolved",
        ),
    }
}

fn claude_json_report(stdout: &str) -> AuthenticationReport {
    let parsed = serde_json::from_str::<Value>(stdout);
    match parsed
        .as_ref()
        .ok()
        .and_then(|value| value.get("loggedIn"))
        .and_then(Value::as_bool)
    {
        Some(true) => AuthenticationReport::new(
            AuthenticationState::Authenticated,
            "`claude auth status --json` reported loggedIn=true",
        ),
        Some(false) => AuthenticationReport::new(
            AuthenticationState::Unauthenticated,
            "`claude auth status --json` reported loggedIn=false",
        ),
        None => AuthenticationReport::new(
            AuthenticationState::Inconclusive,
            "`claude auth status --json` did not return the documented loggedIn boolean",
        ),
    }
}

fn probe_opencode() -> AuthenticationReport {
    if opencode_environment_credential_present() {
        return AuthenticationReport::new(
            AuthenticationState::CredentialSourceObserved,
            "a supported OpenCode provider credential environment variable is present and non-empty; its contents were not logged",
        );
    }
    match opencode_stored_credential_present() {
        Ok(true) => AuthenticationReport::new(
            AuthenticationState::CredentialSourceObserved,
            "OpenCode auth storage contains at least one provider entry; credential values were not logged",
        ),
        Ok(false) => AuthenticationReport::new(
            AuthenticationState::Unauthenticated,
            "no supported OpenCode provider environment variable or OpenCode auth entry was observed",
        ),
        Err(()) => AuthenticationReport::new(
            AuthenticationState::Inconclusive,
            "OpenCode auth storage existed but could not be safely classified",
        ),
    }
}

fn opencode_environment_credential_present() -> bool {
    opencode_environment_credential_present_with(|name| env::var_os(name))
}

fn opencode_environment_credential_present_with(
    mut lookup: impl FnMut(&str) -> Option<OsString>,
) -> bool {
    opencode_credential_environment_names()
        .any(|name| lookup(name).is_some_and(|value| !value.is_empty()))
}

fn opencode_stored_credential_present() -> Result<bool, ()> {
    let mut paths = Vec::new();
    if let Some(data) = env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
        if data.is_absolute() {
            paths.push(data.join("opencode").join("auth.json"));
        }
    }
    if let Some(base) = BaseDirs::new() {
        paths.push(base.data_dir().join("opencode").join("auth.json"));
        paths.push(base.data_local_dir().join("opencode").join("auth.json"));
    }
    paths.sort();
    paths.dedup();
    for path in paths {
        let contents = match fs::read(&path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => return Err(()),
        };
        let value = serde_json::from_slice::<Value>(&contents).map_err(|_| ())?;
        if value
            .as_object()
            .is_some_and(|providers| !providers.is_empty())
        {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;

    use super::{
        claude_json_report, opencode_environment_credential_present_with, reports_not_logged_in,
        AuthenticationState,
    };

    #[test]
    fn codex_not_logged_in_message_is_recognized_without_fuzzy_terminal_parsing() {
        assert!(reports_not_logged_in("Not logged in\n"));
        assert!(!reports_not_logged_in("network failure"));
    }

    #[test]
    fn claude_auth_json_requires_the_documented_boolean() {
        assert_eq!(
            claude_json_report(r#"{"loggedIn":true,"email":"private@example.invalid"}"#).state,
            AuthenticationState::Authenticated
        );
        assert_eq!(
            claude_json_report(r#"{"loggedIn":false}"#).state,
            AuthenticationState::Unauthenticated
        );
        assert_eq!(
            claude_json_report(r#"{"authMethod":"claude.ai"}"#).state,
            AuthenticationState::Inconclusive
        );
    }

    #[test]
    fn opencode_environment_probe_uses_only_nonempty_provider_credentials() {
        assert!(!opencode_environment_credential_present_with(
            |name| match name {
                "CODEX_API_KEY" | "CLAUDE_CODE_OAUTH_TOKEN" => {
                    Some(OsString::from("fixture-agent-only-credential"))
                }
                _ => None,
            }
        ));
        assert!(opencode_environment_credential_present_with(
            |name| match name {
                "OPENAI_API_KEY" => Some(OsString::from("fixture-provider-credential")),
                _ => None,
            }
        ));
        assert!(!opencode_environment_credential_present_with(
            |name| match name {
                "OPENAI_API_KEY" => Some(OsString::new()),
                _ => None,
            }
        ));
    }
}
