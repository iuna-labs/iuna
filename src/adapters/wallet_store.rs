use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
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

use crate::domain::Wallet;
#[cfg(feature = "fuzzing")]
use crate::domain::validate_address;

const WALLET_FILE_VERSION: u32 = 3;
const PLAINTEXT_WALLET_FILE_VERSION: u32 = 2;
const WALLET_ENCRYPTION_ALGORITHM: &str = "chacha20poly1305";
const WALLET_ENCRYPTION_KDF: &str = "pbkdf2-sha256";
const WALLET_ENCRYPTION_ITERATIONS: u32 = 210_000;
const MIN_WALLET_ENCRYPTION_ITERATIONS: u32 = 100_000;
const MAX_WALLET_ENCRYPTION_ITERATIONS: u32 = 1_000_000;
const GENERATED_SEED_WORDS: usize = 24;
const BIP39_SEED_ENTROPY_BYTES: usize = 32;

#[derive(Debug, Serialize, Deserialize)]
struct WalletFile {
    version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    seed: Option<String>,
    address: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    encryption: Option<EncryptedWalletSeed>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct WalletData {
    seed: String,
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
    write_wallet_file(path, seed, wallet.address(), WalletFileMode::CreateNew)?;

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
    write_encrypted_wallet_data_file(
        path,
        WalletData { seed },
        wallet.address(),
        password,
        WalletFileMode::Replace,
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
    write_encrypted_wallet_data_file(
        path,
        WalletData { seed },
        wallet.address(),
        new_password,
        WalletFileMode::Replace,
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
        let seed = stored
            .seed
            .context("legacy wallet file does not contain a seed")?;
        write_wallet_file(path, seed, wallet.address(), WalletFileMode::Replace)
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
    parse_wallet_file_bytes(&bytes, &path.display().to_string())
}

fn parse_wallet_file_bytes(bytes: &[u8], source: &str) -> Result<WalletFile> {
    serde_json::from_slice(bytes).with_context(|| format!("failed to parse wallet file {source}"))
}

enum WalletFileMode {
    CreateNew,
    Replace,
}

fn write_wallet(path: &Path, seed: String, mode: WalletFileMode) -> Result<Wallet> {
    let wallet = Wallet::from_seed(&seed);
    write_wallet_file(path, seed, wallet.address(), mode)?;
    Ok(wallet)
}

fn write_wallet_encrypted(
    path: &Path,
    seed: String,
    password: &str,
    mode: WalletFileMode,
) -> Result<Wallet> {
    let wallet = Wallet::from_seed(&seed);
    write_encrypted_wallet_file(path, seed, wallet.address(), password, mode)?;
    Ok(wallet)
}

fn write_wallet_file(path: &Path, seed: String, address: &str, mode: WalletFileMode) -> Result<()> {
    write_wallet_data_file(path, WalletData { seed }, address, mode)
}

fn write_wallet_data_file(
    path: &Path,
    data: WalletData,
    address: &str,
    mode: WalletFileMode,
) -> Result<()> {
    let stored = WalletFile {
        version: PLAINTEXT_WALLET_FILE_VERSION,
        seed: Some(data.seed),
        address: address.to_string(),
        encryption: None,
    };
    let mut bytes =
        serde_json::to_vec_pretty(&stored).context("failed to serialize wallet file")?;
    bytes.push(b'\n');
    atomic_write_wallet_file(path, &bytes, mode)
}

fn write_encrypted_wallet_file(
    path: &Path,
    seed: String,
    address: &str,
    password: &str,
    mode: WalletFileMode,
) -> Result<()> {
    write_encrypted_wallet_data_file(path, WalletData { seed }, address, password, mode)
}

fn write_encrypted_wallet_data_file(
    path: &Path,
    data: WalletData,
    address: &str,
    password: &str,
    mode: WalletFileMode,
) -> Result<()> {
    let encryption = encrypt_wallet_data(&data, address, password)?;
    let stored = WalletFile {
        version: WALLET_FILE_VERSION,
        seed: None,
        address: address.to_string(),
        encryption: Some(encryption),
    };
    let mut bytes =
        serde_json::to_vec_pretty(&stored).context("failed to serialize wallet file")?;
    bytes.push(b'\n');
    atomic_write_wallet_file(path, &bytes, mode)
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
    Ok(WalletData { seed })
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
    validate_wallet_encryption_iterations(encryption.kdf_iterations)?;
    let salt = decode_hex(&encryption.salt).context("invalid wallet encryption salt")?;
    let nonce = decode_hex(&encryption.nonce).context("invalid wallet encryption nonce")?;
    let ciphertext = decode_hex(&encryption.ciphertext).context("invalid wallet encrypted seed")?;
    if salt.len() != 16 {
        bail!("invalid wallet encryption salt length");
    }
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
        }),
    }
}

fn wallet_encryption_key(password: &str, salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut key = [0_u8; 32];
    pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, iterations, &mut key);
    key
}

fn validate_wallet_encryption_iterations(iterations: u32) -> Result<()> {
    if !(MIN_WALLET_ENCRYPTION_ITERATIONS..=MAX_WALLET_ENCRYPTION_ITERATIONS).contains(&iterations)
    {
        bail!("unsupported wallet encryption iteration count");
    }
    Ok(())
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

#[cfg(feature = "fuzzing")]
pub fn fuzz_parse_wallet_metadata(bytes: &[u8]) -> Result<WalletMetadata> {
    let stored = parse_wallet_file_bytes(bytes, "<fuzz>")?;
    validate_wallet_file_metadata(&stored)?;
    Ok(WalletMetadata {
        address: stored.address,
        encrypted: stored.encryption.is_some(),
    })
}

#[cfg(feature = "fuzzing")]
fn validate_wallet_file_metadata(stored: &WalletFile) -> Result<()> {
    if stored.version != WALLET_FILE_VERSION
        && stored.version != PLAINTEXT_WALLET_FILE_VERSION
        && stored.version != 1
    {
        bail!("unsupported wallet file version {}", stored.version);
    }
    validate_address(&stored.address, "wallet address")?;
    if let Some(encryption) = &stored.encryption {
        if encryption.algorithm != WALLET_ENCRYPTION_ALGORITHM {
            bail!("unsupported wallet encryption algorithm");
        }
        if encryption.kdf != WALLET_ENCRYPTION_KDF {
            bail!("unsupported wallet encryption kdf");
        }
        validate_wallet_encryption_iterations(encryption.kdf_iterations)?;
        let salt = decode_hex(&encryption.salt).context("invalid wallet encryption salt")?;
        if salt.len() != 16 {
            bail!("invalid wallet encryption salt length");
        }
        let nonce = decode_hex(&encryption.nonce).context("invalid wallet encryption nonce")?;
        if nonce.len() != 12 {
            bail!("invalid wallet encryption nonce length");
        }
        let _ = decode_hex(&encryption.ciphertext).context("invalid wallet encrypted seed")?;
    } else {
        let seed = stored
            .seed
            .as_deref()
            .context("wallet file does not contain a seed")?;
        let _ = normalize_seed_phrase(seed)?;
    }
    Ok(())
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

fn atomic_write_wallet_file(path: &Path, bytes: &[u8], mode: WalletFileMode) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create wallet directory {}", parent.display()))?;
    }

    for attempt in 0..16 {
        let temp_path = temp_file_path(path, attempt);
        match open_wallet_temp_file(&temp_path) {
            Ok(mut file) => {
                if let Err(error) = write_and_sync(&mut file, bytes) {
                    let _ = fs::remove_file(&temp_path);
                    return Err(error).with_context(|| {
                        format!("failed to write wallet file {}", path.display())
                    });
                }
                drop(file);
                match mode {
                    WalletFileMode::CreateNew => {
                        if let Err(error) = fs::hard_link(&temp_path, path) {
                            let _ = fs::remove_file(&temp_path);
                            return Err(error).with_context(|| {
                                format!("failed to create wallet file {}", path.display())
                            });
                        }
                        let _ = fs::remove_file(&temp_path);
                    }
                    WalletFileMode::Replace => {
                        if let Err(error) = fs::rename(&temp_path, path) {
                            let _ = fs::remove_file(&temp_path);
                            return Err(error).with_context(|| {
                                format!("failed to replace wallet file {}", path.display())
                            });
                        }
                    }
                }
                sync_parent_dir(path);
                return Ok(());
            }
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|error| error.kind() == std::io::ErrorKind::AlreadyExists) =>
            {
                continue;
            }
            Err(error) => return Err(error),
        }
    }

    bail!(
        "failed to create temporary wallet file for {}",
        path.display()
    )
}

fn open_wallet_temp_file(path: &Path) -> Result<File> {
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

fn write_and_sync(file: &mut File, bytes: &[u8]) -> Result<()> {
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

fn temp_file_path(path: &Path, attempt: u64) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("wallet");
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    path.with_file_name(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        nanos.saturating_add(u128::from(attempt))
    ))
}

fn sync_parent_dir(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{
        load_or_create, load_with_password, read_wallet_file, replace_with_imported_seed_phrase,
        replace_with_imported_seed_phrase_encrypted,
    };

    const TEST_SEED: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art";

    #[test]
    fn stale_atomic_temp_file_does_not_replace_saved_wallet() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("wallet.json");
        let stale_temp = dir.path().join(".wallet.json.crash.tmp");
        let wallet = replace_with_imported_seed_phrase(&path, TEST_SEED).unwrap();
        fs::write(&stale_temp, b"{\"version\": 2,").unwrap();

        let loaded = load_or_create(&path).unwrap();

        assert_eq!(loaded.address(), wallet.address());
        assert!(stale_temp.exists());
    }

    #[test]
    fn encrypted_wallet_rejects_unreasonable_kdf_iterations_before_unlock() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("wallet.json");
        replace_with_imported_seed_phrase_encrypted(&path, TEST_SEED, "password-123456").unwrap();
        let wallet_json = fs::read_to_string(&path).unwrap();
        fs::write(
            &path,
            wallet_json.replace(
                "\"kdf_iterations\": 210000",
                "\"kdf_iterations\": 1000000000",
            ),
        )
        .unwrap();

        let error = load_with_password(&path, "password-123456").unwrap_err();

        assert!(
            error
                .to_string()
                .contains("unsupported wallet encryption iteration count")
        );
    }

    #[test]
    fn encrypted_wallet_metadata_rejects_short_salt() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("wallet.json");
        replace_with_imported_seed_phrase_encrypted(&path, TEST_SEED, "password-123456").unwrap();
        let mut stored = read_wallet_file(&path).unwrap();
        stored.encryption.as_mut().unwrap().salt = "abcd".to_string();
        fs::write(&path, serde_json::to_vec_pretty(&stored).unwrap()).unwrap();

        let error = load_with_password(&path, "password-123456").unwrap_err();

        assert!(
            error
                .to_string()
                .contains("invalid wallet encryption salt length")
        );
    }
}
