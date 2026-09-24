use std::{
    collections::BTreeMap,
    fmt,
    sync::{
        Arc, OnceLock, RwLock,
        atomic::{AtomicU32, Ordering},
    },
};

use secrecy::{ExposeSecret, SecretBox, zeroize::Zeroize};
use sha2::{Digest, Sha256};

use super::block::LeaderProofPayload;
use super::{
    AddressNetwork, BurnBundle, BurnBundlePayload, LeaderProof, ProtocolPublicKey,
    ProtocolSignature, SignatureScheme, V2SpendingAuthorization, VersionedAddress,
    ed25519_public_key, encode_versioned_address, hex_encode, hybrid_key_commitment_address,
    ml_dsa44_public_key, sign_ed25519, sign_ml_dsa44,
};

const WALLET_SEED_DOMAIN: &str = "iuna-wallet-seed";
const WALLET_ML_DSA44_SEED_DOMAIN: &str = "iuna-wallet-ml-dsa44-seed-v1";
const WALLET_ML_DSA44_CHILD_SEED_DOMAIN: &str = "iuna-wallet-ml-dsa44-child-seed-v1";

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum HybridAddressBranch {
    External,
    Reward,
}

impl HybridAddressBranch {
    fn domain_label(self) -> &'static str {
        match self {
            Self::External => "external",
            Self::Reward => "reward",
        }
    }
}

struct HybridChildKey {
    signing_seed: SecretBox<[u8; 32]>,
    public_key: ProtocolPublicKey,
    address: VersionedAddress,
}

type HybridChildKeyCache = BTreeMap<(HybridAddressBranch, u32), Arc<HybridChildKey>>;

#[derive(Clone)]
pub struct Wallet {
    address: String,
    signing_seed: Arc<SecretBox<[u8; 32]>>,
    ml_dsa44_signing_seed: Arc<SecretBox<[u8; 32]>>,
    hybrid_public_key: Arc<OnceLock<ProtocolPublicKey>>,
    hybrid_child_keys: Arc<RwLock<HybridChildKeyCache>>,
    external_address_cursor: Arc<AtomicU32>,
    external_discovery_tip: Arc<RwLock<Option<String>>>,
    reward_address_cursor: Arc<AtomicU32>,
    reward_discovery_tip: Arc<RwLock<Option<String>>>,
}

impl Wallet {
    pub fn from_seed(seed: &str) -> Self {
        let signing_seed = derive_signing_seed(WALLET_SEED_DOMAIN, seed);
        let ml_dsa44_signing_seed = derive_signing_seed(WALLET_ML_DSA44_SEED_DOMAIN, seed);
        let address = hex_encode(ed25519_public_key(signing_seed.expose_secret()));
        Self {
            address,
            signing_seed: Arc::new(signing_seed),
            ml_dsa44_signing_seed: Arc::new(ml_dsa44_signing_seed),
            hybrid_public_key: Arc::new(OnceLock::new()),
            hybrid_child_keys: Arc::new(RwLock::new(BTreeMap::new())),
            external_address_cursor: Arc::new(AtomicU32::new(0)),
            external_discovery_tip: Arc::new(RwLock::new(None)),
            reward_address_cursor: Arc::new(AtomicU32::new(0)),
            reward_discovery_tip: Arc::new(RwLock::new(None)),
        }
    }

    pub fn address(&self) -> &str {
        &self.address
    }

    pub fn legacy_versioned_address(&self) -> VersionedAddress {
        VersionedAddress {
            version: super::AddressVersion::Ed25519PublicKey,
            payload: ed25519_public_key(self.signing_seed.expose_secret()),
        }
    }

    /// Returns the committed hybrid address derived from the existing wallet seed phrase.
    /// This does not make transaction v2 consensus-active.
    pub fn hybrid_versioned_address(&self) -> VersionedAddress {
        hybrid_key_commitment_address(self.hybrid_public_key())
            .expect("wallet always constructs a valid hybrid public key")
    }

    pub fn hybrid_address(&self, network: AddressNetwork) -> String {
        encode_versioned_address(self.hybrid_versioned_address(), network)
            .expect("wallet hybrid address has a valid fixed-size commitment")
    }

    pub fn hybrid_versioned_address_at(
        &self,
        branch: HybridAddressBranch,
        index: u32,
    ) -> VersionedAddress {
        if branch == HybridAddressBranch::External && index == 0 {
            return self.hybrid_versioned_address();
        }
        self.hybrid_child_key(branch, index).address
    }

    pub fn hybrid_address_at(
        &self,
        branch: HybridAddressBranch,
        index: u32,
        network: AddressNetwork,
    ) -> String {
        encode_versioned_address(self.hybrid_versioned_address_at(branch, index), network)
            .expect("wallet hybrid address has a valid fixed-size commitment")
    }

    pub fn hybrid_public_key(&self) -> &ProtocolPublicKey {
        self.hybrid_public_key.get_or_init(|| {
            let mut public_key =
                Vec::with_capacity(SignatureScheme::HybridEd25519MlDsa44.public_key_bytes());
            public_key.extend_from_slice(&ed25519_public_key(self.signing_seed.expose_secret()));
            public_key.extend_from_slice(&ml_dsa44_public_key(
                self.ml_dsa44_signing_seed.expose_secret(),
            ));
            ProtocolPublicKey::new(SignatureScheme::HybridEd25519MlDsa44, public_key)
                .expect("wallet hybrid public key has the scheme-defined length")
        })
    }

    pub(crate) fn external_address_cursor(&self) -> u32 {
        self.external_address_cursor.load(Ordering::Relaxed)
    }

    pub(crate) fn advance_external_address_cursor(&self, index: u32) {
        self.external_address_cursor
            .fetch_max(index, Ordering::Relaxed);
    }

    pub(crate) fn external_discovery_tip_matches(&self, tip: &str) -> bool {
        self.external_discovery_tip
            .read()
            .expect("wallet external discovery lock is not poisoned")
            .as_deref()
            == Some(tip)
    }

    pub(crate) fn mark_external_discovery_tip(&self, tip: &str) {
        *self
            .external_discovery_tip
            .write()
            .expect("wallet external discovery lock is not poisoned") = Some(tip.to_string());
    }

    pub(crate) fn reward_address_cursor(&self) -> u32 {
        self.reward_address_cursor.load(Ordering::Relaxed)
    }

    pub(crate) fn advance_reward_address_cursor(&self, index: u32) {
        self.reward_address_cursor
            .fetch_max(index, Ordering::Relaxed);
    }

    pub(crate) fn reward_discovery_tip_matches(&self, tip: &str) -> bool {
        self.reward_discovery_tip
            .read()
            .expect("wallet reward discovery lock is not poisoned")
            .as_deref()
            == Some(tip)
    }

    pub(crate) fn mark_reward_discovery_tip(&self, tip: &str) {
        *self
            .reward_discovery_tip
            .write()
            .expect("wallet reward discovery lock is not poisoned") = Some(tip.to_string());
    }

    /// Creates both signatures over the same canonical transaction-v2 payload.
    pub fn sign_hybrid_authorization(
        &self,
        payload: &[u8],
    ) -> anyhow::Result<V2SpendingAuthorization> {
        let mut signature =
            Vec::with_capacity(SignatureScheme::HybridEd25519MlDsa44.signature_bytes());
        signature.extend_from_slice(&sign_ed25519(self.signing_seed.expose_secret(), payload));
        signature.extend_from_slice(&sign_ml_dsa44(
            self.ml_dsa44_signing_seed.expose_secret(),
            payload,
        )?);
        V2SpendingAuthorization::new(
            self.hybrid_public_key().clone(),
            ProtocolSignature::new(SignatureScheme::HybridEd25519MlDsa44, signature)?,
        )
    }

    /// Signs a transaction-v2 input owned by this wallet. Existing version-0 value uses its
    /// original Ed25519 key; migrated version-1 value uses the hybrid key.
    pub fn sign_v2_authorization(
        &self,
        owner: VersionedAddress,
        payload: &[u8],
    ) -> anyhow::Result<V2SpendingAuthorization> {
        if owner == self.legacy_versioned_address() {
            return V2SpendingAuthorization::new(
                ProtocolPublicKey::new(SignatureScheme::Ed25519, owner.payload.to_vec())?,
                ProtocolSignature::new(
                    SignatureScheme::Ed25519,
                    sign_ed25519(self.signing_seed.expose_secret(), payload).to_vec(),
                )?,
            );
        }
        if owner == self.hybrid_versioned_address() {
            return self.sign_hybrid_authorization(payload);
        }
        let child = self
            .hybrid_child_keys
            .read()
            .expect("wallet hybrid child-key cache lock is not poisoned")
            .values()
            .find(|child| child.address == owner)
            .cloned();
        if let Some(child) = child {
            return sign_hybrid_authorization_with_key(
                self.signing_seed.expose_secret(),
                child.signing_seed.expose_secret(),
                &child.public_key,
                payload,
            );
        }
        anyhow::bail!("transaction v2 input is not owned by this wallet")
    }

    fn hybrid_child_key(&self, branch: HybridAddressBranch, index: u32) -> Arc<HybridChildKey> {
        let descriptor = (branch, index);
        if let Some(key) = self
            .hybrid_child_keys
            .read()
            .expect("wallet hybrid child-key cache lock is not poisoned")
            .get(&descriptor)
        {
            return Arc::clone(key);
        }
        let signing_seed =
            derive_child_signing_seed(self.ml_dsa44_signing_seed.expose_secret(), branch, index);
        let mut bytes =
            Vec::with_capacity(SignatureScheme::HybridEd25519MlDsa44.public_key_bytes());
        bytes.extend_from_slice(&ed25519_public_key(self.signing_seed.expose_secret()));
        bytes.extend_from_slice(&ml_dsa44_public_key(signing_seed.expose_secret()));
        let public_key = ProtocolPublicKey::new(SignatureScheme::HybridEd25519MlDsa44, bytes)
            .expect("wallet hybrid child public key has the scheme-defined length");
        let address = hybrid_key_commitment_address(&public_key)
            .expect("wallet always constructs a valid hybrid child public key");
        let key = Arc::new(HybridChildKey {
            signing_seed,
            public_key,
            address,
        });
        self.hybrid_child_keys
            .write()
            .expect("wallet hybrid child-key cache lock is not poisoned")
            .insert(descriptor, Arc::clone(&key));
        key
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
            reward_address: payload.reward_address,
            burns: payload.burns,
            burns_v2: payload.burns_v2,
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
            .field("ml_dsa44_signing_seed", &"[REDACTED]")
            .finish()
    }
}

impl PartialEq for Wallet {
    fn eq(&self, other: &Self) -> bool {
        self.address == other.address
    }
}

impl Eq for Wallet {}

fn derive_signing_seed(domain: &str, seed: &str) -> SecretBox<[u8; 32]> {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update(b":");
    hasher.update(seed.as_bytes());
    let mut seed_hash = hasher.finalize();
    let signing_seed = SecretBox::init_with_mut(|signing_seed: &mut [u8; 32]| {
        signing_seed.copy_from_slice(&seed_hash);
    });
    seed_hash.zeroize();
    signing_seed
}

fn derive_child_signing_seed(
    parent: &[u8; 32],
    branch: HybridAddressBranch,
    index: u32,
) -> SecretBox<[u8; 32]> {
    let mut hasher = Sha256::new();
    hasher.update(WALLET_ML_DSA44_CHILD_SEED_DOMAIN.as_bytes());
    hasher.update(b":");
    hasher.update(branch.domain_label().as_bytes());
    hasher.update(b":");
    hasher.update(index.to_be_bytes());
    hasher.update(b":");
    hasher.update(parent);
    let mut seed_hash = hasher.finalize();
    let signing_seed = SecretBox::init_with_mut(|signing_seed: &mut [u8; 32]| {
        signing_seed.copy_from_slice(&seed_hash);
    });
    seed_hash.zeroize();
    signing_seed
}

fn sign_hybrid_authorization_with_key(
    ed25519_signing_seed: &[u8; 32],
    ml_dsa44_signing_seed: &[u8; 32],
    public_key: &ProtocolPublicKey,
    payload: &[u8],
) -> anyhow::Result<V2SpendingAuthorization> {
    let mut signature = Vec::with_capacity(SignatureScheme::HybridEd25519MlDsa44.signature_bytes());
    signature.extend_from_slice(&sign_ed25519(ed25519_signing_seed, payload));
    signature.extend_from_slice(&sign_ml_dsa44(ml_dsa44_signing_seed, payload)?);
    V2SpendingAuthorization::new(
        public_key.clone(),
        ProtocolSignature::new(SignatureScheme::HybridEd25519MlDsa44, signature)?,
    )
}

#[cfg(test)]
mod tests {
    use secrecy::ExposeSecret;

    use super::{HybridAddressBranch, Wallet};
    use crate::domain::{
        AddressNetwork, LegacyTransactionId, SignatureScheme, TransactionV2, TransactionV2Domain,
        TransactionV2Input, TransactionV2LegacyInput, TransactionV2Output, hex_encode,
        verify_ed25519, verify_ml_dsa44,
    };

    #[test]
    fn debug_output_redacts_the_wallet_signing_seed() {
        let wallet = Wallet::from_seed("debug-redaction-wallet-seed");
        let signing_seed = hex_encode(wallet.signing_seed.expose_secret());
        let ml_dsa44_signing_seed = hex_encode(wallet.ml_dsa44_signing_seed.expose_secret());
        let debug = format!("{wallet:?}");

        assert!(debug.contains("[REDACTED]"));
        assert!(!debug.contains(&signing_seed));
        assert!(!debug.contains(&ml_dsa44_signing_seed));
    }

    #[test]
    fn existing_seed_deterministically_derives_a_hybrid_address() {
        let first = Wallet::from_seed("hybrid-wallet-seed");
        let second = Wallet::from_seed("hybrid-wallet-seed");
        let other = Wallet::from_seed("other-hybrid-wallet-seed");

        assert!(first.hybrid_public_key.get().is_none());
        assert_eq!(first.address(), second.address());
        assert_eq!(first.hybrid_public_key(), second.hybrid_public_key());
        assert_eq!(
            first.hybrid_address(AddressNetwork::Mainnet),
            "iuna1py9qlrnw6cm3mpz26hwwkww9spaa96zrw2g34gu0p4y3ea5cqhg0qa82x9s"
        );
        assert_eq!(
            first.hybrid_address_at(HybridAddressBranch::External, 1, AddressNetwork::Mainnet),
            "iuna1pkxdcktlzf5tc2p59xns2nccq7rg5zjhwg2zn5r9u73c5j9jxgk8s0e5l4e"
        );
        assert!(first.hybrid_public_key.get().is_some());
        assert_eq!(
            first.hybrid_address(AddressNetwork::Mainnet),
            second.hybrid_address(AddressNetwork::Mainnet)
        );
        assert_ne!(
            first.hybrid_address(AddressNetwork::Mainnet),
            other.hybrid_address(AddressNetwork::Mainnet)
        );
        assert_ne!(
            first.hybrid_address(AddressNetwork::Mainnet),
            first.hybrid_address(AddressNetwork::Testnet)
        );
        assert_eq!(
            first.hybrid_public_key().scheme(),
            SignatureScheme::HybridEd25519MlDsa44
        );
        assert_eq!(
            first.hybrid_address(AddressNetwork::Mainnet),
            "iuna1py9qlrnw6cm3mpz26hwwkww9spaa96zrw2g34gu0p4y3ea5cqhg0qa82x9s"
        );
    }

    #[test]
    fn browser_v2_transfer_vector_matches_node_encoding_and_signatures() {
        let wallet = Wallet::from_seed("hybrid-wallet-seed");
        let owner = wallet.hybrid_versioned_address_at(HybridAddressBranch::External, 0);
        let recipient = wallet.hybrid_versioned_address_at(HybridAddressBranch::External, 1);
        let change = wallet.hybrid_versioned_address_at(HybridAddressBranch::External, 2);
        let domain = TransactionV2Domain::new("iuna-mainnet-candidate", [0x11; 32]).unwrap();
        let mut transaction = TransactionV2::Transfer {
            inputs: vec![TransactionV2Input {
                outpoint_txid: [0x22; 32],
                outpoint_index: 7,
                owner,
            }],
            outputs: vec![
                TransactionV2Output {
                    address: recipient,
                    amount: 1_000_000,
                },
                TransactionV2Output {
                    address: change,
                    amount: 995_921,
                },
            ],
            fee: 4_079,
            authorizations: Vec::new(),
        };
        let payload = transaction.signing_bytes(&domain).unwrap();
        let authorization = wallet.sign_v2_authorization(owner, &payload).unwrap();
        let TransactionV2::Transfer { authorizations, .. } = &mut transaction else {
            unreachable!();
        };
        authorizations.push(authorization);

        transaction.verify_authorizations(&domain).unwrap();
        assert_eq!(transaction.encoded_size_bytes(&domain).unwrap(), 4_079);
        assert_eq!(
            hex_encode(transaction.transaction_id(&domain).unwrap()),
            "bda748b6ba9fd6550ca47d580f980a1c00d0050a20992750265a62d23b0b590d"
        );
    }

    #[test]
    fn browser_v2_migration_vector_matches_node_encoding_and_signature() {
        let wallet = Wallet::from_seed("hybrid-wallet-seed");
        let owner = wallet.legacy_versioned_address();
        let destination = wallet.hybrid_versioned_address_at(HybridAddressBranch::External, 2);
        let domain = TransactionV2Domain::new("iuna-mainnet-candidate", [0x11; 32]).unwrap();
        let mut transaction = TransactionV2::Migration {
            inputs: vec![TransactionV2LegacyInput {
                outpoint_id: LegacyTransactionId::Hash([0x33; 32]),
                outpoint_index: 4,
                owner,
            }],
            outputs: vec![TransactionV2Output {
                address: destination,
                amount: 1_999_693,
            }],
            fee: 307,
            authorizations: Vec::new(),
        };
        let payload = transaction.signing_bytes(&domain).unwrap();
        let authorization = wallet.sign_v2_authorization(owner, &payload).unwrap();
        let TransactionV2::Migration { authorizations, .. } = &mut transaction else {
            unreachable!();
        };
        authorizations.push(authorization);

        transaction.verify_authorizations(&domain).unwrap();
        assert_eq!(transaction.encoded_size_bytes(&domain).unwrap(), 307);
        assert_eq!(
            hex_encode(transaction.transaction_id(&domain).unwrap()),
            "37539c9d89a391c599b838c3f415e7adc93a36c22cd4fb5fe44a79433f336075"
        );
    }

    #[test]
    fn hybrid_authorization_signs_both_components_over_the_same_payload() {
        let wallet = Wallet::from_seed("hybrid-signing-wallet-seed");
        let payload = b"canonical transaction-v2 payload";
        let authorization = wallet.sign_hybrid_authorization(payload).unwrap();
        let signature = authorization.signature().as_bytes();
        let public_key = authorization.public_key().as_bytes();
        let ed25519_public_key: &[u8; 32] = public_key[..32].try_into().unwrap();
        let ed25519_signature: &[u8; 64] = signature[..64].try_into().unwrap();
        let ml_dsa44_public_key: &[u8; 1_312] = public_key[32..].try_into().unwrap();
        let ml_dsa44_signature: &[u8; 2_420] = signature[64..].try_into().unwrap();

        assert_eq!(
            authorization.committed_address().unwrap(),
            wallet.hybrid_versioned_address()
        );
        verify_ed25519(
            ed25519_public_key,
            payload,
            ed25519_signature,
            "wallet test",
        )
        .unwrap();
        verify_ml_dsa44(
            ml_dsa44_public_key,
            payload,
            ml_dsa44_signature,
            "wallet test",
        )
        .unwrap();
        assert!(
            verify_ml_dsa44(
                ml_dsa44_public_key,
                b"tampered",
                ml_dsa44_signature,
                "wallet test",
            )
            .is_err()
        );
    }

    #[test]
    fn v2_authorization_uses_the_scheme_required_by_the_owned_address() {
        let wallet = Wallet::from_seed("v2-migration-wallet-seed");
        let payload = b"migration payload";

        let legacy = wallet
            .sign_v2_authorization(wallet.legacy_versioned_address(), payload)
            .unwrap();
        assert_eq!(legacy.scheme(), SignatureScheme::Ed25519);
        assert_eq!(
            legacy.authorized_address().unwrap(),
            wallet.legacy_versioned_address()
        );

        let hybrid = wallet
            .sign_v2_authorization(wallet.hybrid_versioned_address(), payload)
            .unwrap();
        assert_eq!(hybrid.scheme(), SignatureScheme::HybridEd25519MlDsa44);
        assert!(
            wallet
                .sign_v2_authorization(
                    Wallet::from_seed("another-wallet").legacy_versioned_address(),
                    payload,
                )
                .is_err()
        );
    }

    #[test]
    fn child_hybrid_addresses_are_deterministic_separated_and_spendable() {
        let first = Wallet::from_seed("rotating-hybrid-wallet-seed");
        let restored = Wallet::from_seed("rotating-hybrid-wallet-seed");
        let external_zero = first.hybrid_versioned_address_at(HybridAddressBranch::External, 0);
        let external_one = first.hybrid_versioned_address_at(HybridAddressBranch::External, 1);
        let reward_zero = first.hybrid_versioned_address_at(HybridAddressBranch::Reward, 0);

        assert_eq!(external_zero, first.hybrid_versioned_address());
        assert_eq!(
            external_one,
            restored.hybrid_versioned_address_at(HybridAddressBranch::External, 1)
        );
        assert_ne!(external_one, external_zero);
        assert_ne!(reward_zero, external_one);

        let authorization = first
            .sign_v2_authorization(external_one, b"rotated child spend")
            .unwrap();
        assert_eq!(authorization.committed_address().unwrap(), external_one);
    }
}
