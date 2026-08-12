#!/usr/bin/env bash
set -euo pipefail

usage() {
  echo "Usage: $0 <version>" >&2
  echo "Example: $0 0.2.48" >&2
}

die() {
  echo "error: $*" >&2
  exit 1
}

replace_in_file() {
  local file="$1"
  local pattern="$2"
  local replacement="$3"
  perl -0pi -e "s/${pattern}/${replacement}/g" "$file"
}

ensure_clean_worktree() {
  if ! git diff --quiet || ! git diff --cached --quiet || [ -n "$(git ls-files --others --exclude-standard)" ]; then
    die "worktree is not clean; commit or stash changes before releasing"
  fi
}

ensure_tauri_cli() {
  if ! cargo tauri --version >/dev/null 2>&1; then
    cargo install tauri-cli --locked --version "^2"
  fi
}

update_versions() {
  local version="$1"

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

  git add Cargo.toml Cargo.lock src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/tauri.conf.json README.md
  git commit -m "Release ${tag}"
  git tag -a "$tag" -m "Release ${tag}"
}

build_macos_desktop_if_possible() {
  local version="$1"
  local artifact="downloads/iuna-v${version}-macos-aarch64-desktop.app.zip"

  [ -f "$artifact" ] && return 0
  [ "$(uname -s)" = "Darwin" ] || return 0
  [ "$(uname -m)" = "arm64" ] || die "macOS desktop artifact requires Apple silicon; expected ${artifact}"

  ensure_tauri_cli
  cargo build --release --locked
  mkdir -p src-tauri/binaries downloads
  cp target/release/iuna src-tauri/binaries/iuna-sidecar-aarch64-apple-darwin
  chmod +x src-tauri/binaries/iuna-sidecar-aarch64-apple-darwin
  (cd src-tauri && cargo tauri build --bundles app)

  local app="src-tauri/target/release/bundle/macos/iuna.app"
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

  mkdir -p downloads
  [ -f "downloads/${linux_x86_64_package}.tar.gz" ] && [ -f "downloads/${linux_aarch64_package}.tar.gz" ] && return 0

  docker run --rm --platform=linux/amd64 \
    -e "IUNA_VERSION=${version}" \
    -e "HOST_UID=$(id -u)" \
    -e "HOST_GID=$(id -g)" \
    -v "$(pwd):/src/iuna:ro" \
    -v "$(pwd)/downloads:/out" \
    rust:1.86-bookworm \
    bash -lc '
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
      cp README.md LICENSE "/tmp/site/${linux_x86_64_package}/"
      cp README.md LICENSE "/tmp/site/${linux_aarch64_package}/"
      tar -C /tmp/site -czf "/out/${linux_x86_64_package}.tar.gz" "${linux_x86_64_package}"
      tar -C /tmp/site -czf "/out/${linux_aarch64_package}.tar.gz" "${linux_aarch64_package}"
      chown "${HOST_UID}:${HOST_GID}" "/out/${linux_x86_64_package}.tar.gz" "/out/${linux_aarch64_package}.tar.gz"
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
  require_desktop_artifacts "$version"
  write_download_checksums
}

build_docker_image() {
  local version="$1"
  local www_image="${IUNA_WWW_IMAGE:-iuna-www:v${version}}"
  local node_image="${IUNA_NODE_IMAGE:-iuna-node:v${version}}"

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

  docker save "$image" -o "$image_file"
  scp "$image_file" "${remote_host}:~/"
  ssh "$remote_host" "sudo k3s ctr -n k8s.io images import ~/${remote_file} && rm ~/${remote_file}"
}

render_manifest() {
  local www_image="$1"
  local node_image="$2"
  local output="$3"

  sed \
    -e "s|\${IUNA_WWW_IMAGE}|${www_image}|g" \
    -e "s|\${IUNA_NODE_IMAGE}|${node_image}|g" \
    config/deployment.yml > "$output"
}

deploy_docker_image() {
  local version="$1"
  local www_image="${IUNA_WWW_IMAGE:-iuna-www:v${version}}"
  local node_image="${IUNA_NODE_IMAGE:-iuna-node:v${version}}"
  local kubectl_context="${IUNA_KUBECTL_CONTEXT:-jhx-app}"
  local tmp_folder

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
  git rev-parse --verify "v${version}" >/dev/null 2>&1 && die "tag v${version} already exists"

  update_versions "$version"
  commit_and_tag "$version"
  build_versions "$version"
  build_docker_image "$version"
  deploy_docker_image "$version"
}

main "$@"
