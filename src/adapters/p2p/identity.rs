use std::{
    collections::BTreeMap,
    sync::{Mutex as StdMutex, OnceLock},
};

use anyhow::Result;
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use secrecy::{ExposeSecret, SecretBox};

use crate::app::{GossipEnvelope, NETWORK_ID};

use super::GossipNetwork;

static NODE_SIGNING_KEYS: OnceLock<StdMutex<BTreeMap<String, SecretBox<[u8; 32]>>>> =
    OnceLock::new();

pub(super) fn new_node_id() -> String {
    let signing_seed = SecretBox::init_with_mut(|bytes: &mut [u8; 32]| {
        getrandom::getrandom(bytes).expect("secure randomness unavailable for p2p node id");
    });
    let signing_key = SigningKey::from_bytes(signing_seed.expose_secret());
    let node_id = hex_encode(&signing_key.verifying_key().to_bytes());
    node_signing_keys()
        .lock()
        .expect("node signing key registry mutex poisoned")
        .insert(node_id.clone(), signing_seed);
    node_id
}

fn node_signing_keys() -> &'static StdMutex<BTreeMap<String, SecretBox<[u8; 32]>>> {
    NODE_SIGNING_KEYS.get_or_init(|| StdMutex::new(BTreeMap::new()))
}

pub(super) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        encoded.push(HEX[(byte >> 4) as usize] as char);
        encoded.push(HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

pub(super) fn decode_hex_array<const N: usize>(value: &str) -> Result<[u8; N]> {
    if value.len() != N * 2 {
        anyhow::bail!("hex value has {} chars, expected {}", value.len(), N * 2);
    }
    let mut bytes = [0_u8; N];
    for (index, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
        let high = hex_nibble(chunk[0])?;
        let low = hex_nibble(chunk[1])?;
        bytes[index] = (high << 4) | low;
    }
    Ok(bytes)
}

pub(super) fn hex_nibble(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => anyhow::bail!("invalid hex digit"),
    }
}

pub(super) fn new_verification_nonce() -> String {
    let mut bytes = [0_u8; 32];
    getrandom::getrandom(&mut bytes)
        .expect("secure randomness unavailable for p2p verification nonce");
    hex_encode(&bytes)
}

pub(super) fn peer_verification_payload(address: &str, nonce: &str, node_id: &str) -> String {
    format!("iuna-peer-verification:v1:{NETWORK_ID}:{node_id}:{address}:{nonce}")
}

pub(super) fn peer_verification_response(
    network: &GossipNetwork,
    address: &str,
    nonce: &str,
) -> Option<GossipEnvelope> {
    peer_verification_response_for_node_id(&network.inner.node_id, address, nonce)
}

pub(super) fn peer_verification_response_for_node_id(
    node_id: &str,
    address: &str,
    nonce: &str,
) -> Option<GossipEnvelope> {
    let keys = node_signing_keys()
        .lock()
        .expect("node signing key registry mutex poisoned");
    let signing_seed = keys.get(node_id)?;
    let signing_key = SigningKey::from_bytes(signing_seed.expose_secret());
    let payload = peer_verification_payload(address, nonce, node_id);
    let signature: Signature = signing_key.sign(payload.as_bytes());
    Some(GossipEnvelope::PeerVerificationResponse {
        address: address.to_string(),
        nonce: nonce.to_string(),
        node_id: node_id.to_string(),
        signature: hex_encode(&signature.to_bytes()),
    })
}

pub(super) fn peer_verification_response_is_valid(
    response_address: &str,
    response_nonce: &str,
    response_node_id: &str,
    signature: &str,
    expected_address: &str,
    expected_nonce: &str,
    expected_node_id: &str,
) -> bool {
    if response_address != expected_address
        || response_nonce != expected_nonce
        || response_node_id != expected_node_id
    {
        return false;
    }
    let public_key = match decode_hex_array::<32>(response_node_id) {
        Ok(public_key) => public_key,
        Err(_) => return false,
    };
    let signature = match decode_hex_array::<64>(signature) {
        Ok(signature) => Signature::from_bytes(&signature),
        Err(_) => return false,
    };
    let verifying_key = match VerifyingKey::from_bytes(&public_key) {
        Ok(verifying_key) => verifying_key,
        Err(_) => return false,
    };
    verifying_key
        .verify(
            peer_verification_payload(expected_address, expected_nonce, expected_node_id)
                .as_bytes(),
            &signature,
        )
        .is_ok()
}
