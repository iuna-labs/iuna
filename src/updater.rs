use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::Path,
};

use anyhow::{Context, Result, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use flate2::read::GzDecoder;
use minisign_verify::{PublicKey, Signature};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const RELEASE_METADATA_URL: &str = "https://getiuna.org/downloads/latest.json";
const UPDATE_PUBLIC_KEY: &str = include_str!("../config/update-signing.key.pub");
const MAX_UPDATE_BYTES: u64 = 256 * 1024 * 1024;

#[derive(Debug, Deserialize)]
struct ReleaseMetadata {
    version: String,
    #[serde(default)]
    artifacts: BTreeMap<String, ReleaseArtifact>,
}

#[derive(Debug, Deserialize)]
struct ReleaseArtifact {
    url: String,
    sha256: String,
    signature: String,
}

pub(crate) async fn handle_cli_command() -> Result<bool> {
    let mut args = std::env::args().skip(1);
    let Some(command) = args.next() else {
        return Ok(false);
    };

    if matches!(command.as_str(), "--version" | "-V") {
        if args.next().is_some() {
            bail!("--version does not accept additional arguments");
        }
        println!("iuna {}", env!("CARGO_PKG_VERSION"));
        return Ok(true);
    }

    if command != "update" {
        return Ok(false);
    }

    let check_only = match args.next().as_deref() {
        None => false,
        Some("--check") => true,
        Some(other) => bail!("unknown update option {other}; use `iuna update [--check]`"),
    };
    if let Some(other) = args.next() {
        bail!("unexpected update argument {other}; use `iuna update [--check]`");
    }

    let release = fetch_release_metadata().await?;
    let current = Version::parse(env!("CARGO_PKG_VERSION")).context("invalid built-in version")?;
    let available = Version::parse(release.version.trim_start_matches('v'))
        .context("release metadata contains an invalid version")?;

    if available <= current {
        println!("iuna v{current} is up to date");
        return Ok(true);
    }

    println!("iuna v{available} is available (currently v{current})");
    if check_only {
        return Ok(true);
    }

    install_update(&release, &available).await?;
    println!("updated iuna to v{available}; restart any running iuna service");
    Ok(true)
}

async fn fetch_release_metadata() -> Result<ReleaseMetadata> {
    let response = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?
        .get(RELEASE_METADATA_URL)
        .header(reqwest::header::ACCEPT, "application/json")
        .send()
        .await
        .context("could not fetch iuna release metadata")?
        .error_for_status()
        .context("iuna release metadata request failed")?;

    response
        .json()
        .await
        .context("could not decode iuna release metadata")
}

async fn install_update(release: &ReleaseMetadata, version: &Version) -> Result<()> {
    let target = update_target()?;
    let artifact = release
        .artifacts
        .get(target)
        .with_context(|| format!("release v{version} has no artifact for {target}"))?;
    let archive = download_artifact(artifact).await?;
    verify_artifact(&archive, artifact)?;

    let current_exe =
        std::env::current_exe().context("could not locate the running iuna binary")?;
    replace_executable(&current_exe, &archive, version)
}

async fn download_artifact(artifact: &ReleaseArtifact) -> Result<Vec<u8>> {
    let response = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()?
        .get(&artifact.url)
        .send()
        .await
        .context("could not download the iuna update")?
        .error_for_status()
        .context("iuna update download failed")?;

    if response
        .content_length()
        .is_some_and(|size| size > MAX_UPDATE_BYTES)
    {
        bail!("iuna update is larger than the allowed 256 MiB");
    }
    let bytes = response
        .bytes()
        .await
        .context("could not read the iuna update")?;
    if bytes.len() as u64 > MAX_UPDATE_BYTES {
        bail!("iuna update is larger than the allowed 256 MiB");
    }
    Ok(bytes.to_vec())
}

fn verify_artifact(bytes: &[u8], artifact: &ReleaseArtifact) -> Result<()> {
    let actual_hash = format!("{:x}", Sha256::digest(bytes));
    if !actual_hash.eq_ignore_ascii_case(&artifact.sha256) {
        bail!("iuna update checksum verification failed");
    }

    let public_key_text = decode_tauri_signature(UPDATE_PUBLIC_KEY)
        .context("the embedded iuna update public key is invalid")?;
    let public_key = PublicKey::decode(&public_key_text)
        .context("the embedded iuna update public key is invalid")?;
    let signature_text = decode_tauri_signature(&artifact.signature)
        .context("the iuna update signature is invalid")?;
    let signature =
        Signature::decode(&signature_text).context("the iuna update signature is invalid")?;
    public_key
        .verify(bytes, &signature, true)
        .context("iuna update signature verification failed")
}

fn decode_tauri_signature(encoded: &str) -> Result<String> {
    let decoded = BASE64
        .decode(encoded.trim())
        .context("invalid base64 in Tauri signature")?;
    String::from_utf8(decoded).context("Tauri signature is not UTF-8")
}

fn replace_executable(current_exe: &Path, archive: &[u8], version: &Version) -> Result<()> {
    let parent = current_exe
        .parent()
        .context("the running iuna binary has no parent directory")?;
    let staged = parent.join(format!(".iuna-update-{version}-{}", std::process::id()));
    let result = stage_binary(&staged, archive).and_then(|_| {
        fs::rename(&staged, current_exe).with_context(|| {
            format!(
                "cannot replace {}; install iuna in a writable directory or run the update with sufficient permissions",
                current_exe.display()
            )
        })
    });
    if result.is_err() {
        let _ = fs::remove_file(&staged);
    }
    result
}

fn stage_binary(destination: &Path, archive: &[u8]) -> Result<()> {
    let decoder = GzDecoder::new(archive);
    let mut tar = tar::Archive::new(decoder);
    let mut found = false;

    for entry in tar
        .entries()
        .context("could not read the iuna update archive")?
    {
        let mut entry = entry.context("could not read an iuna update archive entry")?;
        let path = entry
            .path()
            .context("invalid path in iuna update archive")?;
        if path.file_name().and_then(|name| name.to_str()) != Some("iuna")
            || !entry.header().entry_type().is_file()
        {
            continue;
        }
        if found {
            bail!("iuna update archive contains multiple binaries");
        }

        let mut output = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(destination)
            .with_context(|| format!("cannot stage update at {}", destination.display()))?;
        let mut buffer = [0_u8; 64 * 1024];
        loop {
            let count = entry.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            output.write_all(&buffer[..count])?;
        }
        output.sync_all()?;
        found = true;
    }

    if !found {
        bail!("iuna update archive does not contain an iuna binary");
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

fn update_target() -> Result<&'static str> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    return Ok("linux-x86_64");
    #[cfg(all(target_os = "linux", target_arch = "aarch64"))]
    return Ok("linux-aarch64");
    #[cfg(not(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "aarch64")
    )))]
    bail!("automatic CLI updates are not available for this platform")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_metadata_remains_compatible_with_original_shape() {
        let metadata: ReleaseMetadata = serde_json::from_str(
            r#"{"tag":"v0.4.29","version":"0.4.29","url":"https://getiuna.org/downloads/"}"#,
        )
        .unwrap();
        assert_eq!(metadata.version, "0.4.29");
        assert!(metadata.artifacts.is_empty());
    }

    #[test]
    fn archive_without_binary_is_rejected() {
        let mut encoded = Vec::new();
        {
            let encoder =
                flate2::write::GzEncoder::new(&mut encoded, flate2::Compression::default());
            let mut archive = tar::Builder::new(encoder);
            let mut header = tar::Header::new_gnu();
            header.set_size(4);
            header.set_cksum();
            archive
                .append_data(&mut header, "README.md", &b"test"[..])
                .unwrap();
            archive.into_inner().unwrap().finish().unwrap();
        }
        let temp = tempfile::tempdir().unwrap();
        let error = stage_binary(&temp.path().join("iuna"), &encoded).unwrap_err();
        assert!(error.to_string().contains("does not contain"));
    }

    #[test]
    fn embedded_public_key_verifies_tauri_signature_format() {
        const SIGNATURE: &str = "dW50cnVzdGVkIGNvbW1lbnQ6IHNpZ25hdHVyZSBmcm9tIHRhdXJpIHNlY3JldCBrZXkKUlVRVDljclZTTXNMbzZUQ3cwMUNibXF4bHhBZ0Vka0pCREhXS1VFWmNsbVc4ejVsUzZaN2ZNQ1VoVkwyV05nMGF2dzMrNjNMUlB5Wm1WS2JnbG4xVXhjV2s0N3dsMWx5aGdNPQp0cnVzdGVkIGNvbW1lbnQ6IHRpbWVzdGFtcDoxNzg5MDQwNjQ0CWZpbGU6dG1wLklaQThVTDRXOEEKRldRWG9UaXplbFAwVk5CUllNSHJDSSsyM2dLaXBPTXA0UWNOc0xtLyt2d2lLdTgvSmM3dWRZZmpqTkl2Q3RCMTlEWEx5dVNUK0gxeHlJQXY2VWhNQWc9PQo=";
        let artifact = ReleaseArtifact {
            url: "https://getiuna.org/test".to_string(),
            sha256: format!("{:x}", Sha256::digest(b"iuna updater test vector")),
            signature: SIGNATURE.to_string(),
        };
        verify_artifact(b"iuna updater test vector", &artifact).unwrap();
    }
}
