use std::env;
use std::ffi::OsString;
use std::fmt;
use std::path::Path;

use directories::BaseDirs;
use serde_json::Value;

const REDACTED_TOKEN: &str = "<REDACTED_TOKEN>";
const OUTSIDE_PATH: &str = "<OUTSIDE_PATH>";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SupportedSecretEnvironment {
    name: &'static str,
    opencode_provider: bool,
}

pub(crate) const SUPPORTED_SECRET_ENVIRONMENTS: &[SupportedSecretEnvironment] = &[
    secret_environment("ANTHROPIC_API_KEY", true),
    secret_environment("ANTHROPIC_AUTH_TOKEN", false),
    secret_environment("ANTHROPIC_FOUNDRY_API_KEY", false),
    secret_environment("AWS_ACCESS_KEY_ID", false),
    secret_environment("AWS_BEARER_TOKEN_BEDROCK", false),
    secret_environment("AWS_SECRET_ACCESS_KEY", false),
    secret_environment("AWS_SESSION_TOKEN", false),
    secret_environment("AZURE_OPENAI_AD_TOKEN", false),
    secret_environment("AZURE_OPENAI_API_KEY", true),
    secret_environment("CLAUDE_CODE_API_KEY", false),
    secret_environment("CLAUDE_CODE_OAUTH_TOKEN", false),
    secret_environment("CODEX_API_KEY", false),
    secret_environment("GEMINI_API_KEY", true),
    secret_environment("GH_TOKEN", false),
    secret_environment("GITHUB_TOKEN", false),
    secret_environment("GOOGLE_API_KEY", false),
    secret_environment("GOOGLE_GENERATIVE_AI_API_KEY", true),
    secret_environment("OPENAI_API_KEY", true),
    secret_environment("OPENROUTER_API_KEY", true),
    secret_environment("OPENCODE_API_KEY", false),
];

const fn secret_environment(
    name: &'static str,
    opencode_provider: bool,
) -> SupportedSecretEnvironment {
    SupportedSecretEnvironment {
        name,
        opencode_provider,
    }
}

pub(crate) fn supported_secret_environment_names() -> impl Iterator<Item = &'static str> {
    SUPPORTED_SECRET_ENVIRONMENTS
        .iter()
        .map(|variable| variable.name)
}

pub(crate) fn opencode_credential_environment_names() -> impl Iterator<Item = &'static str> {
    SUPPORTED_SECRET_ENVIRONMENTS
        .iter()
        .filter(|variable| variable.opencode_provider)
        .map(|variable| variable.name)
}

#[derive(Clone, Eq, PartialEq)]
pub struct Redactor {
    replacements: Vec<(String, &'static str)>,
    secret_replacements: Vec<String>,
    sandbox_variants: Vec<String>,
}

impl fmt::Debug for Redactor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Redactor")
            .field("replacement_count", &self.replacements.len())
            .field("secret_replacement_count", &self.secret_replacements.len())
            .field("sandbox_variant_count", &self.sandbox_variants.len())
            .finish()
    }
}

impl Redactor {
    pub fn for_environment(sandbox: &Path) -> Self {
        Self::for_environment_with_secret_lookup(sandbox, |name| env::var_os(name))
    }

    fn for_environment_with_secret_lookup(
        sandbox: &Path,
        secret_lookup: impl FnMut(&str) -> Option<OsString>,
    ) -> Self {
        let mut replacements = Vec::new();
        if let Some(base) = BaseDirs::new() {
            add_path_replacements(&mut replacements, base.home_dir(), "<HOME>");
        }
        if let Some(user) = env::var_os("USERNAME").or_else(|| env::var_os("USER")) {
            let user = user.to_string_lossy().into_owned();
            if !user.is_empty() {
                replacements.push((user, "<USER>"));
            }
        }
        let sandbox_variants = path_variants(sandbox);
        add_path_variant_replacements(&mut replacements, &sandbox_variants, "<SANDBOX>");
        replacements.sort_by(|left, right| right.0.len().cmp(&left.0.len()));
        replacements.dedup_by(|left, right| left.0 == right.0);
        let secret_replacements = secret_environment_values(secret_lookup);
        Self {
            replacements,
            secret_replacements,
            sandbox_variants,
        }
    }

    pub fn from_pairs(pairs: impl IntoIterator<Item = (String, &'static str)>) -> Self {
        let mut replacements: Vec<_> = pairs.into_iter().collect();
        let sandbox_variants = replacements
            .iter()
            .filter(|(_, replacement)| *replacement == "<SANDBOX>")
            .map(|(sensitive, _)| sensitive.clone())
            .collect();
        replacements.sort_by(|left, right| right.0.len().cmp(&left.0.len()));
        replacements.dedup_by(|left, right| left.0 == right.0);
        Self {
            replacements,
            secret_replacements: Vec::new(),
            sandbox_variants,
        }
    }

    pub fn redact(&self, input: &str) -> String {
        let mut redacted = redact_sandbox_traversals(input, &self.sandbox_variants);
        for sensitive in &self.secret_replacements {
            redacted = if sensitive.len() < 8 {
                redact_exact_json_string_value(&redacted, sensitive, REDACTED_TOKEN)
            } else {
                redacted.replace(sensitive, REDACTED_TOKEN)
            };
        }
        for (sensitive, replacement) in &self.replacements {
            redacted = if *replacement == "<USER>" {
                replace_ascii_case_insensitive_except_json_keys(&redacted, sensitive, replacement)
            } else {
                replace_ascii_case_insensitive(&redacted, sensitive, replacement)
            };
        }
        redacted = redact_prefixed_value(&redacted, "sk-", REDACTED_TOKEN);
        redacted = redact_prefixed_value(&redacted, "ghp_", REDACTED_TOKEN);
        redacted = redact_prefixed_value(&redacted, "Bearer ", REDACTED_TOKEN);
        redacted = redact_json_string_field(&redacted, "api_key", REDACTED_TOKEN);
        redacted = redact_json_string_field(&redacted, "authorization", REDACTED_TOKEN);
        redact_absolute_path_strings(&redacted)
    }
}

fn secret_environment_values(lookup: impl FnMut(&str) -> Option<OsString>) -> Vec<String> {
    let mut values = supported_secret_environment_names()
        .filter_map(lookup)
        .filter(|value| !value.is_empty())
        // Fixture payloads are UTF-8. A non-Unicode environment value cannot
        // occur verbatim in `input`, so only exact Unicode values are candidates.
        .filter_map(|value| value.into_string().ok())
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    values.sort_by(|left, right| right.len().cmp(&left.len()).then_with(|| left.cmp(right)));
    values.dedup();
    values
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LeakKind {
    Username,
    HomePath,
    SecretPrefix(&'static str),
    SensitiveField(&'static str),
    OutsideSandboxPath(String),
}

impl fmt::Display for LeakKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Username => formatter.write_str("current username"),
            Self::HomePath => formatter.write_str("home directory path"),
            Self::SecretPrefix(prefix) => write!(formatter, "secret prefix `{prefix}`"),
            Self::SensitiveField(field) => write!(formatter, "sensitive field `{field}`"),
            Self::OutsideSandboxPath(field) => {
                write!(
                    formatter,
                    "absolute path outside sandbox in field `{field}`"
                )
            }
        }
    }
}

pub fn detect_leaks(raw: &str, value: &Value) -> Vec<LeakKind> {
    let mut leaks = Vec::new();
    let lower = raw.to_ascii_lowercase();
    if let Some(user) = env::var_os("USERNAME").or_else(|| env::var_os("USER")) {
        let user = user.to_string_lossy();
        if !user.is_empty() && value_contains_ascii_case_insensitive(value, &user) {
            leaks.push(LeakKind::Username);
        }
    }
    if let Some(base) = BaseDirs::new() {
        let variants = path_variants(base.home_dir());
        if variants
            .iter()
            .any(|variant| lower.contains(&variant.to_ascii_lowercase()))
        {
            leaks.push(LeakKind::HomePath);
        }
    }
    for (needle, kind) in [
        ("sk-", LeakKind::SecretPrefix("sk-")),
        ("ghp_", LeakKind::SecretPrefix("ghp_")),
        ("bearer ", LeakKind::SecretPrefix("Bearer ")),
    ] {
        if contains_prefixed_value(raw, needle) {
            leaks.push(kind);
        }
    }
    detect_sensitive_fields(value, &mut leaks);
    detect_absolute_paths(value, None, false, &mut leaks);
    leaks.sort_by_key(ToString::to_string);
    leaks.dedup();
    leaks
}

fn add_path_replacements(
    replacements: &mut Vec<(String, &'static str)>,
    path: &Path,
    replacement: &'static str,
) {
    add_path_variant_replacements(replacements, &path_variants(path), replacement);
}

fn add_path_variant_replacements(
    replacements: &mut Vec<(String, &'static str)>,
    variants: &[String],
    replacement: &'static str,
) {
    for variant in variants {
        if !variant.is_empty() {
            replacements.push((variant.clone(), replacement));
        }
    }
}

fn path_variants(path: &Path) -> Vec<String> {
    let native = path.to_string_lossy().into_owned();
    let mut bases = vec![native.clone()];
    if let Some(unc) = native.strip_prefix(r"\\?\UNC\") {
        bases.push(format!(r"\\{unc}"));
    } else if let Some(non_verbatim) = native.strip_prefix(r"\\?\") {
        bases.push(non_verbatim.to_owned());
    }

    let mut variants = Vec::new();
    for base in bases {
        let forward = base.replace('\\', "/");
        let backward = base.replace('/', "\\");
        let escaped_backward = backward.replace('\\', "\\\\");
        variants.extend([base, forward, backward, escaped_backward]);
    }
    variants.sort();
    variants.dedup();
    variants
}

fn replace_ascii_case_insensitive(input: &str, needle: &str, replacement: &str) -> String {
    if needle.is_empty() {
        return input.to_owned();
    }
    if !needle.is_ascii() {
        return input.replace(needle, replacement);
    }

    let lower_input = input.to_ascii_lowercase();
    let lower_needle = needle.to_ascii_lowercase();
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(relative) = lower_input[cursor..].find(&lower_needle) {
        let start = cursor + relative;
        let end = start + needle.len();
        output.push_str(&input[cursor..start]);
        output.push_str(replacement);
        cursor = end;
    }
    output.push_str(&input[cursor..]);
    output
}

fn replace_ascii_case_insensitive_except_json_keys(
    input: &str,
    needle: &str,
    replacement: &str,
) -> String {
    if needle.is_empty() {
        return input.to_owned();
    }
    let lower_input = input.to_ascii_lowercase();
    let lower_needle = needle.to_ascii_lowercase();
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(relative) = lower_input[cursor..].find(&lower_needle) {
        let start = cursor + relative;
        let end = start + needle.len();
        output.push_str(&input[cursor..start]);
        if json_string_containing_offset_is_key(input, start) {
            output.push_str(&input[start..end]);
        } else {
            output.push_str(replacement);
        }
        cursor = end;
    }
    output.push_str(&input[cursor..]);
    output
}

fn json_string_containing_offset_is_key(input: &str, offset: usize) -> bool {
    let bytes = input.as_bytes();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes.get(cursor) != Some(&b'"') {
            cursor += 1;
            continue;
        }
        let Some(relative_end) = input.get(cursor + 1..).and_then(find_json_string_end) else {
            return false;
        };
        let end = cursor + 1 + relative_end;
        if cursor < offset && offset < end {
            let colon = skip_ascii_whitespace(bytes, end + 1);
            return bytes.get(colon) == Some(&b':');
        }
        cursor = end + 1;
    }
    false
}

fn redact_exact_json_string_value(input: &str, sensitive: &str, replacement: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(relative) = input[cursor..].find('"') {
        let start = cursor + relative;
        output.push_str(&input[cursor..start]);
        let Some(relative_end) = input.get(start + 1..).and_then(find_json_string_end) else {
            output.push_str(&input[start..]);
            return output;
        };
        let end = start + 1 + relative_end;
        let token = &input[start..=end];
        let decoded = serde_json::from_str::<String>(token);
        let colon = skip_ascii_whitespace(input.as_bytes(), end + 1);
        if decoded.as_deref().is_ok_and(|value| value == sensitive)
            && input.as_bytes().get(colon) != Some(&b':')
        {
            match serde_json::to_string(replacement) {
                Ok(encoded) => output.push_str(&encoded),
                Err(_) => output.push_str(token),
            }
        } else {
            output.push_str(token);
        }
        cursor = end + 1;
    }
    output.push_str(&input[cursor..]);
    output
}

fn redact_prefixed_value(input: &str, prefix: &str, replacement: &str) -> String {
    let lower_input = input.to_ascii_lowercase();
    let lower_prefix = prefix.to_ascii_lowercase();
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(relative) = lower_input[cursor..].find(&lower_prefix) {
        let start = cursor + relative;
        let embedded_in_word = input[..start].chars().next_back().is_some_and(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
        });
        if embedded_in_word {
            let prefix_end = start + prefix.len();
            output.push_str(&input[cursor..prefix_end]);
            cursor = prefix_end;
            continue;
        }
        output.push_str(&input[cursor..start]);
        let mut end = start + prefix.len();
        for character in input[end..].chars() {
            if character.is_ascii_alphanumeric()
                || matches!(character, '-' | '_' | '.' | '+' | '/' | '=' | '~')
            {
                end += character.len_utf8();
            } else {
                break;
            }
        }
        output.push_str(replacement);
        cursor = end;
    }
    output.push_str(&input[cursor..]);
    output
}

fn contains_prefixed_value(input: &str, prefix: &str) -> bool {
    let lower_input = input.to_ascii_lowercase();
    let lower_prefix = prefix.to_ascii_lowercase();
    let mut cursor = 0;
    while let Some(relative) = lower_input[cursor..].find(&lower_prefix) {
        let start = cursor + relative;
        let embedded_in_word = input[..start].chars().next_back().is_some_and(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '_' | '-')
        });
        if !embedded_in_word {
            return true;
        }
        cursor = start + prefix.len();
    }
    false
}

fn value_contains_ascii_case_insensitive(value: &Value, needle: &str) -> bool {
    match value {
        Value::String(text) => text
            .to_ascii_lowercase()
            .contains(&needle.to_ascii_lowercase()),
        Value::Array(items) => items
            .iter()
            .any(|item| value_contains_ascii_case_insensitive(item, needle)),
        Value::Object(map) => map
            .values()
            .any(|item| value_contains_ascii_case_insensitive(item, needle)),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

fn redact_json_string_field(input: &str, field: &str, replacement: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let needle = format!("\"{}\"", field.to_ascii_lowercase());
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(relative) = lower[cursor..].find(&needle) {
        let field_start = cursor + relative;
        let after_field = field_start + needle.len();
        output.push_str(&input[cursor..after_field]);
        let colon = skip_ascii_whitespace(input.as_bytes(), after_field);
        if input.as_bytes().get(colon) != Some(&b':') {
            cursor = after_field;
            continue;
        }
        let quote = skip_ascii_whitespace(input.as_bytes(), colon + 1);
        if input.as_bytes().get(quote) != Some(&b'"') {
            cursor = after_field;
            continue;
        }
        output.push_str(&input[after_field..=quote]);
        let Some(end_relative) = find_json_string_end(&input[quote + 1..]) else {
            cursor = quote + 1;
            continue;
        };
        let end = quote + 1 + end_relative;
        output.push_str(replacement);
        output.push('"');
        cursor = end + 1;
    }
    output.push_str(&input[cursor..]);
    output
}

fn find_json_string_end(input: &str) -> Option<usize> {
    let mut escaped = false;
    for (index, character) in input.char_indices() {
        if escaped {
            escaped = false;
        } else if character == '\\' {
            escaped = true;
        } else if character == '"' {
            return Some(index);
        }
    }
    None
}

fn redact_absolute_path_strings(input: &str) -> String {
    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    let http_path_span = http_envelope_path_span(input);
    while let Some(relative) = input[cursor..].find('"') {
        let start = cursor + relative;
        output.push_str(&input[cursor..start]);
        let Some(end_relative) = find_json_string_end(&input[start + 1..]) else {
            output.push_str(&input[start..]);
            return output;
        };
        let end = start + 1 + end_relative;
        let token = &input[start..=end];
        if http_path_span == Some((start, end)) {
            output.push_str(token);
            cursor = end + 1;
            continue;
        }
        match serde_json::from_str::<String>(token) {
            Ok(decoded) => {
                let redacted = redact_absolute_path_substrings(&decoded);
                if redacted == decoded {
                    output.push_str(token);
                } else if let Ok(encoded) = serde_json::to_string(&redacted) {
                    output.push_str(&encoded);
                } else {
                    output.push_str(token);
                }
            }
            Err(_) => output.push_str(token),
        }
        cursor = end + 1;
    }
    output.push_str(&input[cursor..]);
    output
}

fn http_envelope_path_span(input: &str) -> Option<(usize, usize)> {
    let envelope: Value = serde_json::from_str(input).ok()?;
    let object = envelope.as_object()?;
    if !(object.contains_key("method")
        && object.contains_key("path")
        && object.contains_key("content_type"))
    {
        return None;
    }
    root_string_field_span(input, "path")
}

fn root_string_field_span(input: &str, field: &str) -> Option<(usize, usize)> {
    let bytes = input.as_bytes();
    let mut depth = 0_usize;
    let mut index = 0_usize;
    while index < bytes.len() {
        match bytes.get(index) {
            Some(b'{' | b'[') => {
                depth = depth.saturating_add(1);
                index += 1;
            }
            Some(b'}' | b']') => {
                depth = depth.saturating_sub(1);
                index += 1;
            }
            Some(b'"') => {
                let relative_end = find_json_string_end(input.get(index + 1..)?)?;
                let end = index + 1 + relative_end;
                if depth == 1 {
                    let decoded = serde_json::from_str::<String>(input.get(index..=end)?).ok()?;
                    let colon = skip_ascii_whitespace(bytes, end + 1);
                    if decoded == field && bytes.get(colon) == Some(&b':') {
                        let value_start = skip_ascii_whitespace(bytes, colon + 1);
                        if bytes.get(value_start) != Some(&b'"') {
                            return None;
                        }
                        let value_relative_end =
                            find_json_string_end(input.get(value_start + 1..)?)?;
                        return Some((value_start, value_start + 1 + value_relative_end));
                    }
                }
                index = end + 1;
            }
            Some(_) => index += 1,
            None => break,
        }
    }
    None
}

fn skip_ascii_whitespace(bytes: &[u8], mut index: usize) -> usize {
    while bytes.get(index).is_some_and(u8::is_ascii_whitespace) {
        index += 1;
    }
    index
}

fn redact_sandbox_traversals(input: &str, sandbox_variants: &[String]) -> String {
    if sandbox_variants.is_empty() {
        return input.to_owned();
    }
    if path_escapes_prefix(input, sandbox_variants) && serde_json::from_str::<Value>(input).is_err()
    {
        return OUTSIDE_PATH.to_owned();
    }

    let mut output = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(relative) = input[cursor..].find('"') {
        let start = cursor + relative;
        output.push_str(&input[cursor..start]);
        let Some(end_relative) = find_json_string_end(&input[start + 1..]) else {
            output.push_str(&input[start..]);
            return output;
        };
        let end = start + 1 + end_relative;
        let token = &input[start..=end];
        match serde_json::from_str::<String>(token) {
            Ok(decoded) if path_escapes_prefix(&decoded, sandbox_variants) => {
                if let Ok(encoded) = serde_json::to_string(OUTSIDE_PATH) {
                    output.push_str(&encoded);
                } else {
                    output.push_str(token);
                }
            }
            Ok(_) | Err(_) => output.push_str(token),
        }
        cursor = end + 1;
    }
    output.push_str(&input[cursor..]);
    output
}

fn redact_absolute_path_substrings(input: &str) -> String {
    if sandbox_placeholder_escapes(input)
        || is_whole_absolute_file_path(input)
        || find_absolute_path_start(input, 0).is_some()
    {
        OUTSIDE_PATH.to_owned()
    } else {
        input.to_owned()
    }
}

fn sandbox_placeholder_escapes(text: &str) -> bool {
    path_escapes_prefix(text, &["<SANDBOX>".to_owned()])
}

fn path_escapes_prefix(text: &str, prefixes: &[String]) -> bool {
    let normalized_text = text.replace('\\', "/").to_ascii_lowercase();
    prefixes.iter().any(|prefix| {
        let normalized_prefix = prefix
            .replace('\\', "/")
            .trim_end_matches('/')
            .to_ascii_lowercase();
        if normalized_prefix.is_empty() {
            return false;
        }

        let mut cursor = 0;
        while let Some(relative) = normalized_text[cursor..].find(&normalized_prefix) {
            let start = cursor + relative;
            let after_prefix = start + normalized_prefix.len();
            let same_path = normalized_text
                .as_bytes()
                .get(after_prefix)
                .is_none_or(|byte| *byte == b'/');
            if same_path
                && normalized_text
                    .get(after_prefix..)
                    .is_some_and(relative_path_escapes_root)
            {
                return true;
            }
            cursor = after_prefix;
        }
        false
    })
}

fn relative_path_escapes_root(suffix: &str) -> bool {
    let end = suffix
        .char_indices()
        .find(|(_, character)| {
            character.is_whitespace()
                || matches!(
                    character,
                    '"' | '\''
                        | '`'
                        | '<'
                        | '>'
                        | '('
                        | ')'
                        | '['
                        | ']'
                        | '{'
                        | '}'
                        | ','
                        | ';'
                        | '?'
                        | '#'
                )
        })
        .map_or(suffix.len(), |(index, _)| index);
    let mut depth = 0_usize;
    for segment in suffix[..end].split('/') {
        match segment {
            "" | "." => {}
            ".." if depth == 0 => return true,
            ".." => depth -= 1,
            _ => depth = depth.saturating_add(1),
        }
    }
    false
}

fn find_absolute_path_start(input: &str, from: usize) -> Option<usize> {
    let bytes = input.as_bytes();
    let mut index = from;
    while index < bytes.len() {
        if !input.is_char_boundary(index) {
            index += 1;
            continue;
        }
        let boundary = index == 0
            || bytes
                .get(index.wrapping_sub(1))
                .is_some_and(|byte| byte.is_ascii_whitespace() || b"'\"`([={".contains(byte));
        if boundary {
            let windows_drive = bytes.get(index).is_some_and(u8::is_ascii_alphabetic)
                && bytes.get(index + 1) == Some(&b':')
                && matches!(bytes.get(index + 2), Some(b'\\' | b'/'));
            let remainder = input.get(index..)?;
            let unc = remainder.starts_with(r"\\") || remainder.starts_with("//");
            let unix_segments = unix_path_segment_count(remainder);
            let unix = unix_segments >= 2
                || (unix_segments == 1 && single_segment_unix_path_context(input, index));
            if windows_drive || unc || unix {
                return Some(index);
            }
        }
        index += 1;
    }
    None
}

fn single_segment_unix_path_context(input: &str, start: usize) -> bool {
    let end = path_token_end(input, start);
    if start == 0 {
        return end == input.len();
    }
    let prefix = input[..start].trim_end();
    let command = prefix
        .rsplit(|character: char| {
            character.is_whitespace() || matches!(character, '(' | '[' | '{' | '"' | '\'')
        })
        .next()
        .unwrap_or_default()
        .trim_end_matches([':', '='])
        .to_ascii_lowercase();
    matches!(
        command.as_str(),
        "cat"
            | "type"
            | "read"
            | "open"
            | "head"
            | "tail"
            | "less"
            | "more"
            | "rm"
            | "cp"
            | "mv"
            | "stat"
            | "ls"
            | "dir"
            | "get-content"
    )
}

fn is_whole_absolute_file_path(text: &str) -> bool {
    let text = text.trim();
    let bytes = text.as_bytes();
    let windows_drive = bytes.first().is_some_and(u8::is_ascii_alphabetic)
        && bytes.get(1) == Some(&b':')
        && matches!(bytes.get(2), Some(b'\\' | b'/'));
    let unc = text.starts_with(r"\\") || text.starts_with("//");
    let unix_segments = unix_path_segment_count(text);
    let unix = unix_segments >= 2 || (unix_segments == 1 && path_token_end(text, 0) == text.len());
    windows_drive || unc || unix
}

fn unix_path_segment_count(text: &str) -> usize {
    if !text.starts_with('/') || text.starts_with("//") {
        return 0;
    }
    let end = path_token_end(text, 0);
    text.get(1..end)
        .map(|candidate| {
            candidate
                .split('/')
                .filter(|segment| !segment.is_empty())
                .count()
        })
        .unwrap_or(0)
}

fn path_token_end(text: &str, start: usize) -> usize {
    text.get(start..)
        .and_then(|tail| {
            tail.char_indices()
                .skip(1)
                .find(|(_, character)| {
                    character.is_whitespace()
                        || matches!(
                            character,
                            '"' | '\''
                                | '`'
                                | '<'
                                | '>'
                                | '('
                                | ')'
                                | '['
                                | ']'
                                | '{'
                                | '}'
                                | ','
                                | ';'
                                | '?'
                                | '#'
                        )
                })
                .map(|(relative, _)| start + relative)
        })
        .unwrap_or(text.len())
}

fn detect_absolute_paths(
    value: &Value,
    key: Option<&str>,
    http_envelope: bool,
    leaks: &mut Vec<LeakKind>,
) {
    match value {
        Value::Object(map) => {
            let is_http = map.contains_key("method")
                && map.contains_key("path")
                && map.contains_key("content_type");
            for (child_key, child) in map {
                detect_absolute_paths(child, Some(child_key), http_envelope || is_http, leaks);
            }
        }
        Value::Array(items) => {
            for item in items {
                detect_absolute_paths(item, key, http_envelope, leaks);
            }
        }
        Value::String(text) => {
            if http_envelope && key == Some("path") {
                return;
            }
            if text == OUTSIDE_PATH {
                return;
            }
            if sandbox_placeholder_escapes(text) || looks_like_absolute_file_path(text) {
                leaks.push(LeakKind::OutsideSandboxPath(safe_field_label(key)));
            }
        }
        _ => {}
    }
}

fn detect_sensitive_fields(value: &Value, leaks: &mut Vec<LeakKind>) {
    match value {
        Value::Object(map) => {
            for (child_key, child) in map {
                if let Some(field) = sensitive_field_name(child_key) {
                    if child.as_str() != Some(REDACTED_TOKEN) {
                        leaks.push(LeakKind::SensitiveField(field));
                    }
                } else {
                    detect_sensitive_fields(child, leaks);
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                detect_sensitive_fields(item, leaks);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
}

fn sensitive_field_name(field: &str) -> Option<&'static str> {
    if field.eq_ignore_ascii_case("api_key") {
        Some("api_key")
    } else if field.eq_ignore_ascii_case("authorization") {
        Some("authorization")
    } else {
        None
    }
}

fn looks_like_absolute_file_path(text: &str) -> bool {
    is_whole_absolute_file_path(text) || find_absolute_path_start(text, 0).is_some()
}

fn safe_field_label(key: Option<&str>) -> String {
    key.filter(|value| {
        !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    })
    .unwrap_or("<array-or-unknown>")
    .to_owned()
}

#[cfg(test)]
mod tests {
    use std::error::Error;
    use std::ffi::OsString;

    use super::{supported_secret_environment_names, Redactor};

    #[test]
    fn supported_environment_secrets_are_exact_length_first_and_skip_empty_values(
    ) -> Result<(), Box<dyn Error>> {
        let sandbox = tempfile::tempdir()?;
        let redactor =
            Redactor::for_environment_with_secret_lookup(sandbox.path(), |name| match name {
                "CODEX_API_KEY" => Some(OsString::from("fixture-credential")),
                "OPENAI_API_KEY" => Some(OsString::from("fixture-credential-long")),
                "CLAUDE_CODE_OAUTH_TOKEN" => Some(OsString::new()),
                _ => None,
            });
        let input = r#"{"long":"fixture-credential-long","short":"fixture-credential","case":"FIXTURE-credential","empty":""}"#;

        assert_eq!(
            redactor.redact(input),
            r#"{"long":"<REDACTED_TOKEN>","short":"<REDACTED_TOKEN>","case":"FIXTURE-credential","empty":""}"#
        );
        assert!(!format!("{redactor:?}").contains("fixture-credential"));
        Ok(())
    }

    #[test]
    fn short_environment_secret_only_redacts_an_exact_json_string_value(
    ) -> Result<(), Box<dyn Error>> {
        let sandbox = tempfile::tempdir()?;
        let redactor =
            Redactor::for_environment_with_secret_lookup(sandbox.path(), |name| match name {
                "OPENAI_API_KEY" => Some(OsString::from("a")),
                _ => None,
            });
        let input = r#"{"a":"key","article":"a normal sentence","secret":"a"}"#;

        assert_eq!(
            redactor.redact(input),
            r#"{"a":"key","article":"a normal sentence","secret":"<REDACTED_TOKEN>"}"#
        );
        Ok(())
    }

    #[test]
    fn shared_secret_environment_list_covers_agent_credentials() {
        for name in [
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_FOUNDRY_API_KEY",
            "AWS_BEARER_TOKEN_BEDROCK",
            "AZURE_OPENAI_API_KEY",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CODEX_API_KEY",
            "GEMINI_API_KEY",
            "GOOGLE_GENERATIVE_AI_API_KEY",
            "OPENAI_API_KEY",
            "OPENROUTER_API_KEY",
        ] {
            assert!(
                supported_secret_environment_names().any(|candidate| candidate == name),
                "missing supported secret environment name: {name}"
            );
        }
    }
}
