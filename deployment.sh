#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")"

usage() {
  echo "Usage:"
  echo "  $0 [--genesis] [--skip-long-tests] <version>"
  echo "  $0 --website-only"
  echo
  echo "Options:"
  echo "  --genesis          Start a new chain and permanently replace the node PVC"
  echo "  --skip-long-tests  Skip long-running release test suites"
  echo "  --website-only     Deploy the website from the current commit without a release"
  echo "  -h, --help         Show this help"
  echo
  echo "Examples:"
  echo "  $0 0.4.7"
  echo "  $0 --skip-long-tests 0.4.7"
  echo "  $0 --genesis 0.4.7"
  echo "  $0 --website-only"
}

die() {
  echo "error: $*" >&2
  exit 1
}

require_command() {
  local command_name="$1"

  command -v "$command_name" >/dev/null 2>&1 || die "missing required command: ${command_name}"
}

docker_native_linux_platform() {
  local architecture

  # Keep rustc native to the Docker engine; cross-compile only the release binaries.
  architecture="$(docker info --format '{{.Architecture}}')" || die "could not determine Docker engine architecture"
  case "$architecture" in
    amd64|x86_64)
      printf '%s\n' linux/amd64
      ;;
    arm64|aarch64)
      printf '%s\n' linux/arm64
      ;;
    *)
      die "unsupported Docker engine architecture: ${architecture}"
      ;;
  esac
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
    die "worktree is not clean; commit or stash changes before deploying or releasing"
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

clear_nsis_installers() {
  local nsis_dir="$1"

  mkdir -p "$nsis_dir"
  find "$nsis_dir" -maxdepth 1 -type f -name '*-setup.exe' -delete
}

versioned_nsis_installer() {
  local nsis_dir="$1"
  local version="$2"
  local installer="${nsis_dir}/iuna_${version}_x64-setup.exe"

  [ -f "$installer" ] || die "Windows installer for version ${version} was not produced at ${installer}"
  printf '%s\n' "$installer"
}

run_release_tests() {
  local skip_long_tests="$1"

  require_command cargo

  ./scripts/check-dependencies.sh
  cargo test --locked
  cargo check --locked --manifest-path fuzz/Cargo.toml
  ./e2e/iuna_e2e.py test snapshots

  if [ "$skip_long_tests" = "true" ]; then
    echo "WARNING: skipping long-running adversarial, fuzz, post-activation E2E, and property test suites"
    return 0
  fi

  local fuzz_runs="${IUNA_FUZZ_RUNS:-256}"
  local vdf_fuzz_runs="${IUNA_VDF_FUZZ_RUNS:-16}"
  validate_positive_integer IUNA_FUZZ_RUNS "$fuzz_runs"
  validate_positive_integer IUNA_VDF_FUZZ_RUNS "$vdf_fuzz_runs"

  cargo test --locked --release --lib domain::adversarial_tests:: -- --ignored
  cargo run --locked --manifest-path fuzz/Cargo.toml --bin p2p_envelope -- -runs="$fuzz_runs" fuzz/corpus/p2p_envelope
  cargo run --locked --manifest-path fuzz/Cargo.toml --bin compact_snapshot -- -runs="$fuzz_runs" fuzz/corpus/compact_snapshot
  cargo run --locked --manifest-path fuzz/Cargo.toml --bin domain_json -- -runs="$fuzz_runs" fuzz/corpus/domain_json
  cargo run --locked --manifest-path fuzz/Cargo.toml --bin stratum_request -- -runs="$fuzz_runs" fuzz/corpus/stratum_request
  cargo run --locked --manifest-path fuzz/Cargo.toml --bin wallet_config -- -runs="$fuzz_runs" fuzz/corpus/wallet_config
  cargo run --locked --manifest-path fuzz/Cargo.toml --bin vdf_proof -- -runs="$vdf_fuzz_runs" fuzz/corpus/vdf_proof
  cargo test --locked --release --features e2e --test properties -- --ignored
  ./e2e/iuna_e2e.py test post-activation --build
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

  cargo update --offline -p iuna --precise "$version"
  cargo update --offline --manifest-path src-tauri/Cargo.toml -p iuna-desktop --precise "$version"
  cargo update --offline --manifest-path fuzz/Cargo.toml -p iuna --precise "$version"
  cargo check --locked >/dev/null
  cargo check --locked --manifest-path src-tauri/Cargo.toml >/dev/null
  cargo check --locked --manifest-path fuzz/Cargo.toml >/dev/null
}

commit_and_tag() {
  local version="$1"
  local tag="v${version}"

  require_command git

  git add Cargo.toml Cargo.lock fuzz/Cargo.lock src-tauri/Cargo.toml src-tauri/Cargo.lock src-tauri/tauri.conf.json README.md
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
  local nsis_dir="src-tauri/target/release/bundle/nsis"
  clear_nsis_installers "$nsis_dir"
  (cd src-tauri && cargo tauri build --bundles nsis)

  local installer
  installer="$(versioned_nsis_installer "$nsis_dir" "$version")"
  cp "$installer" "$artifact"
}

build_windows_desktop_in_docker_if_possible() {
  local version="$1"
  local artifact="downloads/iuna-v${version}-windows-x86_64-desktop-setup.exe"
  local builder_platform
  local builder_arch

  [ -f "$artifact" ] && return 0
  command -v docker >/dev/null 2>&1 || return 0

  builder_platform="$(docker_native_linux_platform)"
  builder_arch="${builder_platform#linux/}"

  mkdir -p downloads
  docker run --rm --pull=always --platform="$builder_platform" \
    -e "IUNA_VERSION=${version}" \
    -e "HOST_UID=$(id -u)" \
    -e "HOST_GID=$(id -g)" \
    -v iuna-windows-cargo-registry:/usr/local/cargo/registry \
    -v iuna-windows-cargo-git:/usr/local/cargo/git \
    -v iuna-windows-root-cache:/root/.cache \
    -v "iuna-windows-${builder_arch}-target:/work/iuna/target" \
    -v "iuna-windows-${builder_arch}-tauri-target:/work/iuna/src-tauri/target" \
    -v "$(pwd):/src/iuna:ro" \
    -v "$(pwd)/downloads:/out" \
    rust:1.88-bookworm \
    bash -c '
      set -euo pipefail

      apt-get update
      # The Linux-hosted Tauri CLI inspects enabled tray features while preparing
      # bundle settings, even when cargo-xwin targets a Windows NSIS installer.
      apt-get install -y --no-install-recommends \
        clang \
        libayatana-appindicator3-dev \
        lld \
        llvm \
        nsis \
        pkg-config
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
      nsis_dir=target/x86_64-pc-windows-msvc/release/bundle/nsis
      mkdir -p "$nsis_dir"
      find "$nsis_dir" -maxdepth 1 -type f -name "*-setup.exe" -delete
      cargo tauri build --runner cargo-xwin --target x86_64-pc-windows-msvc --bundles nsis

      installer="${nsis_dir}/iuna_${IUNA_VERSION}_x64-setup.exe"
      [ -f "$installer" ] || { echo "Windows installer for version ${IUNA_VERSION} was not produced at ${installer}" >&2; exit 1; }
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
  local builder_platform

  mkdir -p .docker-build downloads
  [ -f "downloads/${linux_x86_64_package}.tar.gz" ] \
    && [ -f "downloads/${linux_aarch64_package}.tar.gz" ] \
    && [ -f .docker-build/iuna-node-linux-x86_64 ] \
    && return 0

  require_command docker
  builder_platform="$(docker_native_linux_platform)"

  docker run --rm --pull=always --platform="$builder_platform" \
    -e "IUNA_VERSION=${version}" \
    -e "HOST_UID=$(id -u)" \
    -e "HOST_GID=$(id -g)" \
    -v "$(pwd):/src/iuna:ro" \
    -v "$(pwd)/downloads:/out" \
    -v "$(pwd)/.docker-build:/node-out" \
    rust:1.88-bookworm \
    bash -c '
      set -euo pipefail

      apt-get update
      case "$(uname -m)" in
        x86_64)
          apt-get install -y --no-install-recommends gcc-aarch64-linux-gnu libc6-dev-arm64-cross
          ;;
        aarch64|arm64)
          apt-get install -y --no-install-recommends gcc-x86-64-linux-gnu libc6-dev-amd64-cross
          ;;
        *)
          echo "unsupported Linux builder architecture: $(uname -m)" >&2
          exit 1
          ;;
      esac
      rm -rf /var/lib/apt/lists/*
      rustup target add aarch64-unknown-linux-gnu x86_64-unknown-linux-gnu

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
      CC_x86_64_unknown_linux_gnu=x86_64-linux-gnu-gcc \
      AR_x86_64_unknown_linux_gnu=x86_64-linux-gnu-ar \
      CARGO_TARGET_X86_64_UNKNOWN_LINUX_GNU_LINKER=x86_64-linux-gnu-gcc \
      cargo build --release --locked --target x86_64-unknown-linux-gnu

      tag="v${IUNA_VERSION}"
      linux_x86_64_package="iuna-${tag}-linux-x86_64"
      linux_aarch64_package="iuna-${tag}-linux-aarch64"
      mkdir -p "/tmp/site/${linux_x86_64_package}" "/tmp/site/${linux_aarch64_package}"
      cp target/x86_64-unknown-linux-gnu/release/iuna "/tmp/site/${linux_x86_64_package}/"
      cp target/aarch64-unknown-linux-gnu/release/iuna "/tmp/site/${linux_aarch64_package}/"
      cp target/x86_64-unknown-linux-gnu/release/iuna /node-out/iuna-node-linux-x86_64
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

website_image_for_head() {
  local commit

  require_command git
  commit="$(git rev-parse --short=12 HEAD)"
  printf '%s\n' "${IUNA_WWW_IMAGE:-iuna-www:git-${commit}}"
}

build_website_image() {
  local image="$1"

  require_command docker
  docker build --platform=linux/amd64 --progress=plain -t "$image" .
  echo "Built website image: ${image}"
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

deploy_website_image() {
  local image="$1"
  local kubectl_context="${IUNA_KUBECTL_CONTEXT:-jhx-app}"
  local tmp_folder

  require_command kubectl

  tmp_folder="$(mktemp -d)"
  trap 'rm -rf "$tmp_folder"' RETURN

  import_image_to_k3s "$image" "$tmp_folder"
  kubectl --context "$kubectl_context" -n iuna set image deployment/www "iuna-www=${image}"
  kubectl --context "$kubectl_context" -n iuna rollout restart deployment/www
  kubectl --context "$kubectl_context" -n iuna rollout status deployment/www
}

render_manifest() {
  local www_image="$1"
  local node_image="$2"
  local node_pvc="$3"
  local genesis="$4"
  local output="$5"
  local local_allowlist_file="config/admin-ip-allowlist.local"
  local allowlist_entry
  local allowlist_entry_count=0
  local escaped_www_image
  local escaped_node_image
  local escaped_node_pvc

  escaped_www_image="$(escape_sed_replacement "$www_image")"
  escaped_node_image="$(escape_sed_replacement "$node_image")"
  escaped_node_pvc="$(escape_sed_replacement "$node_pvc")"

  [ -f "$local_allowlist_file" ] || die "missing local admin allowlist: ${local_allowlist_file}"
  while IFS= read -r allowlist_entry || [ -n "$allowlist_entry" ]; do
    allowlist_entry="${allowlist_entry%%#*}"
    allowlist_entry="${allowlist_entry//[[:space:]]/}"
    [ -z "$allowlist_entry" ] && continue
    [[ "$allowlist_entry" =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}/(3[0-2]|[12]?[0-9])$ ]] || \
      die "invalid CIDR in ${local_allowlist_file}: ${allowlist_entry}"
    allowlist_entry_count=$((allowlist_entry_count + 1))
  done < "$local_allowlist_file"
  [ "$allowlist_entry_count" -gt 0 ] || die "local admin allowlist is empty: ${local_allowlist_file}"

  sed \
    -e "s|\${IUNA_WWW_IMAGE}|${escaped_www_image}|g" \
    -e "s|\${IUNA_NODE_IMAGE}|${escaped_node_image}|g" \
    -e "s|\${IUNA_NODE_PVC}|${escaped_node_pvc}|g" \
    config/deployment.yml | awk \
      -v local_allowlist_file="$local_allowlist_file" \
      -v genesis="$genesis" '
      $0 == "${IUNA_NODE_GENESIS_ARG}" {
        if (genesis == "true") print "            - --genesis"
        next
      }
      $0 == "${IUNA_ADMIN_IP_ALLOWLIST_LOCAL}" {
        while ((getline entry < local_allowlist_file) > 0) {
          sub(/#.*/, "", entry)
          gsub(/[[:space:]]/, "", entry)
          if (entry != "") print "      - " entry
        }
        close(local_allowlist_file)
        next
      }
      { print }
    ' > "$output"
}

deploy_docker_image() {
  local version="$1"
  local genesis="$2"
  local www_image="${IUNA_WWW_IMAGE:-iuna-www:v${version}}"
  local node_image="${IUNA_NODE_IMAGE:-iuna-node:v${version}}"
  local node_pvc="local-path-db-pvc"
  local kubectl_context="${IUNA_KUBECTL_CONTEXT:-jhx-app}"
  local tmp_folder

  require_command kubectl

  tmp_folder="$(mktemp -d)"
  trap 'rm -rf "$tmp_folder"' RETURN

  import_image_to_k3s "$www_image" "$tmp_folder"
  import_image_to_k3s "$node_image" "$tmp_folder"
  render_manifest "$www_image" "$node_image" "$node_pvc" "$genesis" "${tmp_folder}/deployment.yml"
  kubectl --context "$kubectl_context" apply -f config/traefik.yml

  if [ "$genesis" = "true" ]; then
    echo "WARNING: the existing chain is about to be permanently deleted."
    echo "Kubernetes context: ${kubectl_context}"
    echo "Namespace: iuna"
    echo "PVC: ${node_pvc}"
    if ! confirm "Are you absolutely sure? (Y/N) "; then
      echo "Deployment aborted; the existing node and PVC were not deleted"
      exit 1
    fi
    echo "Deleting the existing node and PVC ${node_pvc}"
    kubectl --context "$kubectl_context" -n iuna delete deployment node --ignore-not-found --wait=true
    kubectl --context "$kubectl_context" -n iuna delete pvc "$node_pvc" --ignore-not-found --wait=true
  fi

  local current_www_selector
  current_www_selector="$(kubectl --context "$kubectl_context" -n iuna get deployment www -o jsonpath='{.spec.selector.matchLabels.app}' 2>/dev/null || true)"
  if [ -n "$current_www_selector" ] && [ "$current_www_selector" != "iuna-www" ]; then
    kubectl --context "$kubectl_context" -n iuna delete deployment www --wait=true
  fi

  kubectl --context "$kubectl_context" apply -f "${tmp_folder}/deployment.yml"
  kubectl --context "$kubectl_context" -n iuna rollout restart deployment/www deployment/node
  kubectl --context "$kubectl_context" -n iuna rollout status deployment/www
  kubectl --context "$kubectl_context" -n iuna rollout status deployment/node

  if [ "$genesis" = "true" ]; then
    echo "Genesis started successfully; removing --genesis for subsequent pod starts"
    render_manifest "$www_image" "$node_image" "$node_pvc" false "${tmp_folder}/deployment.yml"
    kubectl --context "$kubectl_context" apply -f "${tmp_folder}/deployment.yml"
    kubectl --context "$kubectl_context" -n iuna rollout status deployment/node
  fi
}

main() {
  local genesis=false
  local skip_long_tests=false
  local website_only=false
  local version=""

  while [ "$#" -gt 0 ]; do
    case "$1" in
      --genesis)
        [ "$genesis" = "false" ] || die "--genesis may only be specified once"
        genesis=true
        ;;
      --skip-long-tests)
        [ "$skip_long_tests" = "false" ] || die "--skip-long-tests may only be specified once"
        skip_long_tests=true
        ;;
      --website-only)
        [ "$website_only" = "false" ] || die "--website-only may only be specified once"
        website_only=true
        ;;
      -h|--help)
        usage
        exit 0
        ;;
      -*)
        die "unknown option: $1"
        ;;
      *)
        [ -z "$version" ] || { usage >&2; exit 2; }
        version="${1#v}"
        ;;
    esac
    shift
  done

  if [ "$website_only" = "true" ]; then
    [ -z "$version" ] || die "--website-only does not accept a version"
    [ "$genesis" = "false" ] || die "--website-only cannot be combined with --genesis"
    [ "$skip_long_tests" = "false" ] || die "--website-only cannot be combined with --skip-long-tests"

    ensure_clean_worktree

    local website_image
    website_image="$(website_image_for_head)"
    echo "Website-only deployment from commit $(git rev-parse --short HEAD)"
    echo "Image: ${website_image}"
    if ! confirm "Are you sure you want to deploy the website? (y/N) "; then
      echo "Aborting deployment"
      exit 1
    fi
    build_website_image "$website_image"
    deploy_website_image "$website_image"
    exit 0
  fi

  [ -n "$version" ] || { usage >&2; exit 2; }
  [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "version must look like 0.2.48"

  ensure_clean_worktree

  if [ "$genesis" = "true" ]; then
    echo "WARNING: this starts a new chain with --genesis and a fresh PVC."
    echo "The existing chain in PVC local-path-db-pvc will be PERMANENTLY DELETED."
    echo "An empty PVC will then be created with the same permanent name."
    if ! confirm "Are you sure? (Y/N) "; then
      echo "Deployment aborted"
      exit 1
    fi
  fi

  # Check if the tag already exists; if it does, only deploy
  if git rev-parse --verify "v${version}" >/dev/null 2>&1; then
    ensure_head_matches_tag "v${version}"
    echo "Tag v${version} already exists; rebuilding Docker images and deploying"
    if [ "$genesis" != "true" ] && ! confirm "Are you sure you want to deploy v${version}? (y/N) "; then
      echo "Aborting deployment"
      exit 1
    fi
    run_release_tests "$skip_long_tests"
    build_linux_cli_archives "$version"
    build_docker_image "$version"
    deploy_docker_image "$version" "$genesis"
    exit 0
  fi

  update_versions "$version"
  run_release_tests "$skip_long_tests"
  build_versions "$version"
  commit_and_tag "$version"
  build_docker_image "$version"
  deploy_docker_image "$version" "$genesis"
}

if [[ "${BASH_SOURCE[0]}" == "$0" ]]; then
  main "$@"
fi
