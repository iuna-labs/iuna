use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::Path,
};

use anyhow::{Context, Result, anyhow, bail};
use bip39::{Language, Mnemonic};
use chacha20poly1305::{
    ChaCha20Poly1305, KeyInit, Nonce,
    aead::{Aead, Payload},
};
use pbkdf2::pbkdf2_hmac;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::domain::{OwnedBlindedTransaction, Wallet};

const WALLET_FILE_VERSION: u32 = 3;
const PLAINTEXT_WALLET_FILE_VERSION: u32 = 2;
const WALLET_ENCRYPTION_ALGORITHM: &str = "chacha20poly1305";
const WALLET_ENCRYPTION_KDF: &str = "pbkdf2-sha256";
const WALLET_ENCRYPTION_ITERATIONS: u32 = 210_000;
const GENERATED_SEED_WORDS: usize = 24;
const BIP39_SEED_ENTROPY_BYTES: usize = 32;

#[derive(Debug, Serialize, Deserialize)]
struct WalletFile {
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seed: Option<String>,
    address: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    owned_blinded_transactions: Vec<OwnedBlindedTransaction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    encryption: Option<EncryptedWalletSeed>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct WalletData {
    seed: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    owned_blinded_transactions: Vec<OwnedBlindedTransaction>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WalletMetadata {
    pub address: String,
    pub encrypted: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct EncryptedWalletSeed {
    algorithm: String,
    kdf: String,
    kdf_iterations: u32,
    salt: String,
    nonce: String,
    ciphertext: String,
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

pub fn replace_with_generated_seed_phrase_encrypted(
    path: &Path,
    password: &str,
) -> Result<(Wallet, String)> {
    let seed = generate_seed_phrase()?;
    let wallet = write_wallet_encrypted(path, seed.clone(), password, WalletFileMode::Replace)?;
    Ok((wallet, seed))
}

pub fn replace_with_imported_seed_phrase(path: &Path, seed_phrase: &str) -> Result<Wallet> {
    let seed = normalize_seed_phrase(seed_phrase)?;
    write_wallet(path, seed, WalletFileMode::Replace)
}

pub fn replace_with_imported_seed_phrase_encrypted(
    path: &Path,
    seed_phrase: &str,
    password: &str,
) -> Result<Wallet> {
    let seed = normalize_seed_phrase(seed_phrase)?;
    write_wallet_encrypted(path, seed, password, WalletFileMode::Replace)
}

pub fn setup_seed_phrase(path: &Path) -> Result<Option<String>> {
    setup_seed_phrase_with_password(path, None)
}

pub fn setup_seed_phrase_with_password(
    path: &Path,
    password: Option<&str>,
) -> Result<Option<String>> {
    if !path.exists() {
        return Ok(None);
    }
    let stored = read_wallet_file(path)?;
    let seed = match wallet_seed(&stored, password) {
        Ok(seed) => seed,
        Err(_) => return Ok(None),
    };
    let normalized = match normalize_seed_phrase(&seed) {
        Ok(seed) => seed,
        Err(_) => return Ok(None),
    };
    if normalized == seed {
        Ok(Some(normalized))
    } else {
        Ok(None)
    }
}

pub fn metadata(path: &Path) -> Result<Option<WalletMetadata>> {
    if !path.exists() {
        return Ok(None);
    }
    let stored = read_wallet_file(path)?;
    Ok(Some(WalletMetadata {
        address: stored.address,
        encrypted: stored.encryption.is_some(),
    }))
}

pub fn load_with_password(path: &Path, password: &str) -> Result<Wallet> {
    load_encrypted_or_plaintext(path, Some(password))
}

pub fn load_owned_blinded_transactions(
    path: &Path,
    password: Option<&str>,
) -> Result<Vec<OwnedBlindedTransaction>> {
    let stored = read_wallet_file(path)?;
    Ok(wallet_data(&stored, password)?.owned_blinded_transactions)
}

pub fn replace_owned_blinded_transactions(
    path: &Path,
    password: Option<&str>,
    owned_blinded_transactions: Vec<OwnedBlindedTransaction>,
) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let stored = read_wallet_file(path)?;
    let mut data = wallet_data(&stored, password)?;
    data.owned_blinded_transactions = owned_blinded_transactions;
    let mut file = open_wallet_file(path, WalletFileMode::Replace)?;
    if stored.encryption.is_some() {
        let password = password
            .context("wallet is encrypted; unlock it before persisting blinded transactions")?;
        write_encrypted_wallet_data_file(&mut file, data, &stored.address, password)
    } else {
        write_wallet_data_file(&mut file, data, &stored.address)
    }
    .with_context(|| format!("failed to update wallet file {}", path.display()))
}

pub fn encrypt_existing_with_password(path: &Path, password: &str) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let stored = read_wallet_file(path)?;
    if stored.encryption.is_some() {
        let _ = wallet_from_stored(&stored, Some(password))?;
        return Ok(());
    }
    let data = wallet_data(&stored, None)?;
    let seed = data.seed;
    let seed = normalize_seed_phrase(&seed).unwrap_or(seed);
    let wallet = Wallet::from_seed(&seed);
    if wallet.address() != stored.address {
        bail!(
            "wallet file has address {}, but its seed derives {}",
            stored.address,
            wallet.address()
        );
    }
    let mut file = open_wallet_file(path, WalletFileMode::Replace)?;
    write_encrypted_wallet_data_file(
        &mut file,
        WalletData {
            seed,
            owned_blinded_transactions: data.owned_blinded_transactions,
        },
        wallet.address(),
        password,
    )
    .with_context(|| format!("failed to encrypt wallet file {}", path.display()))
}

pub fn reencrypt_with_password(
    path: &Path,
    current_password: &str,
    new_password: &str,
) -> Result<Wallet> {
    let stored = read_wallet_file(path)?;
    let data = wallet_data(&stored, Some(current_password))?;
    let seed = data.seed;
    let seed = normalize_seed_phrase(&seed).unwrap_or(seed);
    let wallet = Wallet::from_seed(&seed);
    if wallet.address() != stored.address {
        bail!(
            "wallet file has address {}, but its seed derives {}",
            stored.address,
            wallet.address()
        );
    }
    let mut file = open_wallet_file(path, WalletFileMode::Replace)?;
    write_encrypted_wallet_data_file(
        &mut file,
        WalletData {
            seed,
            owned_blinded_transactions: data.owned_blinded_transactions,
        },
        wallet.address(),
        new_password,
    )
    .with_context(|| format!("failed to re-encrypt wallet file {}", path.display()))?;
    Ok(wallet)
}

fn load(path: &Path) -> Result<Wallet> {
    load_encrypted_or_plaintext(path, None)
}

fn load_encrypted_or_plaintext(path: &Path, password: Option<&str>) -> Result<Wallet> {
    let stored = read_wallet_file(path)?;
    let wallet = wallet_from_stored(&stored, password)?;
    if stored.version == 1 {
        let mut file = OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(path)
            .with_context(|| format!("failed to migrate wallet file {}", path.display()))?;
        let seed = stored
            .seed
            .context("legacy wallet file does not contain a seed")?;
        write_wallet_file(&mut file, seed, wallet.address())
            .with_context(|| format!("failed to migrate wallet file {}", path.display()))?;
        return Ok(wallet);
    }
    if stored.version != WALLET_FILE_VERSION && stored.version != PLAINTEXT_WALLET_FILE_VERSION {
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

fn wallet_from_stored(stored: &WalletFile, password: Option<&str>) -> Result<Wallet> {
    let seed = wallet_seed(stored, password)?;
    Ok(Wallet::from_seed(&seed))
}

fn wallet_seed(stored: &WalletFile, password: Option<&str>) -> Result<String> {
    if let Some(encryption) = &stored.encryption {
        let password = password.context("wallet is encrypted; unlock it with the UI password")?;
        return decrypt_seed(encryption, &stored.address, password);
    }
    stored
        .seed
        .clone()
        .context("wallet file does not contain a seed")
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

fn write_wallet_encrypted(
    path: &Path,
    seed: String,
    password: &str,
    mode: WalletFileMode,
) -> Result<Wallet> {
    let wallet = Wallet::from_seed(&seed);
    let mut file = open_wallet_file(path, mode)?;
    write_encrypted_wallet_file(&mut file, seed, wallet.address(), password)
        .with_context(|| format!("failed to write wallet file {}", path.display()))?;
    Ok(wallet)
}

fn write_wallet_file(file: &mut File, seed: String, address: &str) -> Result<()> {
    write_wallet_data_file(
        file,
        WalletData {
            seed,
            owned_blinded_transactions: Vec::new(),
        },
        address,
    )
}

fn write_wallet_data_file(file: &mut File, data: WalletData, address: &str) -> Result<()> {
    let stored = WalletFile {
        version: PLAINTEXT_WALLET_FILE_VERSION,
        seed: Some(data.seed),
        address: address.to_string(),
        owned_blinded_transactions: data.owned_blinded_transactions,
        encryption: None,
    };
    let bytes = serde_json::to_vec_pretty(&stored).context("failed to serialize wallet file")?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn write_encrypted_wallet_file(
    file: &mut File,
    seed: String,
    address: &str,
    password: &str,
) -> Result<()> {
    write_encrypted_wallet_data_file(
        file,
        WalletData {
            seed,
            owned_blinded_transactions: Vec::new(),
        },
        address,
        password,
    )
}

fn write_encrypted_wallet_data_file(
    file: &mut File,
    data: WalletData,
    address: &str,
    password: &str,
) -> Result<()> {
    let encryption = encrypt_wallet_data(&data, address, password)?;
    let stored = WalletFile {
        version: WALLET_FILE_VERSION,
        seed: None,
        address: address.to_string(),
        owned_blinded_transactions: Vec::new(),
        encryption: Some(encryption),
    };
    let bytes = serde_json::to_vec_pretty(&stored).context("failed to serialize wallet file")?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn wallet_data(stored: &WalletFile, password: Option<&str>) -> Result<WalletData> {
    if let Some(encryption) = &stored.encryption {
        let password = password.context("wallet is encrypted; unlock it with the UI password")?;
        return decrypt_wallet_data(encryption, &stored.address, password);
    }
    let seed = stored
        .seed
        .clone()
        .context("wallet file does not contain a seed")?;
    Ok(WalletData {
        seed,
        owned_blinded_transactions: stored.owned_blinded_transactions.clone(),
    })
}

fn encrypt_wallet_data(
    data: &WalletData,
    address: &str,
    password: &str,
) -> Result<EncryptedWalletSeed> {
    let salt = random_bytes::<16>()?;
    let nonce = random_bytes::<12>()?;
    let key = wallet_encryption_key(password, &salt, WALLET_ENCRYPTION_ITERATIONS);
    let cipher = ChaCha20Poly1305::new((&key).into());
    let plaintext =
        serde_json::to_vec(data).context("failed to serialize encrypted wallet data")?;
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &plaintext,
                aad: address.as_bytes(),
            },
        )
        .map_err(|_| anyhow!("failed to encrypt wallet seed"))?;
    Ok(EncryptedWalletSeed {
        algorithm: WALLET_ENCRYPTION_ALGORITHM.to_string(),
        kdf: WALLET_ENCRYPTION_KDF.to_string(),
        kdf_iterations: WALLET_ENCRYPTION_ITERATIONS,
        salt: hex_encode(salt),
        nonce: hex_encode(nonce),
        ciphertext: hex_encode(ciphertext),
    })
}

fn decrypt_seed(encryption: &EncryptedWalletSeed, address: &str, password: &str) -> Result<String> {
    Ok(decrypt_wallet_data(encryption, address, password)?.seed)
}

fn decrypt_wallet_data(
    encryption: &EncryptedWalletSeed,
    address: &str,
    password: &str,
) -> Result<WalletData> {
    if encryption.algorithm != WALLET_ENCRYPTION_ALGORITHM {
        bail!("unsupported wallet encryption algorithm");
    }
    if encryption.kdf != WALLET_ENCRYPTION_KDF {
        bail!("unsupported wallet encryption kdf");
    }
    let salt = decode_hex(&encryption.salt).context("invalid wallet encryption salt")?;
    let nonce = decode_hex(&encryption.nonce).context("invalid wallet encryption nonce")?;
    let ciphertext = decode_hex(&encryption.ciphertext).context("invalid wallet encrypted seed")?;
    if nonce.len() != 12 {
        bail!("invalid wallet encryption nonce length");
    }
    let key = wallet_encryption_key(password, &salt, encryption.kdf_iterations);
    let cipher = ChaCha20Poly1305::new((&key).into());
    let plaintext = cipher
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: &ciphertext,
                aad: address.as_bytes(),
            },
        )
        .map_err(|_| anyhow!("invalid wallet password"))?;
    match serde_json::from_slice::<WalletData>(&plaintext) {
        Ok(data) => Ok(data),
        Err(_) => Ok(WalletData {
            seed: String::from_utf8(plaintext).context("wallet seed is not valid utf-8")?,
            owned_blinded_transactions: Vec::new(),
        }),
    }
}

fn wallet_encryption_key(password: &str, salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut key = [0_u8; 32];
    pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, iterations, &mut key);
    key
}

fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0_u8; N];
    getrandom::getrandom(&mut bytes)
        .map_err(|error| anyhow!("failed to read system randomness: {error:?}"))?;
    Ok(bytes)
}

fn generate_seed_phrase() -> Result<String> {
    let mut entropy = [0_u8; BIP39_SEED_ENTROPY_BYTES];
    getrandom::getrandom(&mut entropy)
        .map_err(|error| anyhow!("failed to read system randomness: {error:?}"))?;
    let mnemonic = Mnemonic::from_entropy_in(Language::English, &entropy)
        .context("failed to generate BIP-39 seed phrase")?;
    Ok(mnemonic.to_string())
}

fn hex_encode(bytes: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.as_ref().len() * 2);
    for byte in bytes.as_ref() {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

fn decode_hex(input: &str) -> Result<Vec<u8>> {
    if input.len() % 2 != 0 {
        bail!("hex string has odd length");
    }
    let mut bytes = Vec::with_capacity(input.len() / 2);
    for pair in input.as_bytes().chunks_exact(2) {
        let high = decode_hex_nibble(pair[0])?;
        let low = decode_hex_nibble(pair[1])?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn decode_hex_nibble(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => bail!("invalid hex character"),
    }
}

fn normalize_seed_phrase(seed_phrase: &str) -> Result<String> {
    let normalized = seed_phrase
        .split_whitespace()
        .map(|word| word.trim().to_ascii_lowercase())
        .filter(|word| !word.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if normalized.split_whitespace().count() != GENERATED_SEED_WORDS {
        bail!("seed phrase must contain 24 words");
    }
    for word in normalized.split_whitespace() {
        if !word.chars().all(|ch| ch.is_ascii_lowercase()) {
            bail!("seed phrase words must contain only letters");
        }
    }
    let mnemonic = Mnemonic::parse_in_normalized(Language::English, &normalized)
        .context("invalid BIP-39 seed phrase")?;
    Ok(mnemonic.to_string())
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

    use bip39::{Language, Mnemonic};
    use tempfile::tempdir;

    use crate::domain::{
        BlindedReveal, BlindedTransaction, OwnedBlindedTransaction, Transaction, TxInput, TxOutput,
    };

    use super::{
        encrypt_existing_with_password, load_or_create, load_owned_blinded_transactions,
        load_with_password, metadata, replace_owned_blinded_transactions,
        replace_with_generated_seed_phrase, replace_with_generated_seed_phrase_encrypted,
        replace_with_imported_seed_phrase, setup_seed_phrase, setup_seed_phrase_with_password,
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
        assert!(Mnemonic::parse_in_normalized(Language::English, &seed_phrase).is_ok());
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
    fn encrypted_generated_wallet_hides_seed_and_requires_password() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("wallet.json");

        let (wallet, seed_phrase) =
            replace_with_generated_seed_phrase_encrypted(&path, "correct horse battery staple")
                .unwrap();
        let stored = fs::read_to_string(&path).unwrap();

        assert!(stored.contains("\"version\": 3"));
        assert!(stored.contains("\"encryption\""));
        assert!(!stored.contains(&seed_phrase));
        assert_eq!(metadata(&path).unwrap().unwrap().address, wallet.address());
        assert!(metadata(&path).unwrap().unwrap().encrypted);
        assert!(
            load_or_create(&path)
                .unwrap_err()
                .to_string()
                .contains("encrypted")
        );
        assert!(load_with_password(&path, "wrong password").is_err());

        let loaded = load_with_password(&path, "correct horse battery staple").unwrap();
        assert_eq!(loaded.address(), wallet.address());
        assert_eq!(
            setup_seed_phrase_with_password(&path, Some("correct horse battery staple"))
                .unwrap()
                .as_deref(),
            Some(seed_phrase.as_str())
        );
        assert!(setup_seed_phrase(&path).unwrap().is_none());
    }

    #[test]
    fn plaintext_wallet_can_be_encrypted_in_place() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("wallet.json");

        let (wallet, seed_phrase) = replace_with_generated_seed_phrase(&path).unwrap();
        encrypt_existing_with_password(&path, "correct horse battery staple").unwrap();
        let stored = fs::read_to_string(&path).unwrap();

        assert!(stored.contains("\"version\": 3"));
        assert!(!stored.contains(&seed_phrase));
        assert_eq!(
            load_with_password(&path, "correct horse battery staple")
                .unwrap()
                .address(),
            wallet.address()
        );
    }

    #[test]
    fn plaintext_wallet_persists_owned_blinded_transactions() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("wallet.json");
        replace_with_generated_seed_phrase(&path).unwrap();
        let owned = sample_owned_blinded_transaction();

        replace_owned_blinded_transactions(&path, None, vec![owned.clone()]).unwrap();

        assert_eq!(
            load_owned_blinded_transactions(&path, None).unwrap(),
            vec![owned]
        );
    }

    #[test]
    fn encrypted_wallet_persists_owned_blinded_transactions_without_plaintext() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("wallet.json");
        replace_with_generated_seed_phrase_encrypted(&path, "correct horse battery staple")
            .unwrap();
        let owned = sample_owned_blinded_transaction();

        replace_owned_blinded_transactions(
            &path,
            Some("correct horse battery staple"),
            vec![owned.clone()],
        )
        .unwrap();

        let stored = fs::read_to_string(&path).unwrap();
        assert!(!stored.contains(&owned.payload.signature().to_string()));
        assert!(!stored.contains(&owned.reveal.key));
        assert_eq!(
            load_owned_blinded_transactions(&path, Some("correct horse battery staple")).unwrap(),
            vec![owned]
        );
        assert!(load_owned_blinded_transactions(&path, Some("wrong password")).is_err());
    }

    #[test]
    fn imports_normalized_seed_phrase() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("wallet.json");

        let wallet = replace_with_imported_seed_phrase(
            &path,
            " ABANDON  abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art ",
        )
        .unwrap();
        let loaded = load_or_create(&path).unwrap();

        assert_eq!(wallet.address(), loaded.address());
        assert_eq!(
            setup_seed_phrase(&path).unwrap().as_deref(),
            Some(
                "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art"
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

    #[test]
    fn rejects_imported_seed_phrase_with_invalid_checksum() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("wallet.json");
        let invalid_checksum = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon";

        let error = replace_with_imported_seed_phrase(&path, invalid_checksum).unwrap_err();

        assert!(error.to_string().contains("BIP-39"));
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

    fn sample_owned_blinded_transaction() -> OwnedBlindedTransaction {
        let payload = Transaction::Transfer {
            inputs: vec![TxInput {
                outpoint: crate::domain::OutPoint {
                    txid: "a".repeat(64),
                    index: 0,
                },
                owner: "mv_sample_owner".to_string(),
                signature: "b".repeat(64),
            }],
            outputs: vec![TxOutput {
                address: "mv_sample_recipient".to_string(),
                amount: 1,
            }],
            fee: 1,
            signature: "c".repeat(64),
        };
        OwnedBlindedTransaction {
            transaction: BlindedTransaction {
                commitment: "d".repeat(64),
                fee: 1,
                encrypted_size: 42,
                expires_at_height: 10,
                nonce: "e".repeat(24),
                ciphertext: "f".repeat(84),
                payload_hash: "1".repeat(64),
            },
            payload,
            reveal: BlindedReveal {
                commitment: "d".repeat(64),
                key: "2".repeat(64),
            },
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
