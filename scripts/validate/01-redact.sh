#!/usr/bin/env bash
# Phase 1, PR #2 (W3): dgent-redact masks secrets and leaves ordinary shell output alone.
source "$(dirname "$0")/lib.sh"
need crates/dgent-redact "PR #2 (secret redaction)"

step "unit tests"
check "cargo test -p dgent-redact" cargo test -q -p dgent-redact

step "pipe sample text through redact()"
# A throwaway binary outside the workspace that runs stdin through dgent_redact::redact.
mkdir -p "$TMP/r/src"
cat >"$TMP/r/Cargo.toml" <<TOML
[package]
name = "redact-demo"
version = "0.0.0"
edition = "2024"
[dependencies]
dgent-redact = { path = "$PWD/crates/dgent-redact" }
[workspace]
TOML
echo 'fn main(){let s=std::io::read_to_string(std::io::stdin()).unwrap();print!("{}",dgent_redact::redact(&s));}' >"$TMP/r/src/main.rs"
redact() { CARGO_TARGET_DIR=$PWD/target/validate cargo run -q --manifest-path "$TMP/r/Cargo.toml"; }
check "demo builds" redact </dev/null

# Each line has one secret; none of the secret text may survive.
secrets=(
  "export AWS_ACCESS_KEY_ID=AKIAIOSFODNN7EXAMPLE|AKIAIOSFODNN7EXAMPLE"
  "git remote add o https://ghp_0123456789abcdefghijklmnopqrstuvwxyz@github.com/x|ghp_0123456789"
  "SLACK=xoxb-123456789012-abcdefghijkl|xoxb-1234"
  "curl -H 'Authorization: Bearer abc.def-ghi_jkl' https://api|abc.def-ghi_jkl"
  "mysql --password=hunter2hunter2|hunter2hunter2"
  "token eyJhbGciOiJIUzI1NiJ9.eyJzdWIiOiIxIn0.c2lnbmF0dXJl|c2lnbmF0dXJl"
)
for pair in "${secrets[@]}"; do
  line=${pair%|*} secret=${pair#*|}
  out=$(printf '%s\n' "$line" | redact)
  if grep -qF -- "$secret" <<<"$out" || ! grep -qF '[REDACTED]' <<<"$out"; then
    bad "masks: $line -> $out"
  else ok "masks: $out"; fi
done
key=$(printf -- '-----BEGIN OPENSSH PRIVATE KEY-----\nb3BlbnNzaC1rZXktdjEAAAAA\n-----END OPENSSH PRIVATE KEY-----\n')
out=$(printf '%s\n' "$key" | redact)
grep -q b3BlbnNzaC1rZXktdjEAAAAA <<<"$out" && bad "private key block: $out" || ok "private key block masked whole"

# Ordinary output must come back byte for byte.
plain=$( { ls -la / 2>/dev/null | head -n 5; git log -3 --format='%h %an %s' 2>/dev/null; env | grep -E '^(PATH|HOME|SHELL)='
  echo "ls: cannot access 'secret': No such file or directory"; echo "[sudo] password for russell:"; uuidgen 2>/dev/null; } )
[ "$(printf '%s\n' "$plain" | redact)" = "$plain" ] && ok "ordinary shell output unchanged" \
  || bad "ordinary shell output was changed: $(diff <(printf '%s\n' "$plain") <(printf '%s\n' "$plain" | redact) | head -n 5)"

echo "Note: not wired into the TUI yet. PR #4 (overseer) calls it; 08-overseer.sh checks that."
finish
