use ed25519_dalek::{Signature, Signer, SigningKey};
use sha2::{Digest, Sha256};

use super::block::LeaderProofPayload;
use super::{
    BurnBundle, BurnBundlePayload, LeaderProof, PUBLIC_KEY_BYTES, decode_hex_array, hex_encode,
};

const WALLET_SEED_DOMAIN: &str = "iuna-wallet-seed";

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Wallet {
    address: String,
    secret: String,
}

impl Wallet {
    pub fn from_seed(seed: &str) -> Self {
        let seed_hash = Sha256::digest(format!("{WALLET_SEED_DOMAIN}:{seed}").as_bytes());
        let mut signing_seed = [0_u8; 32];
        signing_seed.copy_from_slice(&seed_hash);
        let signing_key = SigningKey::from_bytes(&signing_seed);
        let secret = hex_encode(signing_seed);
        let address = hex_encode(signing_key.verifying_key().to_bytes());
        Self { address, secret }
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub(super) fn sign_payload(&self, payload: &str) -> String {
        let seed =
            decode_hex_array::<PUBLIC_KEY_BYTES>(&self.secret).expect("wallet secret is valid hex");
        let signing_key = SigningKey::from_bytes(&seed);
        let signature: Signature = signing_key.sign(payload.as_bytes());
        hex_encode(signature.to_bytes())
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
