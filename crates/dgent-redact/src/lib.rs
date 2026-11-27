//! Masks secrets in terminal text before it is sent to an LLM backend.

use regex::Regex;
use std::borrow::Cow;
use std::sync::LazyLock;

const MASK: &str = "[REDACTED]";

// (pattern, replacement); `$n` keeps the non-secret part, such as a key name.
static RULES: LazyLock<Vec<(Regex, String)>> = LazyLock::new(|| {
    [
        // PEM private key blocks, whole block including markers.
        (
            r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?-----END [A-Z0-9 ]*PRIVATE KEY-----",
            MASK.to_string(),
        ),
        // AWS access key ids.
        (r"\b(?:AKIA|ASIA)[0-9A-Z]{16}\b", MASK.to_string()),
        // GitHub tokens: classic (ghp_, gho_, ...) and fine-grained.
        (
            r"\b(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{22,})",
            MASK.to_string(),
        ),
        // Slack tokens.
        (r"\bxox[abprs]-[A-Za-z0-9-]{10,}", MASK.to_string()),
        // JWTs (header and payload both start with base64 `{"`).
        (
            r"\beyJ[A-Za-z0-9_-]+\.eyJ[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+",
            MASK.to_string(),
        ),
        // Authorization: Bearer <token>
        (
            r"(?i)(\bbearer[ \t]+)[A-Za-z0-9._~+/-]+=*",
            format!("${{1}}{MASK}"),
        ),
    ]
    .into_iter()
    .map(|(p, r)| (Regex::new(p).expect("valid redaction pattern"), r))
    .collect()
});

// Key and separator of password=..., API_KEY: ..., "client_secret": "...".
// Only double-quoted keys, so `ls: cannot access 'secret': ...` is left alone.
static ASSIGN_KEY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)(?:"[a-z0-9_.-]*(?:password|passwd|secret|token|api[_-]?key)[a-z0-9_.-]*"|\b[a-z0-9_.-]*(?:password|passwd|secret|token|api[_-]?key)[a-z0-9_.-]*)[ \t]*[=:][ \t]*"#,
    )
    .expect("valid assignment pattern")
});

/// Masks the value after each assignment key. A quoted value runs to its
/// closing quote; a bare value runs to whitespace (punctuation can be part of
/// a secret) but stops where the next key starts, so `a=1;token="x y"` and
/// compact JSON mask both values.
fn mask_assignments(text: &str) -> Option<String> {
    let keys: Vec<_> = ASSIGN_KEY.find_iter(text).collect();
    let mut out = String::new();
    let mut done = 0; // text[..done] is already copied or masked
    for (i, key) in keys.iter().enumerate() {
        if key.start() < done {
            continue; // key text sat inside an earlier value
        }
        let start = key.end();
        let rest = &text[start..];
        let len = match rest.chars().next() {
            Some(q @ ('"' | '\'')) => rest[1..].find([q, '\n']).map_or(rest.len(), |e| {
                e + 1 + usize::from(rest[1 + e..].starts_with(q))
            }),
            _ => {
                let next = keys[i + 1..]
                    .iter()
                    .map(|k| k.start() - start)
                    .find(|&k| k > 0)
                    .unwrap_or(rest.len());
                rest[..next].find(char::is_whitespace).unwrap_or(next)
            }
        };
        if len == 0 {
            continue;
        }
        out.push_str(&text[done..start]);
        out.push_str(MASK);
        done = start + len;
    }
    (done > 0).then(|| out + &text[done..])
}

/// Returns `text` with known secret shapes replaced by `[REDACTED]`.
/// Borrows when nothing matched.
pub fn redact(text: &str) -> Cow<'_, str> {
    let mut out = Cow::Borrowed(text);
    for (re, rep) in RULES.iter() {
        if re.is_match(&out) {
            out = Cow::Owned(re.replace_all(&out, rep.as_str()).into_owned());
        }
    }
    match mask_assignments(&out) {
        Some(masked) => Cow::Owned(masked),
        None => out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_each_pattern() {
        let cases = [
            (
                "-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\nAAAA\n-----END OPENSSH PRIVATE KEY-----\nok",
                "[REDACTED]\nok",
            ),
            (
                "-----BEGIN RSA PRIVATE KEY-----\nMIIEow\n-----END RSA PRIVATE KEY-----",
                "[REDACTED]",
            ),
            ("key AKIAIOSFODNN7EXAMPLE here", "key [REDACTED] here"),
            ("ghp_abcdefghijklmnopqrstuvwxyz0123456789", "[REDACTED]"),
            (
                "github_pat_11ABCDEFG0123456789_abcdefghijklmnopqrstuvwxyz",
                "[REDACTED]",
            ),
            ("SLACK=xoxb-1234567890-abcdefghij", "SLACK=[REDACTED]"),
            (
                "eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxMjM0In0.dBjftJeZ4CVPmB92K27uhbUJU1p1r_wW1gFWFOEjXk",
                "[REDACTED]",
            ),
            (
                "Authorization: Bearer abc.def-123/xyz==",
                "Authorization: Bearer [REDACTED]",
            ),
            ("password=hunter2 next", "password=[REDACTED] next"),
            ("export API_KEY='s3cr3t value'", "export API_KEY=[REDACTED]"),
            (
                r#"{"client_secret": "abc def"}"#,
                r#"{"client_secret": [REDACTED]}"#,
            ),
            (
                "aws_secret_access_key = wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY",
                "aws_secret_access_key = [REDACTED]",
            ),
            ("DB_PASSWORD: pa:ss@w0rd", "DB_PASSWORD: [REDACTED]"),
            (
                // Punctuation can be part of the secret (printenv), so mask to whitespace.
                "DB_PASSWORD=&violet7 X_TOKEN=cedar&vi;ol|et API_KEY=example; next",
                "DB_PASSWORD=[REDACTED] X_TOKEN=[REDACTED] API_KEY=[REDACTED] next",
            ),
            (
                r#"API_KEY=example;TOKEN="sample value" next"#,
                "API_KEY=[REDACTED]TOKEN=[REDACTED] next",
            ),
            (
                r#"{"token":1234,"password": "sample value"}"#,
                r#"{"token":[REDACTED]"password": [REDACTED]}"#,
            ),
            (
                r#"{"user":"a","password":"p1","api_key":"k1"}"#,
                r#"{"user":"a","password":[REDACTED],"api_key":[REDACTED]}"#,
            ),
        ];
        for (input, want) in cases {
            assert_eq!(redact(input), want, "input: {input:?}");
        }
    }

    #[test]
    fn leaves_ordinary_shell_output_alone() {
        let output = "\
$ ls -la ~/.ssh
total 24
drwx------  2 russ russ 4096 Nov 26 19:08 .
-rw-------  1 russ russ  411 Nov 26 19:08 id_ed25519
-rw-r--r--  1 russ russ   97 Nov 26 19:08 id_ed25519.pub
$ git log --oneline -2
0acf6ef Merge pull request #1 from xchewtoyx/claude/project-thread-jov69k
20fb79c Encode modifiers on Insert/Delete/PgUp/PgDn and F-keys
$ env | grep -E 'PATH|PWD'
PATH=/usr/local/bin:/usr/bin:/bin
OLDPWD=/home/russ
[sudo] password for russ:
Password:
ls: cannot access 'secret': No such file or directory
$ systemctl status nginx
● nginx.service - A high performance web server
   Active: active (running) since Thu 2026-11-26 19:08:38 UTC; 3h ago
 Main PID: 1234 (nginx)
$ cat /proc/sys/kernel/random/uuid
6f1c2b9e-8a7d-4c3e-9b1a-2d5e6f7a8b9c
sha256:9807af0e638c12e753aa1b4e601791bf2a8f71274023c8bbeec12fc149950857
";
        assert!(
            matches!(redact(output), Cow::Borrowed(_)),
            "{}",
            redact(output)
        );
    }
}
