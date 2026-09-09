#!/usr/bin/env bash
set -euo pipefail

repo_root="$(git rev-parse --show-toplevel)"
git -C "$repo_root" config core.hooksPath .githooks
echo "Git hooks enabled for ${repo_root}"
