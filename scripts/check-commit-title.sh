#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "Usage: $0 <commit-message-file>" >&2
  echo "       $0 --title <commit-title>" >&2
  exit 2
}

case "$#:${1:-}" in
  1:*)
    [ -f "$1" ] || usage
    IFS= read -r title < "$1" || true
    ;;
  2:--title)
    title="$2"
    ;;
  *)
    usage
    ;;
esac

pattern='^(feat|fix|docs|refactor|perf|test|build|ci|chore|revert)(\([a-z0-9][a-z0-9._/-]*\))?!?: .+'

if [[ ! "$title" =~ $pattern ]]; then
  cat >&2 <<EOF
Invalid commit title:
  ${title}

Use a Conventional Commit prefix:
  feat, fix, docs, refactor, perf, test, build, ci, chore, or revert

Examples:
  feat(wallet): add transaction filters
  fix!: reject incompatible database formats
  chore(release): release v0.5.0
EOF
  exit 1
fi
