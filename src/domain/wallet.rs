use std::{fmt, sync::Arc};

use secrecy::{ExposeSecret, SecretBox, zeroize::Zeroize};
use sha2::{Digest, Sha256};

use super::block::LeaderProofPayload;
use super::{
    BurnBundle, BurnBundlePayload, LeaderProof, ed25519_public_key, hex_encode, sign_ed25519,
};

const WALLET_SEED_DOMAIN: &str = "iuna-wallet-seed";

#[derive(Clone)]
pub struct Wallet {
    address: String,
    signing_seed: Arc<SecretBox<[u8; 32]>>,
}

impl Wallet {
    pub fn from_seed(seed: &str) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(WALLET_SEED_DOMAIN.as_bytes());
        hasher.update(b":");
        hasher.update(seed.as_bytes());
        let mut seed_hash = hasher.finalize();
        let signing_seed = SecretBox::init_with_mut(|signing_seed: &mut [u8; 32]| {
            signing_seed.copy_from_slice(&seed_hash);
        });
        seed_hash.zeroize();
        let address = hex_encode(ed25519_public_key(signing_seed.expose_secret()));
        Self {
            address,
            signing_seed: Arc::new(signing_seed),
        }
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub(super) fn sign_payload(&self, payload: &str) -> String {
        self.sign_bytes(payload.as_bytes())
    }

    pub(super) fn sign_bytes(&self, payload: &[u8]) -> String {
        hex_encode(sign_ed25519(self.signing_seed.expose_secret(), payload))
    }

    pub(super) fn leader_proof(&self, payload: &LeaderProofPayload) -> LeaderProof {
        let signature = self.sign_payload(&payload.canonical());
        LeaderProof {
            ticket_id: payload.ticket_id.clone(),
            public_key: self.address.clone(),
            signature,
        }
    }

    pub(super) fn burn_bundle(&self, payload: BurnBundlePayload) -> BurnBundle {
        let signature = self.sign_payload(&payload.canonical());
        BurnBundle {
            height: payload.height,
            prev_hash: payload.prev_hash,
            slot: payload.slot,
            member: self.address.clone(),
            burns: payload.burns,
            signature,
        }
    }
}

impl fmt::Debug for Wallet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Wallet")
            .field("address", &self.address)
            .field("signing_seed", &"[REDACTED]")
            .finish()
    }
}

impl PartialEq for Wallet {
    fn eq(&self, other: &Self) -> bool {
        self.address == other.address
    }
}

impl Eq for Wallet {}

#[cfg(test)]
mod tests {
    use secrecy::ExposeSecret;

    use super::Wallet;
    use crate::domain::hex_encode;

    #[test]
    fn debug_output_redacts_the_wallet_signing_seed() {
        let wallet = Wallet::from_seed("debug-redaction-wallet-seed");
        let signing_seed = hex_encode(wallet.signing_seed.expose_secret());
        let debug = format!("{wallet:?}");

        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(&signing_seed));
    }
}
