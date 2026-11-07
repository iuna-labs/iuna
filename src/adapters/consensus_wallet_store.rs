//! Encrypted Ed25519 seed storage for wallets.

use std::{
    fs::{self, OpenOptions},
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
use secrecy::{ExposeSecret, SecretBox};
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::app::wallet_commands::Wallet;

const FILE_VERSION: u32 = 1;
const ALGORITHM: &str = "chacha20poly1305";
const KDF: &str = "pbkdf2-sha256";
const KDF_ITERATIONS: u32 = 210_000;
const MIN_KDF_ITERATIONS: u32 = 100_000;
const MAX_KDF_ITERATIONS: u32 = 1_000_000;
const MAX_FILE_BYTES: u64 = 16 * 1024;
// A format identifier, fixed since the first wallet file: changing it would lock those files.
const AAD_DOMAIN: &[u8] = b"IUNA-DEVNET-WALLET-v1";

#[derive(Debug, Deserialize, Serialize)]
struct WalletFile {
    version: u32,
    public_key: String,
    algorithm: String,
    kdf: String,
    kdf_iterations: u32,
    salt: String,
    nonce: String,
    ciphertext: String,
}

/// Minimum for newly chosen wallet passwords, shared with the node UI and the browser wallet.
/// Unlocking does not enforce it, so wallets created under an older, shorter minimum still open.
pub const MIN_NEW_PASSWORD_CHARS: usize = 10;
pub const MAX_PASSWORD_BYTES: usize = 1_024;

fn check_new_password(password: &str) -> Result<()> {
    if password.chars().count() < MIN_NEW_PASSWORD_CHARS || password.len() > MAX_PASSWORD_BYTES {
        bail!(
            "wallet password must contain at least {MIN_NEW_PASSWORD_CHARS} characters and at most {MAX_PASSWORD_BYTES} bytes"
        );
    }
    Ok(())
}

pub fn create(path: &Path, password: &str) -> Result<Wallet> {
    check_new_password(password)?;
    let seed = random_secret_seed()?;
    let wallet = Wallet::from_seed(*seed.expose_secret());
    write_new(path, seed.expose_secret(), wallet.public_key(), password)?;
    Ok(wallet)
}

/// Creates a wallet and returns its recovery phrase exactly once to the caller.
pub fn create_recoverable(path: &Path, password: &str) -> Result<(Wallet, String)> {
    check_new_password(password)?;
    let seed = random_secret_seed()?;
    let phrase = Mnemonic::from_entropy_in(Language::English, seed.expose_secret())?.to_string();
    let wallet = Wallet::from_seed(*seed.expose_secret());
    write_new(path, seed.expose_secret(), wallet.public_key(), password)?;
    Ok((wallet, phrase))
}

/// Restores the exact Ed25519 wallet encoded by an iuna recovery phrase.
pub fn restore(path: &Path, password: &str, phrase: &str) -> Result<Wallet> {
    check_new_password(password)?;
    let mnemonic = Mnemonic::parse_in_normalized(Language::English, phrase)
        .context("invalid recovery phrase")?;
    let entropy = mnemonic.to_entropy();
    if entropy.len() != 32 {
        bail!("recovery phrase must contain 24 words");
    }
    let mut seed = [0_u8; 32];
    seed.copy_from_slice(&entropy);
    let wallet = Wallet::from_seed(seed);
    write_new(path, &seed, wallet.public_key(), password)?;
    Ok(wallet)
}

pub fn load(path: &Path, password: &str) -> Result<Wallet> {
    let (seed, public_key) = decrypt_seed(path, password)?;
    let wallet = Wallet::from_seed(*seed.expose_secret());
    if wallet.public_key() != public_key {
        bail!("wallet public key does not match encrypted seed");
    }
    Ok(wallet)
}

/// Re-encrypts the same seed under a new password, replacing the wallet file atomically.
pub fn change_password(path: &Path, current: &str, new: &str) -> Result<Wallet> {
    check_new_password(new)?;
    let (seed, public_key) = decrypt_seed(path, current)?;
    write_wallet(
        path,
        seed.expose_secret(),
        public_key,
        new,
        Install::Replace,
    )?;
    Ok(Wallet::from_seed(*seed.expose_secret()))
}

pub fn recovery_phrase(path: &Path, password: &str) -> Result<String> {
    let (seed, _) = decrypt_seed(path, password)?;
    Ok(Mnemonic::from_entropy_in(Language::English, seed.expose_secret())?.to_string())
}

fn decrypt_seed(path: &Path, password: &str) -> Result<(SecretBox<[u8; 32]>, [u8; 32])> {
    let stored = read_wallet_file(path)?;
    let public_key = decode_array::<32>(&stored.public_key).context("invalid wallet public key")?;
    let salt = decode_array::<16>(&stored.salt).context("invalid wallet salt")?;
    let nonce = decode_array::<12>(&stored.nonce).context("invalid wallet nonce")?;
    let ciphertext = decode_hex(&stored.ciphertext).context("invalid wallet ciphertext")?;
    let key = encryption_key(password, &salt, stored.kdf_iterations);
    let cipher = ChaCha20Poly1305::new(key.expose_secret().into());
    let aad = associated_data(&public_key);
    let plaintext = SecretBox::new(Box::new(
        cipher
            .decrypt(
                Nonce::from_slice(&nonce),
                Payload {
                    msg: &ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| anyhow!("invalid wallet password or corrupted wallet"))?,
    ));
    if plaintext.expose_secret().len() != 32 {
        bail!("invalid decrypted wallet seed length");
    }
    let seed = SecretBox::init_with_mut(|seed: &mut [u8; 32]| {
        seed.copy_from_slice(plaintext.expose_secret());
    });
    Ok((seed, public_key))
}

pub fn public_key(path: &Path) -> Result<[u8; 32]> {
    let stored = read_wallet_file(path)?;
    decode_array::<32>(&stored.public_key).context("invalid wallet public key")
}

fn read_wallet_file(path: &Path) -> Result<WalletFile> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("reading wallet metadata {}", path.display()))?;
    if metadata.len() > MAX_FILE_BYTES {
        bail!("wallet file exceeds size limit");
    }
    let bytes = fs::read(path).with_context(|| format!("reading wallet {}", path.display()))?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        bail!("wallet file exceeds size limit");
    }
    let stored: WalletFile = serde_json::from_slice(&bytes).context("decoding wallet file")?;
    validate_metadata(&stored)?;
    Ok(stored)
}

#[derive(Clone, Copy)]
enum Install {
    /// Never overwrite: creation and restoration must not clobber an existing wallet.
    New,
    /// Atomically replace the existing file, for a password change of the same seed.
    Replace,
}

fn write_new(path: &Path, seed: &[u8; 32], public_key: [u8; 32], password: &str) -> Result<()> {
    write_wallet(path, seed, public_key, password, Install::New)
}

fn write_wallet(
    path: &Path,
    seed: &[u8; 32],
    public_key: [u8; 32],
    password: &str,
    install: Install,
) -> Result<()> {
    let salt = random_bytes::<16>()?;
    let nonce = random_bytes::<12>()?;
    let key = encryption_key(password, &salt, KDF_ITERATIONS);
    let cipher = ChaCha20Poly1305::new(key.expose_secret().into());
    let aad = associated_data(&public_key);
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload {
                msg: seed,
                aad: &aad,
            },
        )
        .map_err(|_| anyhow!("encrypting wallet seed"))?;
    let stored = WalletFile {
        version: FILE_VERSION,
        public_key: hex(public_key),
        algorithm: ALGORITHM.to_owned(),
        kdf: KDF.to_owned(),
        kdf_iterations: KDF_ITERATIONS,
        salt: hex(salt),
        nonce: hex(nonce),
        ciphertext: hex(ciphertext),
    };
    let mut bytes = serde_json::to_vec_pretty(&stored)?;
    bytes.push(b'\n');
    atomic_write(path, &bytes, install)
}

fn validate_metadata(stored: &WalletFile) -> Result<()> {
    if stored.version != FILE_VERSION {
        bail!("unsupported wallet version");
    }
    if stored.algorithm != ALGORITHM || stored.kdf != KDF {
        bail!("unsupported wallet encryption parameters");
    }
    if !(MIN_KDF_ITERATIONS..=MAX_KDF_ITERATIONS).contains(&stored.kdf_iterations) {
        bail!("unsupported wallet KDF iteration count");
    }
    Ok(())
}

fn encryption_key(password: &str, salt: &[u8], iterations: u32) -> SecretBox<[u8; 32]> {
    SecretBox::init_with_mut(|key: &mut [u8; 32]| {
        pbkdf2_hmac::<Sha256>(password.as_bytes(), salt, iterations, key);
    })
}

fn associated_data(public_key: &[u8; 32]) -> Vec<u8> {
    [AAD_DOMAIN, public_key].concat()
}

fn random_secret_seed() -> Result<SecretBox<[u8; 32]>> {
    let mut random_error = None;
    let secret = SecretBox::init_with_mut(|bytes: &mut [u8; 32]| {
        if let Err(error) = getrandom::getrandom(bytes) {
            random_error = Some(error);
        }
    });
    if let Some(error) = random_error {
        bail!("reading system randomness: {error:?}");
    }
    Ok(secret)
}

fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0; N];
    getrandom::getrandom(&mut bytes)
        .map_err(|error| anyhow!("reading system randomness: {error:?}"))?;
    Ok(bytes)
}

fn atomic_write(path: &Path, bytes: &[u8], install: Install) -> Result<()> {
    if matches!(install, Install::New) && path.exists() {
        bail!("refusing to overwrite existing wallet");
    }
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension(format!("tmp-{}", hex(random_bytes::<8>()?)));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .with_context(|| format!("creating temporary wallet {}", temporary.display()))?;
    if let Err(error) = file.write_all(bytes).and_then(|_| file.sync_all()) {
        let _ = fs::remove_file(&temporary);
        return Err(error).context("writing temporary wallet");
    }
    drop(file);
    match install {
        Install::New => {
            if let Err(error) = fs::hard_link(&temporary, path) {
                let _ = fs::remove_file(&temporary);
                return Err(error).context("installing wallet without overwrite");
            }
            let _ = fs::remove_file(&temporary);
        }
        Install::Replace => {
            if let Err(error) = fs::rename(&temporary, path) {
                let _ = fs::remove_file(&temporary);
                return Err(error).context("replacing wallet");
            }
        }
    }
    #[cfg(unix)]
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.as_ref().len() * 2);
    for byte in bytes.as_ref() {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}

fn decode_array<const N: usize>(value: &str) -> Result<[u8; N]> {
    decode_hex(value)?
        .try_into()
        .map_err(|_| anyhow!("hex value has incorrect length"))
}

fn decode_hex(value: &str) -> Result<Vec<u8>> {
    if !value.len().is_multiple_of(2) {
        bail!("hex value has odd length");
    }
    value
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| Ok((nibble(pair[0])? << 4) | nibble(pair[1])?))
        .collect()
}

fn nibble(value: u8) -> Result<u8> {
    match value {
        b'0'..=b'9' => Ok(value - b'0'),
        b'a'..=b'f' => Ok(value - b'a' + 10),
        b'A'..=b'F' => Ok(value - b'A' + 10),
        _ => bail!("invalid hex character"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encrypted_wallet_roundtrips_and_never_overwrites() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wallet.json");
        fs::write(path.with_extension("tmp-stale"), b"interrupted write").unwrap();
        let wallet = create(&path, "correct horse battery staple").unwrap();
        let loaded = load(&path, "correct horse battery staple").unwrap();
        assert_eq!(loaded.public_key(), wallet.public_key());
        assert_eq!(public_key(&path).unwrap(), wallet.public_key());
        assert!(create(&path, "replacement").is_err());
        assert!(load(&path, "wrong password").is_err());
        let text = fs::read_to_string(path).unwrap();
        assert!(!text.contains("correct horse"));
    }

    #[test]
    fn recovery_phrase_restores_the_same_wallet() {
        let original_directory = tempfile::tempdir().unwrap();
        let restored_directory = tempfile::tempdir().unwrap();
        let (original, phrase) = create_recoverable(
            &original_directory.path().join("wallet.json"),
            "original password",
        )
        .unwrap();

        assert_eq!(phrase.split_whitespace().count(), 24);
        let restored = restore(
            &restored_directory.path().join("wallet.json"),
            "different password",
            &phrase,
        )
        .unwrap();

        assert_eq!(restored.public_key(), original.public_key());
        assert_eq!(
            recovery_phrase(
                &original_directory.path().join("wallet.json"),
                "original password"
            )
            .unwrap(),
            phrase
        );
        assert!(
            restore(
                &restored_directory.path().join("second-wallet.json"),
                "different password",
                "not a valid recovery phrase",
            )
            .is_err()
        );
    }

    #[test]
    fn password_change_keeps_the_key_and_rejects_the_old_password() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wallet.json");
        let (wallet, phrase) = create_recoverable(&path, "first password").unwrap();
        assert!(change_password(&path, "wrong password", "second password").is_err());
        assert!(change_password(&path, "first password", "short").is_err());
        let changed = change_password(&path, "first password", "second password").unwrap();
        assert_eq!(changed.public_key(), wallet.public_key());
        assert!(load(&path, "first password").is_err());
        assert_eq!(
            load(&path, "second password").unwrap().public_key(),
            wallet.public_key()
        );
        assert_eq!(recovery_phrase(&path, "second password").unwrap(), phrase);
    }

    #[test]
    fn new_passwords_need_the_shared_minimum_length() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wallet.json");
        assert!(create(&path, "123456789").is_err());
        assert!(create_recoverable(&path, "").is_err());
        assert!(!path.exists());
        create(&path, "1234567890").unwrap();
    }

    #[test]
    fn hostile_kdf_cost_is_rejected_before_derivation() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("wallet.json");
        create(&path, "password1234").unwrap();
        let mut stored: WalletFile = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        stored.kdf_iterations = u32::MAX;
        fs::write(&path, serde_json::to_vec(&stored).unwrap()).unwrap();
        assert!(load(&path, "password1234").is_err());
    }
}
