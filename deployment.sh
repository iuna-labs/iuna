#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"

usage() {
  echo "Usage: $0 <version>" >&2
  echo "Example: $0 0.2.48" >&2
}

die() {
  echo "error: $*" >&2
  exit 1
}

require_command() {
  local command_name="$1"

  command -v "$command_name" >/dev/null 2>&1 || die "missing required command: ${command_name}"
}

is_apple_silicon_macos() {
  [ "$(uname -s)" = "Darwin" ] || return 1
  [ "$(uname -m)" = "arm64" ] && return 0
  [ "$(sysctl -n hw.optional.arm64 2>/dev/null || true)" = "1" ]
}

confirm() {
  local prompt="$1"
  local answer

  if ! read -r -p "$prompt" answer || [[ ! "$answer" =~ ^[Yy]$ ]]; then
    return 1
  fi
}

escape_sed_replacement() {
  printf '%s' "$1" | sed -e 's/[\/&]/\\&/g'
}

replace_in_file() {
  local file="$1"
  local pattern="$2"
  local replacement="$3"
  perl -0pi -e "s|${pattern}|${replacement}|g" "$file"
}

validate_positive_integer() {
  local name="$1"
  local value="$2"

  [[ "$value" =~ ^[1-9][0-9]*$ ]] || die "${name} must be a positive integer"
}

ensure_clean_worktree() {
  require_command git

  if ! git diff --quiet || ! git diff --cached --quiet || [ -n "$(git ls-files --others --exclude-standard)" ]; then
    die "worktree is not clean; commit or stash changes before releasing"
  fi
}

ensure_head_matches_tag() {
  local tag="$1"
  local head_commit
  local tag_commit

  head_commit="$(git rev-parse HEAD)"
  tag_commit="$(git rev-parse "${tag}^{commit}")"
  [ "$head_commit" = "$tag_commit" ] || die "${tag} exists, but HEAD is not at ${tag}; checkout ${tag} before redeploying it"
}

ensure_tauri_cli() {
  require_command cargo

  if ! cargo tauri --version >/dev/null 2>&1; then
    cargo install tauri-cli --locked --version "^2"
  fi
}

run_release_tests() {
  require_command cargo

  local fuzz_runs="${IUNA_FUZZ_RUNS:-256}"
  local vdf_fuzz_runs="${IUNA_VDF_FUZZ_RUNS:-16}"
  validate_positive_integer IUNA_FUZZ_RUNS "$fuzz_runs"
  validate_positive_integer IUNA_VDF_FUZZ_RUNS "$vdf_fuzz_runs"

  cargo test --locked
  cargo test --locked domain::adversarial_tests:: -- --ignored
  cargo check --locked --manifest-path fuzz/Cargo.toml
  cargo run --locked --manifest-path fuzz/Cargo.toml --bin p2p_envelope -- -runs="$fuzz_runs" fuzz/corpus/p2p_envelope
  cargo run --locked --manifest-path fuzz/Cargo.toml --bin compact_snapshot -- -runs="$fuzz_runs" fuzz/corpus/compact_snapshot
  cargo run --locked --manifest-path fuzz/Cargo.toml --bin domain_json -- -runs="$fuzz_runs" fuzz/corpus/domain_json
  cargo run --locked --manifest-path fuzz/Cargo.toml --bin stratum_request -- -runs="$fuzz_runs" fuzz/corpus/stratum_request
  cargo run --locked --manifest-path fuzz/Cargo.toml --bin wallet_config -- -runs="$fuzz_runs" fuzz/corpus/wallet_config
  cargo run --locked --manifest-path fuzz/Cargo.toml --bin vdf_proof -- -runs="$vdf_fuzz_runs" fuzz/corpus/vdf_proof
  cargo test --locked --release --test properties -- --ignored
}

update_versions() {
  local version="$1"

  require_command cargo
  require_command perl

  replace_in_file Cargo.toml '(\[package\]\nname = "iuna"\nversion = ")[^"]+' "\${1}${version}"
  replace_in_file src-tauri/Cargo.toml '(\[package\]\nname = "iuna-desktop"\nversion = ")[^"]+' "\${1}${version}"
  replace_in_file src-tauri/tauri.conf.json '("version": ")[^"]+' "\${1}${version}"
  replace_in_file README.md 'downloads/iuna-v[0-9]+\.[0-9]+\.[0-9]+-macos-aarch64-desktop\.app\.zip' "downloads/iuna-v${version}-macos-aarch64-desktop.app.zip"
  replace_in_file README.md 'downloads/iuna-v[0-9]+\.[0-9]+\.[0-9]+-windows-x86_64-desktop-setup\.exe' "downloads/iuna-v${version}-windows-x86_64-desktop-setup.exe"

  cargo update -p iuna --precise "$version"
  cargo update --manifest-path src-tauri/Cargo.toml -p iuna-desktop --precise "$version"
  cargo check --locked >/dev/null
  cargo check --locked --manifest-path src-tauri/Cargo.toml >/dev/null
}

commit_and_tag() {
  local version="$1"
  local tag="v${version}"

  require_command git

  git add Cargo.toml Cargo.lock src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/tauri.conf.json README.md
  git commit -m "Release ${tag}" --no-verify
  git tag -a "$tag" -m "Release ${tag}"
}

build_macos_desktop_if_possible() {
  local version="$1"
  local artifact="downloads/iuna-v${version}-macos-aarch64-desktop.app.zip"

  [ -f "$artifact" ] && return 0
  [ "$(uname -s)" = "Darwin" ] || return 0
  is_apple_silicon_macos || die "macOS desktop artifact requires Apple silicon; expected ${artifact}"

  require_command codesign
  require_command ditto
  require_command rustup
  ensure_tauri_cli
  rustup target add aarch64-apple-darwin
  cargo build --release --locked --target aarch64-apple-darwin
  mkdir -p src-tauri/binaries downloads
  cp target/aarch64-apple-darwin/release/iuna src-tauri/binaries/iuna-sidecar-aarch64-apple-darwin
  chmod +x src-tauri/binaries/iuna-sidecar-aarch64-apple-darwin
  (cd src-tauri && cargo tauri build --target aarch64-apple-darwin --bundles app)

  local app="src-tauri/target/aarch64-apple-darwin/release/bundle/macos/iuna.app"
  codesign --force --deep --sign - --options runtime "$app"
  codesign --verify --deep --strict --verbose=4 "$app"
  ditto -c -k --keepParent "$app" "$artifact"
}

build_windows_desktop_if_possible() {
  local version="$1"
  local artifact="downloads/iuna-v${version}-windows-x86_64-desktop-setup.exe"

  [ -f "$artifact" ] && return 0
  case "$(uname -s)" in
    MINGW*|MSYS*|CYGWIN*) ;;
    *) return 0 ;;
  esac

  ensure_tauri_cli
  cargo build --release --locked
  mkdir -p src-tauri/binaries downloads
  cp target/release/iuna.exe src-tauri/binaries/iuna-sidecar-x86_64-pc-windows-msvc.exe
  (cd src-tauri && cargo tauri build --bundles nsis)

  local installer
  installer="$(find src-tauri/target/release/bundle/nsis -maxdepth 1 -type f -name '*.exe' | head -n 1)"
  [ -n "$installer" ] || die "Windows installer was not produced"
  cp "$installer" "$artifact"
}

build_windows_desktop_in_docker_if_possible() {
  local version="$1"
  local artifact="downloads/iuna-v${version}-windows-x86_64-desktop-setup.exe"

  [ -f "$artifact" ] && return 0
  command -v docker >/dev/null 2>&1 || return 0

  mkdir -p downloads
  docker run --rm --platform=linux/amd64 \
    -e "IUNA_VERSION=${version}" \
    -e "HOST_UID=$(id -u)" \
    -e "HOST_GID=$(id -g)" \
    -v iuna-windows-cargo-registry:/usr/local/cargo/registry \
    -v iuna-windows-cargo-git:/usr/local/cargo/git \
    -v iuna-windows-root-cache:/root/.cache \
    -v iuna-windows-target:/work/iuna/target \
    -v iuna-windows-tauri-target:/work/iuna/src-tauri/target \
    -v "$(pwd):/src/iuna:ro" \
    -v "$(pwd)/downloads:/out" \
    rust:1.86-bookworm \
    bash -c '
      set -euo pipefail

      apt-get update
      apt-get install -y --no-install-recommends clang lld llvm nsis
      rm -rf /var/lib/apt/lists/*
      rustup target add x86_64-pc-windows-msvc
      cargo install --locked cargo-xwin --version 0.19.2
      cargo install --locked tauri-cli --version "^2"

      nsis_utils_path=/root/.cache/tauri/NSIS/Plugins/x86-unicode/additional/nsis_tauri_utils.dll
      mkdir -p "$(dirname "$nsis_utils_path")"
      if [ ! -f "$nsis_utils_path" ]; then
        curl --fail --location --retry 8 --retry-all-errors --retry-delay 3 \
          --output "$nsis_utils_path" \
          https://github.com/tauri-apps/nsis-tauri-utils/releases/download/nsis_tauri_utils-v0.5.3/nsis_tauri_utils.dll
        echo "75197fee3c6a814fe035788d1c34ead39349b860  $nsis_utils_path" | sha1sum -c -
      fi

      mkdir -p /work/iuna
      tar -C /src/iuna \
        --exclude=./target \
        --exclude=./src-tauri/target \
        --exclude=./src-tauri/binaries \
        --exclude=./.agents \
        --exclude=./.codex \
        -cf - . | tar -C /work/iuna -xf -

      cd /work/iuna
      cargo xwin build --release --locked --target x86_64-pc-windows-msvc
      mkdir -p src-tauri/binaries
      cp target/x86_64-pc-windows-msvc/release/iuna.exe src-tauri/binaries/iuna-sidecar-x86_64-pc-windows-msvc.exe

      cd src-tauri
      cargo tauri build --runner cargo-xwin --target x86_64-pc-windows-msvc --bundles nsis

      installer="$(find target/x86_64-pc-windows-msvc/release/bundle/nsis -maxdepth 1 -type f -name "*setup.exe" | head -n 1)"
      [ -n "$installer" ] || { echo "Windows installer was not produced" >&2; exit 1; }
      cp "$installer" "/out/iuna-v${IUNA_VERSION}-windows-x86_64-desktop-setup.exe"
      chown "${HOST_UID}:${HOST_GID}" "/out/iuna-v${IUNA_VERSION}-windows-x86_64-desktop-setup.exe"
    '
}

require_desktop_artifacts() {
  local version="$1"
  local macos_artifact="downloads/iuna-v${version}-macos-aarch64-desktop.app.zip"
  local windows_artifact="downloads/iuna-v${version}-windows-x86_64-desktop-setup.exe"

  [ -f "$macos_artifact" ] || die "missing ${macos_artifact}"
  [ -f "$windows_artifact" ] || die "missing ${windows_artifact}"
}

build_linux_cli_archives() {
  local version="$1"
  local tag="v${version}"
  local linux_x86_64_package="iuna-${tag}-linux-x86_64"
  local linux_aarch64_package="iuna-${tag}-linux-aarch64"

  mkdir -p .docker-build downloads
  [ -f "downloads/${linux_x86_64_package}.tar.gz" ] \
    && [ -f "downloads/${linux_aarch64_package}.tar.gz" ] \
    && [ -f .docker-build/iuna-node-linux-x86_64 ] \
    && return 0

  require_command docker

  docker run --rm --platform=linux/amd64 \
    -e "IUNA_VERSION=${version}" \
    -e "HOST_UID=$(id -u)" \
    -e "HOST_GID=$(id -g)" \
    -v "$(pwd):/src/iuna:ro" \
    -v "$(pwd)/downloads:/out" \
    -v "$(pwd)/.docker-build:/node-out" \
    rust:1.86-bookworm \
    bash -c '
      set -euo pipefail

      apt-get update
      apt-get install -y --no-install-recommends gcc-aarch64-linux-gnu libc6-dev-arm64-cross
      rm -rf /var/lib/apt/lists/*
      rustup target add aarch64-unknown-linux-gnu

      mkdir -p /work/iuna
      tar -C /src/iuna \
        --exclude=./target \
        --exclude=./src-tauri/target \
        --exclude=./src-tauri/binaries \
        --exclude=./.agents \
        --exclude=./.codex \
        --exclude=./.docker-build \
        -cf - . | tar -C /work/iuna -xf -

      cd /work/iuna
      CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc \
      AR_aarch64_unknown_linux_gnu=aarch64-linux-gnu-ar \
      CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc \
      cargo build --release --locked --target aarch64-unknown-linux-gnu
      cargo build --release --locked

      tag="v${IUNA_VERSION}"
      linux_x86_64_package="iuna-${tag}-linux-x86_64"
      linux_aarch64_package="iuna-${tag}-linux-aarch64"
      mkdir -p "/tmp/site/${linux_x86_64_package}" "/tmp/site/${linux_aarch64_package}"
      cp target/release/iuna "/tmp/site/${linux_x86_64_package}/"
      cp target/aarch64-unknown-linux-gnu/release/iuna "/tmp/site/${linux_aarch64_package}/"
      cp target/release/iuna /node-out/iuna-node-linux-x86_64
      cp README.md LICENSE "/tmp/site/${linux_x86_64_package}/"
      cp README.md LICENSE "/tmp/site/${linux_aarch64_package}/"
      tar -C /tmp/site -czf "/out/${linux_x86_64_package}.tar.gz" "${linux_x86_64_package}"
      tar -C /tmp/site -czf "/out/${linux_aarch64_package}.tar.gz" "${linux_aarch64_package}"
      chown "${HOST_UID}:${HOST_GID}" "/out/${linux_x86_64_package}.tar.gz" "/out/${linux_aarch64_package}.tar.gz" /node-out/iuna-node-linux-x86_64
    '
}

write_download_checksums() {
  (
    cd downloads
    rm -f SHA256SUMS

    local files=()
    local file
    for file in *; do
      [ -f "$file" ] || continue
      case "$file" in
        .gitkeep|index.html|SHA256SUMS) continue ;;
      esac
      files+=("$file")
    done

    [ "${#files[@]}" -gt 0 ] || return 0
    if command -v sha256sum >/dev/null 2>&1; then
      sha256sum "${files[@]}" > SHA256SUMS
    else
      for file in "${files[@]}"; do
        shasum -a 256 "$file" | awk "{print \$1 \"  \" \$2}"
      done > SHA256SUMS
    fi
  )
}

build_versions() {
  local version="$1"

  mkdir -p downloads
  build_linux_cli_archives "$version"
  build_macos_desktop_if_possible "$version"
  build_windows_desktop_if_possible "$version"
  build_windows_desktop_in_docker_if_possible "$version"
  require_desktop_artifacts "$version"
  write_download_checksums
}

build_docker_image() {
  local version="$1"
  local www_image="${IUNA_WWW_IMAGE:-iuna-www:v${version}}"
  local node_image="${IUNA_NODE_IMAGE:-iuna-node:v${version}}"

  require_command docker

  [ -f .docker-build/iuna-node-linux-x86_64 ] || die "missing .docker-build/iuna-node-linux-x86_64; run build_versions first"

  docker build --platform=linux/amd64 --progress=plain -t "$www_image" .
  docker build --platform=linux/amd64 --progress=plain -t "$node_image" -f Dockerfile.node .
  echo "Built Docker images: ${www_image}, ${node_image}"
}

import_image_to_k3s() {
  local image="$1"
  local tmp_folder="$2"
  local remote_host="${IUNA_DEPLOY_HOST:-root@jhx.app}"
  local remote_file="${image//[:\/]/_}.tar"
  local image_file="${tmp_folder}/${remote_file}"

  require_command docker
  require_command scp
  require_command ssh

  docker save "$image" -o "$image_file"
  scp "$image_file" "${remote_host}:~/"
  ssh "$remote_host" "sudo k3s ctr -n k8s.io images import ~/${remote_file} && rm ~/${remote_file}"
}

render_manifest() {
  local www_image="$1"
  local node_image="$2"
  local output="$3"
  local escaped_www_image
  local escaped_node_image

  escaped_www_image="$(escape_sed_replacement "$www_image")"
  escaped_node_image="$(escape_sed_replacement "$node_image")"

  sed \
    -e "s|\${IUNA_WWW_IMAGE}|${escaped_www_image}|g" \
    -e "s|\${IUNA_NODE_IMAGE}|${escaped_node_image}|g" \
    config/deployment.yml > "$output"
}

deploy_docker_image() {
  local version="$1"
  local www_image="${IUNA_WWW_IMAGE:-iuna-www:v${version}}"
  local node_image="${IUNA_NODE_IMAGE:-iuna-node:v${version}}"
  local kubectl_context="${IUNA_KUBECTL_CONTEXT:-jhx-app}"
  local tmp_folder

  require_command kubectl

  tmp_folder="$(mktemp -d)"
  trap 'rm -rf "$tmp_folder"' RETURN

  import_image_to_k3s "$www_image" "$tmp_folder"
  import_image_to_k3s "$node_image" "$tmp_folder"
  render_manifest "$www_image" "$node_image" "${tmp_folder}/deployment.yml"

  local current_www_selector
  current_www_selector="$(kubectl --context "$kubectl_context" -n iuna get deployment www -o jsonpath='{.spec.selector.matchLabels.app}' 2>/dev/null || true)"
  if [ -n "$current_www_selector" ] && [ "$current_www_selector" != "iuna-www" ]; then
    kubectl --context "$kubectl_context" -n iuna delete deployment www --wait=true
  fi

  kubectl --context "$kubectl_context" apply -f "${tmp_folder}/deployment.yml"
  kubectl --context "$kubectl_context" -n iuna rollout restart deployment/www deployment/node
  kubectl --context "$kubectl_context" -n iuna rollout status deployment/www
  kubectl --context "$kubectl_context" -n iuna rollout status deployment/node
}

main() {
  [ "$#" -eq 1 ] || { usage; exit 2; }

  local version="${1#v}"
  [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "version must look like 0.2.48"

  ensure_clean_worktree

  # Check if the tag already exists; if it does, only deploy
  if git rev-parse --verify "v${version}" >/dev/null 2>&1; then
    ensure_head_matches_tag "v${version}"
    echo "Tag v${version} already exists; rebuilding Docker images and deploying"
    if ! confirm "Are you sure you want to deploy v${version}? (y/N) "; then
      echo "Aborting deployment"
      exit 1
    fi
    run_release_tests
    build_linux_cli_archives "$version"
    build_docker_image "$version"
    deploy_docker_image "$version"
    exit 0
  fi

  update_versions "$version"
  run_release_tests
  build_versions "$version"
  commit_and_tag "$version"
  build_docker_image "$version"
  deploy_docker_image "$version"
}

main "$@"
