use anyhow::{Context, Result, bail};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use ml_dsa::{
    EncodedVerifyingKey, MlDsa44, Signature as MlDsaSignature, VerifyingKey as MlDsaVerifyingKey,
};

/// Signature schemes understood by the protocol implementation.
///
/// Only `Ed25519` is consensus-active today. The other identifiers reserve a
/// stable vocabulary for the post-quantum migration; accepting either of them
/// requires a separately activated transaction and address format.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub enum SignatureScheme {
    Ed25519 = 0,
    MlDsa44 = 1,
    HybridEd25519MlDsa44 = 2,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtocolPublicKey {
    scheme: SignatureScheme,
    bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProtocolSignature {
    scheme: SignatureScheme,
    bytes: Vec<u8>,
}

impl SignatureScheme {
    pub const fn wire_id(self) -> u8 {
        self as u8
    }

    pub const fn public_key_bytes(self) -> usize {
        match self {
            Self::Ed25519 => 32,
            Self::MlDsa44 => 1_312,
            Self::HybridEd25519MlDsa44 => 32 + 1_312,
        }
    }

    pub const fn signature_bytes(self) -> usize {
        match self {
            Self::Ed25519 => 64,
            Self::MlDsa44 => 2_420,
            Self::HybridEd25519MlDsa44 => 64 + 2_420,
        }
    }

    pub const fn from_wire_id(id: u8) -> Option<Self> {
        match id {
            0 => Some(Self::Ed25519),
            1 => Some(Self::MlDsa44),
            2 => Some(Self::HybridEd25519MlDsa44),
            _ => None,
        }
    }

    pub const fn is_consensus_active(self) -> bool {
        matches!(self, Self::Ed25519)
    }
}

impl ProtocolPublicKey {
    pub fn new(scheme: SignatureScheme, bytes: Vec<u8>) -> Result<Self> {
        validate_material_length("public key", scheme.public_key_bytes(), bytes.len())?;
        Ok(Self { scheme, bytes })
    }

    pub fn scheme(&self) -> SignatureScheme {
        self.scheme
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn encode(&self) -> Vec<u8> {
        encode_material(self.scheme, &self.bytes)
    }

    pub fn decode(encoded: &[u8]) -> Result<Self> {
        let (scheme, bytes) = decode_material(encoded)?;
        Self::new(scheme, bytes.to_vec())
    }
}

impl ProtocolSignature {
    pub fn new(scheme: SignatureScheme, bytes: Vec<u8>) -> Result<Self> {
        validate_material_length("signature", scheme.signature_bytes(), bytes.len())?;
        Ok(Self { scheme, bytes })
    }

    pub fn scheme(&self) -> SignatureScheme {
        self.scheme
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub fn encode(&self) -> Vec<u8> {
        encode_material(self.scheme, &self.bytes)
    }

    pub fn decode(encoded: &[u8]) -> Result<Self> {
        let (scheme, bytes) = decode_material(encoded)?;
        Self::new(scheme, bytes.to_vec())
    }
}

fn encode_material(scheme: SignatureScheme, bytes: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(5 + bytes.len());
    encoded.push(scheme.wire_id());
    encoded.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    encoded.extend_from_slice(bytes);
    encoded
}

fn decode_material(encoded: &[u8]) -> Result<(SignatureScheme, &[u8])> {
    let (&scheme, encoded) = encoded
        .split_first()
        .context("signature material is empty")?;
    let scheme = SignatureScheme::from_wire_id(scheme)
        .with_context(|| format!("unknown signature scheme {scheme}"))?;
    let (length, bytes) = encoded
        .split_at_checked(4)
        .context("signature material length is missing")?;
    let declared = u32::from_be_bytes(length.try_into().expect("four-byte length")) as usize;
    if bytes.len() != declared {
        bail!(
            "signature material declares {declared} bytes but contains {}",
            bytes.len()
        );
    }
    Ok((scheme, bytes))
}

fn validate_material_length(label: &str, expected: usize, actual: usize) -> Result<()> {
    if actual != expected {
        bail!("{label} must contain {expected} bytes, got {actual}");
    }
    Ok(())
}

pub(crate) fn ed25519_public_key(signing_seed: &[u8; 32]) -> [u8; 32] {
    SigningKey::from_bytes(signing_seed)
        .verifying_key()
        .to_bytes()
}

pub(crate) fn sign_ed25519(signing_seed: &[u8; 32], payload: &[u8]) -> [u8; 64] {
    SigningKey::from_bytes(signing_seed)
        .sign(payload)
        .to_bytes()
}

pub(crate) fn validate_ed25519_public_key(public_key: &[u8; 32]) -> Result<()> {
    let verifying_key = VerifyingKey::from_bytes(public_key)
        .context("address payload is not a valid Ed25519 verifying key")?;
    if verifying_key.is_weak() {
        bail!("address payload contains a weak Ed25519 verifying key");
    }
    Ok(())
}

pub(crate) fn verify_ed25519(
    public_key: &[u8; 32],
    payload: &[u8],
    signature: &[u8; 64],
    label: &str,
) -> Result<()> {
    let verifying_key = VerifyingKey::from_bytes(public_key)
        .with_context(|| format!("invalid {label} public key"))?;
    verifying_key
        .verify(payload, &Signature::from_bytes(signature))
        .with_context(|| format!("{label} signature is invalid"))
}

pub(crate) fn verify_ml_dsa44(
    public_key: &[u8; 1_312],
    payload: &[u8],
    signature: &[u8; 2_420],
    label: &str,
) -> Result<()> {
    let encoded_public_key = EncodedVerifyingKey::<MlDsa44>::try_from(public_key.as_slice())
        .with_context(|| format!("invalid {label} ML-DSA-44 public key length"))?;
    let public_key = MlDsaVerifyingKey::<MlDsa44>::decode(&encoded_public_key);
    let signature = MlDsaSignature::<MlDsa44>::try_from(signature.as_slice())
        .with_context(|| format!("invalid {label} ML-DSA-44 signature encoding"))?;
    if !public_key.verify_with_context(payload, &[], &signature) {
        bail!("{label} ML-DSA-44 signature is invalid");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        ProtocolPublicKey, ProtocolSignature, SignatureScheme, ed25519_public_key, sign_ed25519,
        verify_ed25519,
    };

    #[test]
    fn signature_scheme_ids_and_sizes_are_stable() {
        assert_eq!(SignatureScheme::Ed25519.wire_id(), 0);
        assert_eq!(SignatureScheme::MlDsa44.wire_id(), 1);
        assert_eq!(SignatureScheme::HybridEd25519MlDsa44.wire_id(), 2);
        assert_eq!(SignatureScheme::Ed25519.public_key_bytes(), 32);
        assert_eq!(SignatureScheme::Ed25519.signature_bytes(), 64);
        assert_eq!(SignatureScheme::MlDsa44.public_key_bytes(), 1_312);
        assert_eq!(SignatureScheme::MlDsa44.signature_bytes(), 2_420);
        assert_eq!(
            SignatureScheme::HybridEd25519MlDsa44.public_key_bytes(),
            1_344
        );
        assert_eq!(
            SignatureScheme::HybridEd25519MlDsa44.signature_bytes(),
            2_484
        );
        assert!(SignatureScheme::Ed25519.is_consensus_active());
        assert!(!SignatureScheme::MlDsa44.is_consensus_active());
        assert_eq!(SignatureScheme::from_wire_id(3), None);
    }

    #[test]
    fn centralized_ed25519_backend_signs_and_verifies() {
        let seed = [7_u8; 32];
        let public_key = ed25519_public_key(&seed);
        let signature = sign_ed25519(&seed, b"quantum-agility-test");

        verify_ed25519(&public_key, b"quantum-agility-test", &signature, "test").unwrap();
        assert!(verify_ed25519(&public_key, b"tampered", &signature, "test").is_err());
    }

    #[test]
    fn algorithm_tagged_material_roundtrips_and_rejects_malformed_lengths() {
        let key = ProtocolPublicKey::new(SignatureScheme::MlDsa44, vec![5; 1_312]).unwrap();
        let signature =
            ProtocolSignature::new(SignatureScheme::HybridEd25519MlDsa44, vec![9; 2_484]).unwrap();

        assert_eq!(ProtocolPublicKey::decode(&key.encode()).unwrap(), key);
        assert_eq!(
            ProtocolSignature::decode(&signature.encode()).unwrap(),
            signature
        );

        let mut unknown_scheme = key.encode();
        unknown_scheme[0] = 99;
        assert!(ProtocolPublicKey::decode(&unknown_scheme).is_err());

        let mut wrong_declared_length = signature.encode();
        wrong_declared_length[4] -= 1;
        assert!(ProtocolSignature::decode(&wrong_declared_length).is_err());
        assert!(
            ProtocolSignature::new(SignatureScheme::Ed25519, vec![0; 63]).is_err(),
            "scheme-specific lengths must fail closed"
        );
    }
}
