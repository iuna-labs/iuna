#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

require_command() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "error: missing required command: $1" >&2
    exit 1
  }
}

require_command cargo
require_command cargo-audit
require_command jq

lockfiles=(Cargo.lock fuzz/Cargo.lock src-tauri/Cargo.lock)
for lockfile in "${lockfiles[@]}"; do
  cargo audit --file "$lockfile"
done

manifests=(Cargo.toml fuzz/Cargo.toml src-tauri/Cargo.toml)
metadata_file="$(mktemp)"
trap 'rm -f "$metadata_file"' EXIT

for manifest in "${manifests[@]}"; do
  cargo metadata --locked --format-version 1 --manifest-path "$manifest" |
    jq -r '.packages[] | [.name, (.license // "MISSING"), (.source // "LOCAL")] | @tsv'
done | sort -u > "$metadata_file"

policy_failed=false
while IFS=$'\t' read -r package license source; do
  case "$source" in
    LOCAL|registry+https://github.com/rust-lang/crates.io-index) ;;
    *)
      echo "error: dependency ${package} uses unapproved source ${source}" >&2
      policy_failed=true
      ;;
  esac

  if [ "$license" = "MISSING" ]; then
    echo "error: dependency ${package} has no declared license" >&2
    policy_failed=true
    continue
  fi

  license_tokens="$(printf '%s\n' "$license" |
    sed -E 's/[()\/]/ /g; s/(^|[[:space:]])(AND|OR|WITH)([[:space:]]|$)/ /g')"
  for license_token in $license_tokens; do
    case "$license_token" in
      0BSD|Apache-2.0|BSD-1-Clause|BSD-2-Clause|BSD-3-Clause|BSL-1.0|CC0-1.0|ISC|LGPL-2.1-or-later|MIT|MIT-0|MPL-2.0|NCSA|Unicode-3.0|Unlicense|Zlib|LLVM-exception) ;;
      *)
        echo "error: dependency ${package} uses unapproved license token ${license_token} (${license})" >&2
        policy_failed=true
        ;;
    esac
  done
done < "$metadata_file"

if [ "$policy_failed" = "true" ]; then
  exit 1
fi

echo "Dependency audit, source policy, and license policy passed for all manifests."
