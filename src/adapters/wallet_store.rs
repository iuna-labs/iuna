use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::Path,
};

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::domain::Wallet;

const WALLET_FILE_VERSION: u32 = 2;
const GENERATED_SEED_WORDS: usize = 24;
const SEED_WORDS: &[&str] = &[
    "able", "acid", "acorn", "adapt", "agent", "anchor", "angle", "apple", "asset", "atlas",
    "badge", "balance", "beacon", "benefit", "binary", "bitter", "blanket", "border", "brave",
    "bright", "broker", "budget", "cactus", "canvas", "carbon", "castle", "census", "circle",
    "citizen", "clerk", "climate", "coffee", "copper", "corner", "cotton", "cradle", "credit",
    "crisp", "custom", "damage", "decade", "degree", "delight", "delta", "device", "dinner",
    "direct", "domain", "donor", "driver", "dynamic", "eager", "early", "earth", "echo", "economy",
    "edge", "effort", "elder", "ember", "enable", "engine", "equal", "estate", "fabric", "factor",
    "famous", "father", "feature", "federal", "fiction", "filter", "finger", "finish", "flame",
    "forest", "forum", "frost", "future", "galaxy", "garden", "gentle", "giant", "glass", "globe",
    "golden", "grain", "gravity", "green", "harbor", "hazard", "honest", "human", "humble",
    "iceberg", "impact", "income", "index", "infant", "island", "jacket", "jewel", "journal",
    "junior", "kernel", "ladder", "language", "laser", "leader", "legend", "lemon", "level",
    "liberty", "linear", "local", "lottery", "lunar", "magnet", "market", "matrix", "member",
    "memory", "merit", "method", "middle", "minute", "mirror", "model", "moment", "native",
    "network", "neutral", "noble", "normal", "notice", "novel", "object", "ocean", "offer",
    "olive", "orbit", "origin", "oxygen", "packet", "panel", "parent", "pattern", "people",
    "pepper", "permit", "planet", "plastic", "policy", "postal", "prefer", "profit", "public",
    "puzzle", "quality", "quantum", "quiet", "random", "rapid", "radius", "reason", "record",
    "region", "repair", "reward", "ribbon", "rocket", "sample", "scale", "scheme", "secret",
    "sector", "select", "senior", "shadow", "signal", "silver", "simple", "sister", "sketch",
    "social", "solar", "source", "spare", "spirit", "stable", "station", "stone", "street",
    "summer", "supply", "system", "talent", "target", "temple", "tenant", "theory", "ticket",
    "timber", "token", "travel", "treat", "trust", "tunnel", "twelve", "unique", "update",
    "useful", "valid", "valley", "vendor", "verify", "victory", "village", "virtual", "volume",
    "wallet", "window", "winter", "wonder", "yellow", "zero",
];

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

    let seed = generate_seed_phrase()?;
    let wallet = Wallet::from_seed(&seed);
    let mut file = create_wallet_file(path)?;
    write_wallet_file(&mut file, seed, wallet.address())
        .with_context(|| format!("failed to write wallet file {}", path.display()))?;

    Ok(wallet)
}

pub fn replace_with_generated_seed_phrase(path: &Path) -> Result<(Wallet, String)> {
    let seed = generate_seed_phrase()?;
    let wallet = write_wallet(path, seed.clone(), WalletFileMode::Replace)?;
    Ok((wallet, seed))
}

pub fn replace_with_imported_seed_phrase(path: &Path, seed_phrase: &str) -> Result<Wallet> {
    let seed = normalize_seed_phrase(seed_phrase)?;
    write_wallet(path, seed, WalletFileMode::Replace)
}

pub fn setup_seed_phrase(path: &Path) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let stored = read_wallet_file(path)?;
    let normalized = match normalize_seed_phrase(&stored.seed) {
        Ok(seed) => seed,
        Err(_) => return Ok(None),
    };
    if normalized == stored.seed {
        Ok(Some(normalized))
    } else {
        Ok(None)
    }
}

fn load(path: &Path) -> Result<Wallet> {
    let stored = read_wallet_file(path)?;

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

fn read_wallet_file(path: &Path) -> Result<WalletFile> {
    let bytes =
        fs::read(path).with_context(|| format!("failed to read wallet file {}", path.display()))?;
    serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse wallet file {}", path.display()))
}

enum WalletFileMode {
    CreateNew,
    Replace,
}

fn write_wallet(path: &Path, seed: String, mode: WalletFileMode) -> Result<Wallet> {
    let wallet = Wallet::from_seed(&seed);
    let mut file = open_wallet_file(path, mode)?;
    write_wallet_file(&mut file, seed, wallet.address())
        .with_context(|| format!("failed to write wallet file {}", path.display()))?;
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

fn generate_seed_phrase() -> Result<String> {
    let mut bytes = [0_u8; GENERATED_SEED_WORDS];
    getrandom::getrandom(&mut bytes)
        .map_err(|error| anyhow!("failed to read system randomness: {error:?}"))?;
    let words = bytes
        .iter()
        .map(|byte| SEED_WORDS[usize::from(*byte) % SEED_WORDS.len()])
        .collect::<Vec<_>>();
    Ok(words.join(" "))
}

fn normalize_seed_phrase(seed_phrase: &str) -> Result<String> {
    let words = seed_phrase
        .split_whitespace()
        .map(|word| word.trim().to_ascii_lowercase())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>();
    if words.len() != GENERATED_SEED_WORDS {
        bail!("seed phrase must contain 24 words");
    }
    for word in &words {
        if !word.chars().all(|ch| ch.is_ascii_lowercase()) {
            bail!("seed phrase words must contain only letters");
        }
    }
    Ok(words.join(" "))
}

fn create_wallet_file(path: &Path) -> Result<File> {
    open_wallet_file(path, WalletFileMode::CreateNew)
}

fn open_wallet_file(path: &Path, mode: WalletFileMode) -> Result<File> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create wallet directory {}", parent.display()))?;
    }

    let mut options = OpenOptions::new();
    options.write(true);
    match mode {
        WalletFileMode::CreateNew => {
            options.create_new(true);
        }
        WalletFileMode::Replace => {
            options.create(true).truncate(true);
        }
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    options
        .open(path)
        .with_context(|| format!("failed to create wallet file {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{
        load_or_create, replace_with_generated_seed_phrase, replace_with_imported_seed_phrase,
        setup_seed_phrase,
    };

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
        assert!(
            setup_seed_phrase(&dir.path().join("wallet.json"))
                .unwrap()
                .is_some()
        );
    }

    #[test]
    fn generated_wallet_uses_recovery_phrase() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("wallet.json");

        let (_wallet, seed_phrase) = replace_with_generated_seed_phrase(&path).unwrap();
        let words = seed_phrase.split_whitespace().collect::<Vec<_>>();

        assert_eq!(words.len(), 24);
        assert_eq!(
            setup_seed_phrase(&path).unwrap().as_deref(),
            Some(seed_phrase.as_str())
        );
    }

    #[test]
    fn generated_verified_phrase_imports_to_same_wallet() {
        let dir = tempdir().unwrap();
        let generated_path = dir.path().join("generated-wallet.json");
        let imported_path = dir.path().join("imported-wallet.json");

        let (generated_wallet, seed_phrase) =
            replace_with_generated_seed_phrase(&generated_path).unwrap();
        assert_recovery_words_verify(&seed_phrase, &[0, 6, 13, 23]);

        let generated_loaded = load_or_create(&generated_path).unwrap();
        assert_eq!(generated_wallet.address(), generated_loaded.address());

        let imported_wallet =
            replace_with_imported_seed_phrase(&imported_path, &seed_phrase).unwrap();
        assert_recovery_words_verify(
            setup_seed_phrase(&imported_path)
                .unwrap()
                .as_deref()
                .unwrap(),
            &[0, 6, 13, 23],
        );

        let imported_loaded = load_or_create(&imported_path).unwrap();
        assert_eq!(generated_wallet.address(), imported_wallet.address());
        assert_eq!(generated_wallet.address(), imported_loaded.address());
        assert_eq!(
            setup_seed_phrase(&generated_path).unwrap(),
            setup_seed_phrase(&imported_path).unwrap()
        );
    }

    #[test]
    fn imports_normalized_seed_phrase() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("wallet.json");

        let wallet = replace_with_imported_seed_phrase(
            &path,
            " Able  ACID acorn adapt agent anchor angle apple asset atlas badge balance beacon benefit binary bitter blanket border brave bright broker budget cactus canvas ",
        )
        .unwrap();
        let loaded = load_or_create(&path).unwrap();

        assert_eq!(wallet.address(), loaded.address());
        assert_eq!(
            setup_seed_phrase(&path).unwrap().as_deref(),
            Some(
                "able acid acorn adapt agent anchor angle apple asset atlas badge balance beacon benefit binary bitter blanket border brave bright broker budget cactus canvas"
            )
        );
    }

    #[test]
    fn rejects_invalid_imported_seed_phrase() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("wallet.json");

        let error = replace_with_imported_seed_phrase(&path, "too few words").unwrap_err();

        assert!(error.to_string().contains("24 words"));
    }

    fn assert_recovery_words_verify(seed_phrase: &str, indexes: &[usize]) {
        let words = seed_phrase.split_whitespace().collect::<Vec<_>>();
        assert_eq!(words.len(), 24);
        for index in indexes {
            let answer = words[*index].to_ascii_uppercase();
            assert_eq!(
                answer.trim().to_ascii_lowercase(),
                words[*index],
                "word {} should verify case-insensitively",
                index + 1
            );
        }
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
