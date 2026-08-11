use anyhow::{Context, Result, bail};
use getrandom::getrandom;
use pbkdf2::pbkdf2_hmac;
use sha2::{Digest, Sha256};

const PASSWORD_KDF_ALGORITHM: &str = "pbkdf2-sha256";
const PASSWORD_KDF_ITERATIONS: u32 = 210_000;

pub(super) fn validate_password(password: &str) -> Result<()> {
    if password.len() < 12 {
        bail!("password must be at least 12 characters");
    }
    if password.len() > 1024 {
        bail!("password is too long");
    }
    Ok(())
}

pub(super) fn hash_password(password: &str) -> Result<String> {
    let salt = random_bytes::<16>()?;
    let hash = pbkdf2_sha256(password.as_bytes(), &salt, PASSWORD_KDF_ITERATIONS);
    Ok(format!(
        "{PASSWORD_KDF_ALGORITHM}${PASSWORD_KDF_ITERATIONS}${}${}",
        hex_encode(salt),
        hex_encode(hash)
    ))
}

pub(super) fn verify_password(password: &str, encoded: &str) -> Result<bool> {
    let parts = encoded.split('$').collect::<Vec<_>>();
    if parts.len() != 4 || parts[0] != PASSWORD_KDF_ALGORITHM {
        bail!("unsupported password hash");
    }
    let iterations = parts[1]
        .parse::<u32>()
        .context("invalid password hash iterations")?;
    let salt = decode_hex(parts[2]).context("invalid password hash salt")?;
    let expected = decode_hex(parts[3]).context("invalid password hash")?;
    let actual = pbkdf2_sha256(password.as_bytes(), &salt, iterations);
    Ok(constant_time_eq(&actual, &expected))
}

pub(super) fn session_token_hash(token: &str) -> String {
    hex_encode(Sha256::digest(format!("iuna-session:{token}").as_bytes()))
}

pub(super) fn random_hex(bytes: usize) -> Result<String> {
    let mut value = vec![0_u8; bytes];
    getrandom(&mut value)
        .map_err(|error| anyhow::anyhow!("secure random generation failed: {error}"))?;
    Ok(hex_encode(value))
}

pub(super) fn pbkdf2_sha256(password: &[u8], salt: &[u8], iterations: u32) -> [u8; 32] {
    let mut output = [0_u8; 32];
    pbkdf2_hmac::<Sha256>(password, salt, iterations, &mut output);
    output
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    left.iter()
        .zip(right)
        .fold(0_u8, |diff, (left, right)| diff | (left ^ right))
        == 0
}

fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut bytes = [0_u8; N];
    getrandom(&mut bytes)
        .map_err(|error| anyhow::anyhow!("secure random generation failed: {error}"))?;
    Ok(bytes)
}

pub(super) fn hex_encode(bytes: impl AsRef<[u8]>) -> String {
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
