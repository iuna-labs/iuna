use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::Path,
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::domain::Wallet;

const WALLET_FILE_VERSION: u32 = 2;

#[derive(Debug, Serialize, Deserialize)]
struct WalletFile {
    version: u32,
    seed: String,
    address: String,
}

pub fn load_or_create(path: &Path) -> Result<Wallet> {
    if path.exists() {
        return load(path);
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create wallet directory {}", parent.display()))?;
    }

    let seed = random_seed()?;
    let wallet = Wallet::from_seed(&seed);
    let mut file = create_wallet_file(path)?;
    write_wallet_file(&mut file, seed, wallet.address())
        .with_context(|| format!("failed to write wallet file {}", path.display()))?;

    Ok(wallet)
}

fn load(path: &Path) -> Result<Wallet> {
    let bytes =
        fs::read(path).with_context(|| format!("failed to read wallet file {}", path.display()))?;
    let stored: WalletFile = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse wallet file {}", path.display()))?;

    let wallet = Wallet::from_seed(&stored.seed);
    if stored.version == 1 {
        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(path)
            .with_context(|| format!("failed to migrate wallet file {}", path.display()))?;
        write_wallet_file(&mut file, stored.seed, wallet.address())
            .with_context(|| format!("failed to migrate wallet file {}", path.display()))?;
        return Ok(wallet);
    }
    if stored.version != WALLET_FILE_VERSION {
        bail!(
            "unsupported wallet file version {} in {}",
            stored.version,
            path.display()
        );
    }
    if wallet.address() != stored.address {
        bail!(
            "wallet file {} has address {}, but its seed derives {}",
            path.display(),
            stored.address,
            wallet.address()
        );
    }

    Ok(wallet)
}

fn write_wallet_file(file: &mut File, seed: String, address: &str) -> Result<()> {
    let stored = WalletFile {
        version: WALLET_FILE_VERSION,
        seed,
        address: address.to_string(),
    };
    let bytes = serde_json::to_vec_pretty(&stored).context("failed to serialize wallet file")?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn random_seed() -> Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::getrandom(&mut bytes)
        .map_err(|error| anyhow!("failed to read system randomness: {error:?}"))?;
    Ok(hex_encode(&bytes))
}

fn create_wallet_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    options
        .open(path)
        .with_context(|| format!("failed to create wallet file {}", path.display()))
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::load_or_create;

    #[test]
    fn creates_and_reuses_wallet_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("wallet.json");

        let first = load_or_create(&path).unwrap();
        let second = load_or_create(&path).unwrap();

        assert_eq!(first.address(), second.address());
        let stored = fs::read_to_string(path).unwrap();
        assert!(stored.contains(first.address()));
        assert!(!stored.contains("dev-wallet"));
    }

    #[test]
    fn migrates_v1_wallet_file_to_current_address() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("wallet.json");
        fs::write(&path, r#"{"version":1,"seed":"alice","address":"old"}"#).unwrap();

        let wallet = load_or_create(&path).unwrap();
        let stored = fs::read_to_string(path).unwrap();

        assert!(stored.contains("\"version\": 2"));
        assert!(stored.contains(wallet.address()));
    }

    #[test]
    fn rejects_seed_address_mismatch() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("wallet.json");
        fs::write(
            &path,
            r#"{"version":2,"seed":"alice","address":"mv_wrong"}"#,
        )
        .unwrap();

        let error = load_or_create(&path).unwrap_err();

        assert!(error.to_string().contains("seed derives"));
    }
}
