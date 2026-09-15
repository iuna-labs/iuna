#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

usage() {
  echo "usage: scripts/quantum-audit-manifest.sh [--output MANIFEST.json]" >&2
  exit 2
}

output=""
if [ "$#" -gt 0 ]; then
  [ "$#" -eq 2 ] && [ "$1" = "--output" ] || usage
  output="$2"
  [ -n "$output" ] || usage
  if [ -e "$output" ]; then
    echo "error: refusing to overwrite existing manifest: ${output}" >&2
    exit 1
  fi
fi

for command_name in cargo git jq rustc rustup; do
  command -v "$command_name" >/dev/null 2>&1 || {
    echo "error: missing required command: ${command_name}" >&2
    exit 1
  }
done

if [ -n "$(git status --porcelain --untracked-files=normal)" ]; then
  echo "error: refusing to describe a dirty worktree; commit or stash every change first" >&2
  exit 1
fi

sha256_file() {
  local path="$1"
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$path" | awk '{print $1}'
  else
    shasum -a 256 "$path" | awk '{print $1}'
  fi
}

root_lock_sha256="$(sha256_file Cargo.lock)"
fuzz_lock_sha256="$(sha256_file fuzz/Cargo.lock)"
tauri_lock_sha256="$(sha256_file src-tauri/Cargo.lock)"
git_commit="$(git rev-parse HEAD)"
rustc_version="$(rustc --version)"
cargo_version="$(cargo --version)"
nightly_version="$(rustup run nightly rustc --version)"
cargo_fuzz_version="$(cargo fuzz --version)"
captured_at="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

manifest="$(
  jq -n \
    --arg captured_at "$captured_at" \
    --arg git_commit "$git_commit" \
    --arg rustc "$rustc_version" \
    --arg cargo "$cargo_version" \
    --arg nightly_rustc "$nightly_version" \
    --arg cargo_fuzz "$cargo_fuzz_version" \
    --arg root_lock "$root_lock_sha256" \
    --arg fuzz_lock "$fuzz_lock_sha256" \
    --arg tauri_lock "$tauri_lock_sha256" \
    --slurpfile vectors tests/vectors/ml_dsa44_audit.json \
    '{
      format: 1,
      captured_at: $captured_at,
      git: {commit: $git_commit, tree_state: "clean"},
      toolchain: {
        rustc: $rustc,
        cargo: $cargo,
        nightly_rustc: $nightly_rustc,
        cargo_fuzz: $cargo_fuzz
      },
      lockfiles_sha256: {
        "Cargo.lock": $root_lock,
        "fuzz/Cargo.lock": $fuzz_lock,
        "src-tauri/Cargo.lock": $tauri_lock
      },
      vector_sources: $vectors[0].sources
    }'
)"

if [ -n "$output" ]; then
  printf '%s\n' "$manifest" > "$output"
  echo "wrote quantum audit manifest to ${output}"
else
  printf '%s\n' "$manifest"
fi
