use std::collections::BTreeSet;

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

use super::{
    AddressVersion, ProtocolPublicKey, ProtocolSignature, SignatureScheme, VersionedAddress,
    hex_encode, verify_ed25519, verify_ml_dsa44,
};

const TRANSACTION_V2_TAG: &[u8] = b"IUNA-TX-V2";
const ADDRESS_V1_COMMITMENT_TAG: &[u8] = b"IUNA-ADDRESS-V1";
const MAX_CHAIN_ID_BYTES: usize = 64;
const MAX_V2_INPUTS: usize = 1_000;
const MAX_V2_OUTPUTS: usize = 1_000;
const STRATUM_PROOF_HEADER_BYTES: usize = 80;

/// Reserved wire version. It is deliberately separate from the live `Transaction` JSON type.
pub const TRANSACTION_V2_WIRE_VERSION: u16 = 2;

/// Fixed consensus activation height for the `iuna-mainnet-candidate` migration.
///
/// This is deliberately compiled into the protocol rather than exposed as an
/// operator-controlled feature flag. Live consensus must not route v2
/// transactions through this gate until the complete integration is present.
pub const TRANSACTION_V2_ACTIVATION_HEIGHT: Option<u64> = Some(3_000);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransactionV2Domain {
    chain_id: String,
    genesis_hash: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransactionV2Input {
    pub outpoint_txid: [u8; 32],
    pub outpoint_index: u32,
    pub owner: VersionedAddress,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum LegacyTransactionId {
    Hash([u8; 32]),
    Signature([u8; 64]),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransactionV2LegacyInput {
    pub outpoint_id: LegacyTransactionId,
    pub outpoint_index: u32,
    pub owner: VersionedAddress,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransactionV2Output {
    pub address: VersionedAddress,
    pub amount: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct V2SpendingAuthorization {
    public_key: ProtocolPublicKey,
    signature: ProtocolSignature,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum TransactionV2 {
    Migration {
        inputs: Vec<TransactionV2LegacyInput>,
        outputs: Vec<TransactionV2Output>,
        fee: u64,
        authorizations: Vec<V2SpendingAuthorization>,
    },
    Transfer {
        inputs: Vec<TransactionV2Input>,
        outputs: Vec<TransactionV2Output>,
        fee: u64,
        authorizations: Vec<V2SpendingAuthorization>,
    },
    Burn {
        inputs: Vec<TransactionV2Input>,
        change: Vec<TransactionV2Output>,
        amount: u64,
        fee: u64,
        anchor: Option<[u8; 32]>,
        authorizations: Vec<V2SpendingAuthorization>,
    },
    Mine {
        recipient: VersionedAddress,
        anchor: [u8; 32],
        salt: u64,
        nonce: u64,
        difficulty_bits: u32,
        proof_header: Option<[u8; STRATUM_PROOF_HEADER_BYTES]>,
        proof_hash: [u8; 32],
    },
}

impl TransactionV2Domain {
    pub fn new(chain_id: impl Into<String>, genesis_hash: [u8; 32]) -> Result<Self> {
        let chain_id = chain_id.into();
        if chain_id.is_empty() || chain_id.len() > MAX_CHAIN_ID_BYTES || !chain_id.is_ascii() {
            bail!("transaction v2 chain ID must contain 1..={MAX_CHAIN_ID_BYTES} ASCII bytes");
        }
        Ok(Self {
            chain_id,
            genesis_hash,
        })
    }

    pub fn chain_id(&self) -> &str {
        &self.chain_id
    }

    pub fn genesis_hash(&self) -> &[u8; 32] {
        &self.genesis_hash
    }
}

impl V2SpendingAuthorization {
    pub fn new(public_key: ProtocolPublicKey, signature: ProtocolSignature) -> Result<Self> {
        if public_key.scheme() != signature.scheme() {
            bail!("transaction v2 public key and signature schemes differ");
        }
        Ok(Self {
            public_key,
            signature,
        })
    }

    pub fn scheme(&self) -> SignatureScheme {
        self.public_key.scheme()
    }

    pub fn public_key(&self) -> &ProtocolPublicKey {
        &self.public_key
    }

    pub fn signature(&self) -> &ProtocolSignature {
        &self.signature
    }

    /// Computes the address authorized by this public key.
    pub fn authorized_address(&self) -> Result<VersionedAddress> {
        match self.public_key.scheme() {
            SignatureScheme::Ed25519 => Ok(VersionedAddress {
                version: AddressVersion::Ed25519PublicKey,
                payload: self
                    .public_key
                    .as_bytes()
                    .try_into()
                    .expect("validated Ed25519 public key length"),
            }),
            SignatureScheme::HybridEd25519MlDsa44 => {
                hybrid_key_commitment_address(&self.public_key)
            }
            SignatureScheme::MlDsa44 => {
                bail!("ML-DSA-only transaction v2 authorizations are not supported")
            }
        }
    }

    /// Computes the address-v1 commitment for a hybrid authorization.
    pub fn committed_address(&self) -> Result<VersionedAddress> {
        hybrid_key_commitment_address(&self.public_key)
    }
}

impl TransactionV2 {
    pub fn is_burn(&self) -> bool {
        matches!(self, Self::Burn { .. })
    }

    pub fn amount(&self) -> u64 {
        match self {
            Self::Burn { amount, .. } => *amount,
            Self::Migration { .. } | Self::Transfer { .. } | Self::Mine { .. } => 0,
        }
    }

    pub fn burn_anchor(&self) -> Option<[u8; 32]> {
        match self {
            Self::Burn { anchor, .. } => *anchor,
            Self::Migration { .. } | Self::Transfer { .. } | Self::Mine { .. } => None,
        }
    }

    pub fn burn_legacy_owner(&self) -> Result<Option<String>> {
        let Self::Burn {
            inputs,
            authorizations,
            ..
        } = self
        else {
            return Ok(None);
        };
        let owner = inputs
            .first()
            .context("transaction v2 burn has no owner")?
            .owner;
        if inputs.iter().any(|input| input.owner != owner) {
            bail!("transaction v2 burn inputs must have one owner");
        }
        let authorization = authorizations
            .first()
            .context("transaction v2 burn has no authorization")?;
        if authorization.scheme() != SignatureScheme::HybridEd25519MlDsa44
            || authorizations.iter().any(|candidate| {
                candidate.public_key().as_bytes() != authorization.public_key().as_bytes()
            })
        {
            bail!("transaction v2 burn must use one hybrid wallet identity");
        }
        if authorization.committed_address()? != owner {
            bail!("transaction v2 burn authorization does not match its owner");
        }
        Ok(Some(hex_encode(
            &authorization.public_key().as_bytes()[..32],
        )))
    }

    pub fn fee(&self) -> u64 {
        match self {
            Self::Migration { fee, .. } | Self::Transfer { fee, .. } | Self::Burn { fee, .. } => {
                *fee
            }
            Self::Mine { .. } => 0,
        }
    }

    /// Canonical bytes signed by every spending authorization. Signatures are excluded.
    pub fn signing_bytes(&self, domain: &TransactionV2Domain) -> Result<Vec<u8>> {
        self.validate_unsigned_shape()?;
        let mut bytes = encode_prefix(domain)?;
        self.encode_unsigned_body(&mut bytes)?;
        Ok(bytes)
    }

    /// Canonical, length-delimited wire encoding. This is not used by live gossip or blocks.
    pub fn encode(&self, domain: &TransactionV2Domain) -> Result<Vec<u8>> {
        self.validate_shape()?;
        let mut bytes = self.signing_bytes(domain)?;
        let authorizations = self.authorizations();
        encode_count(&mut bytes, authorizations.len(), "authorization count")?;
        for authorization in authorizations {
            bytes.push(authorization.scheme().wire_id());
            encode_bytes(
                &mut bytes,
                authorization.public_key().as_bytes(),
                "authorization public key",
            )?;
            encode_bytes(
                &mut bytes,
                authorization.signature().as_bytes(),
                "authorization signature",
            )?;
        }
        Ok(bytes)
    }

    pub fn decode(encoded: &[u8]) -> Result<(TransactionV2Domain, Self)> {
        let mut reader = Reader::new(encoded);
        if reader.take(TRANSACTION_V2_TAG.len(), "transaction v2 tag")? != TRANSACTION_V2_TAG {
            bail!("transaction v2 tag is invalid");
        }
        let version = reader.u16("transaction version")?;
        if version != TRANSACTION_V2_WIRE_VERSION {
            bail!("unsupported transaction version {version}");
        }
        let chain_id = reader.length_prefixed(MAX_CHAIN_ID_BYTES, "chain ID")?;
        let chain_id = std::str::from_utf8(chain_id)
            .context("transaction v2 chain ID is not UTF-8")?
            .to_string();
        let genesis_hash = reader.array::<32>("genesis hash")?;
        let domain = TransactionV2Domain::new(chain_id, genesis_hash)?;
        let kind = reader.u8("transaction kind")?;

        let unsigned = match kind {
            1 => UnsignedDecoded::Transfer {
                inputs: decode_inputs(&mut reader)?,
                outputs: decode_outputs(&mut reader)?,
                fee: reader.u64("transfer fee")?,
            },
            2 => {
                let inputs = decode_inputs(&mut reader)?;
                let change = decode_outputs(&mut reader)?;
                let amount = reader.u64("burn amount")?;
                let fee = reader.u64("burn fee")?;
                let anchor = decode_optional_array::<32>(&mut reader, "burn anchor")?;
                UnsignedDecoded::Burn {
                    inputs,
                    change,
                    amount,
                    fee,
                    anchor,
                }
            }
            3 => {
                let recipient = decode_address(&mut reader, "mine recipient")?;
                let anchor = reader.array::<32>("mine anchor")?;
                let salt = reader.u64("mine salt")?;
                let nonce = reader.u64("mine nonce")?;
                let difficulty_bits = reader.u32("mine difficulty")?;
                let proof_header = decode_optional_array::<STRATUM_PROOF_HEADER_BYTES>(
                    &mut reader,
                    "proof header",
                )?;
                let proof_hash = reader.array::<32>("proof hash")?;
                UnsignedDecoded::Mine {
                    recipient,
                    anchor,
                    salt,
                    nonce,
                    difficulty_bits,
                    proof_header,
                    proof_hash,
                }
            }
            4 => UnsignedDecoded::Migration {
                inputs: decode_legacy_inputs(&mut reader)?,
                outputs: decode_outputs(&mut reader)?,
                fee: reader.u64("migration fee")?,
            },
            _ => bail!("unsupported transaction v2 kind {kind}"),
        };

        let authorization_count = reader.count(MAX_V2_INPUTS, "authorization count")?;
        let mut authorizations = Vec::with_capacity(authorization_count);
        for _ in 0..authorization_count {
            let scheme_id = reader.u8("authorization scheme")?;
            let scheme = SignatureScheme::from_wire_id(scheme_id)
                .with_context(|| format!("unknown signature scheme {scheme_id}"))?;
            let public_key =
                reader.length_prefixed(scheme.public_key_bytes(), "authorization public key")?;
            if public_key.len() != scheme.public_key_bytes() {
                bail!("authorization public key has the wrong scheme-specific length");
            }
            let signature =
                reader.length_prefixed(scheme.signature_bytes(), "authorization signature")?;
            if signature.len() != scheme.signature_bytes() {
                bail!("authorization signature has the wrong scheme-specific length");
            }
            authorizations.push(V2SpendingAuthorization::new(
                ProtocolPublicKey::new(scheme, public_key.to_vec())?,
                ProtocolSignature::new(scheme, signature.to_vec())?,
            )?);
        }
        reader.finish()?;

        let transaction = unsigned.with_authorizations(authorizations)?;
        transaction.validate_shape()?;
        Ok((domain, transaction))
    }

    /// A v2 outpoint uses this fixed-size ID instead of a potentially variable signature.
    pub fn transaction_id(&self, domain: &TransactionV2Domain) -> Result<[u8; 32]> {
        Ok(Sha256::digest(self.encode(domain)?).into())
    }

    pub fn encoded_size_bytes(&self, domain: &TransactionV2Domain) -> Result<usize> {
        Ok(self.encode(domain)?.len())
    }

    pub fn validate_authorization_commitments(&self) -> Result<()> {
        self.validate_shape()?;
        match self {
            Self::Migration {
                inputs,
                authorizations,
                ..
            } => {
                for (input, authorization) in inputs.iter().zip(authorizations) {
                    if input.owner.version != AddressVersion::Ed25519PublicKey
                        || authorization.scheme() != SignatureScheme::Ed25519
                        || input.owner != authorization.authorized_address()?
                    {
                        bail!(
                            "transaction v2 migration authorization does not match its legacy owner"
                        );
                    }
                }
            }
            Self::Transfer {
                inputs,
                authorizations,
                ..
            }
            | Self::Burn {
                inputs,
                authorizations,
                ..
            } => {
                for (input, authorization) in inputs.iter().zip(authorizations) {
                    if input.owner.version != AddressVersion::HybridKeyCommitment
                        || authorization.scheme() != SignatureScheme::HybridEd25519MlDsa44
                        || input.owner != authorization.authorized_address()?
                    {
                        bail!(
                            "transaction v2 authorization does not match its hybrid owner commitment"
                        );
                    }
                }
            }
            Self::Mine { .. } => {}
        }
        Ok(())
    }

    /// Verifies the authorization required by each input version. Version-0 inputs retain their
    /// Ed25519 rule so existing value can migrate; version-1 inputs require both signature
    /// components. Live consensus must call `ensure_transaction_v2_active` before acceptance.
    pub fn verify_authorizations(&self, domain: &TransactionV2Domain) -> Result<()> {
        self.validate_authorization_commitments()?;
        let payload = self.signing_bytes(domain)?;
        for authorization in self.authorizations() {
            match authorization.scheme() {
                SignatureScheme::Ed25519 => {
                    let public_key = authorization
                        .public_key()
                        .as_bytes()
                        .try_into()
                        .expect("validated Ed25519 public key length");
                    let signature = authorization
                        .signature()
                        .as_bytes()
                        .try_into()
                        .expect("validated Ed25519 signature length");
                    verify_ed25519(public_key, &payload, signature, "transaction v2 input")?;
                }
                SignatureScheme::HybridEd25519MlDsa44 => {
                    let (ed25519_public_key, ml_dsa_public_key) =
                        authorization.public_key().as_bytes().split_at(32);
                    let (ed25519_signature, ml_dsa_signature) =
                        authorization.signature().as_bytes().split_at(64);
                    verify_ed25519(
                        ed25519_public_key
                            .try_into()
                            .expect("validated hybrid key length"),
                        &payload,
                        ed25519_signature
                            .try_into()
                            .expect("validated hybrid signature length"),
                        "transaction v2 classical component",
                    )?;
                    verify_ml_dsa44(
                        ml_dsa_public_key
                            .try_into()
                            .expect("validated hybrid key length"),
                        &payload,
                        ml_dsa_signature
                            .try_into()
                            .expect("validated hybrid signature length"),
                        "transaction v2 post-quantum component",
                    )?;
                }
                SignatureScheme::MlDsa44 => {
                    bail!("ML-DSA-only transaction v2 authorizations are not supported")
                }
            }
        }
        Ok(())
    }

    fn validate_shape(&self) -> Result<()> {
        self.validate_unsigned_shape()?;
        if self.authorizations().len() != self.input_count() {
            bail!("transaction v2 requires exactly one authorization per input");
        }
        Ok(())
    }

    fn validate_unsigned_shape(&self) -> Result<()> {
        if self.input_count() > MAX_V2_INPUTS {
            bail!("transaction v2 has too many inputs");
        }
        if self.outputs().len() > MAX_V2_OUTPUTS {
            bail!("transaction v2 has too many outputs");
        }
        if self.outputs().iter().any(|output| output.amount == 0) {
            bail!("transaction v2 outputs must be greater than zero");
        }
        match self {
            Self::Migration {
                inputs, outputs, ..
            } => {
                if inputs.is_empty() {
                    bail!("transaction v2 migration requires at least one input");
                }
                if outputs.len() != 1
                    || outputs[0].address.version != AddressVersion::HybridKeyCommitment
                {
                    bail!("transaction v2 migration requires exactly one address-v1 output");
                }
                let unique = inputs
                    .iter()
                    .map(|input| (&input.outpoint_id, input.outpoint_index))
                    .collect::<BTreeSet<_>>();
                if unique.len() != inputs.len() {
                    bail!("transaction v2 migration contains a duplicate input");
                }
            }
            Self::Transfer {
                inputs, outputs, ..
            } => {
                if inputs.is_empty() || outputs.is_empty() {
                    bail!("transaction v2 transfer requires inputs and outputs");
                }
                if outputs
                    .iter()
                    .any(|output| output.address.version != AddressVersion::HybridKeyCommitment)
                {
                    bail!("transaction v2 transfer outputs must use address v1");
                }
                let unique = inputs
                    .iter()
                    .map(|input| (input.outpoint_txid, input.outpoint_index))
                    .collect::<BTreeSet<_>>();
                if unique.len() != inputs.len() {
                    bail!("transaction v2 transfer contains a duplicate input");
                }
            }
            Self::Burn {
                inputs,
                change,
                amount,
                ..
            } => {
                if inputs.is_empty() || *amount == 0 {
                    bail!("transaction v2 burn requires inputs and a positive amount");
                }
                if change
                    .iter()
                    .any(|output| output.address.version != AddressVersion::HybridKeyCommitment)
                {
                    bail!("transaction v2 burn change must use address v1");
                }
                let unique = inputs
                    .iter()
                    .map(|input| (input.outpoint_txid, input.outpoint_index))
                    .collect::<BTreeSet<_>>();
                if unique.len() != inputs.len() {
                    bail!("transaction v2 burn contains a duplicate input");
                }
            }
            Self::Mine { .. } => {}
        }
        Ok(())
    }

    fn input_count(&self) -> usize {
        match self {
            Self::Migration { inputs, .. } => inputs.len(),
            Self::Transfer { inputs, .. } | Self::Burn { inputs, .. } => inputs.len(),
            Self::Mine { .. } => 0,
        }
    }

    fn outputs(&self) -> &[TransactionV2Output] {
        match self {
            Self::Migration { outputs, .. } | Self::Transfer { outputs, .. } => outputs,
            Self::Burn { change, .. } => change,
            Self::Mine { .. } => &[],
        }
    }

    fn authorizations(&self) -> &[V2SpendingAuthorization] {
        match self {
            Self::Migration { authorizations, .. }
            | Self::Transfer { authorizations, .. }
            | Self::Burn { authorizations, .. } => authorizations,
            Self::Mine { .. } => &[],
        }
    }

    fn encode_unsigned_body(&self, bytes: &mut Vec<u8>) -> Result<()> {
        match self {
            Self::Migration {
                inputs,
                outputs,
                fee,
                ..
            } => {
                bytes.push(4);
                encode_legacy_inputs(bytes, inputs)?;
                encode_outputs(bytes, outputs)?;
                bytes.extend_from_slice(&fee.to_be_bytes());
            }
            Self::Transfer {
                inputs,
                outputs,
                fee,
                ..
            } => {
                bytes.push(1);
                encode_inputs(bytes, inputs)?;
                encode_outputs(bytes, outputs)?;
                bytes.extend_from_slice(&fee.to_be_bytes());
            }
            Self::Burn {
                inputs,
                change,
                amount,
                fee,
                anchor,
                ..
            } => {
                bytes.push(2);
                encode_inputs(bytes, inputs)?;
                encode_outputs(bytes, change)?;
                bytes.extend_from_slice(&amount.to_be_bytes());
                bytes.extend_from_slice(&fee.to_be_bytes());
                encode_optional_array(bytes, anchor);
            }
            Self::Mine {
                recipient,
                anchor,
                salt,
                nonce,
                difficulty_bits,
                proof_header,
                proof_hash,
            } => {
                bytes.push(3);
                encode_address(bytes, recipient);
                bytes.extend_from_slice(anchor);
                bytes.extend_from_slice(&salt.to_be_bytes());
                bytes.extend_from_slice(&nonce.to_be_bytes());
                bytes.extend_from_slice(&difficulty_bits.to_be_bytes());
                encode_optional_array(bytes, proof_header);
                bytes.extend_from_slice(proof_hash);
            }
        }
        Ok(())
    }
}

pub const fn transaction_v2_is_active(height: u64) -> bool {
    match TRANSACTION_V2_ACTIVATION_HEIGHT {
        Some(activation_height) => height >= activation_height,
        None => false,
    }
}

pub fn ensure_transaction_v2_active(height: u64) -> Result<()> {
    if !transaction_v2_is_active(height) {
        bail!("transaction v2 is recognized but not consensus-active");
    }
    Ok(())
}

pub fn hybrid_key_commitment_address(public_key: &ProtocolPublicKey) -> Result<VersionedAddress> {
    if public_key.scheme() != SignatureScheme::HybridEd25519MlDsa44 {
        bail!("address v1 requires an Ed25519 + ML-DSA-44 public key");
    }
    Ok(VersionedAddress {
        version: AddressVersion::HybridKeyCommitment,
        payload: public_key_commitment(public_key),
    })
}

fn public_key_commitment(public_key: &ProtocolPublicKey) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(ADDRESS_V1_COMMITMENT_TAG);
    hasher.update([public_key.scheme().wire_id()]);
    match public_key.scheme() {
        SignatureScheme::HybridEd25519MlDsa44 => {
            let (ed25519, ml_dsa) = public_key.as_bytes().split_at(32);
            hash_length_prefixed(&mut hasher, ed25519);
            hash_length_prefixed(&mut hasher, ml_dsa);
        }
        SignatureScheme::Ed25519 | SignatureScheme::MlDsa44 => {
            hash_length_prefixed(&mut hasher, public_key.as_bytes());
        }
    }
    hasher.finalize().into()
}

fn hash_length_prefixed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u32).to_be_bytes());
    hasher.update(bytes);
}

fn encode_prefix(domain: &TransactionV2Domain) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(TRANSACTION_V2_TAG);
    bytes.extend_from_slice(&TRANSACTION_V2_WIRE_VERSION.to_be_bytes());
    encode_bytes(&mut bytes, domain.chain_id.as_bytes(), "chain ID")?;
    bytes.extend_from_slice(&domain.genesis_hash);
    Ok(bytes)
}

fn encode_inputs(bytes: &mut Vec<u8>, inputs: &[TransactionV2Input]) -> Result<()> {
    encode_count(bytes, inputs.len(), "input count")?;
    for input in inputs {
        bytes.extend_from_slice(&input.outpoint_txid);
        bytes.extend_from_slice(&input.outpoint_index.to_be_bytes());
        encode_address(bytes, &input.owner);
    }
    Ok(())
}

fn decode_inputs(reader: &mut Reader<'_>) -> Result<Vec<TransactionV2Input>> {
    let count = reader.count(MAX_V2_INPUTS, "input count")?;
    let mut inputs = Vec::with_capacity(count);
    for _ in 0..count {
        inputs.push(TransactionV2Input {
            outpoint_txid: reader.array::<32>("input transaction ID")?,
            outpoint_index: reader.u32("input output index")?,
            owner: decode_address(reader, "input owner")?,
        });
    }
    Ok(inputs)
}

fn encode_legacy_inputs(bytes: &mut Vec<u8>, inputs: &[TransactionV2LegacyInput]) -> Result<()> {
    encode_count(bytes, inputs.len(), "legacy input count")?;
    for input in inputs {
        match input.outpoint_id {
            LegacyTransactionId::Hash(hash) => {
                bytes.push(0);
                bytes.extend_from_slice(&hash);
            }
            LegacyTransactionId::Signature(signature) => {
                bytes.push(1);
                bytes.extend_from_slice(&signature);
            }
        }
        bytes.extend_from_slice(&input.outpoint_index.to_be_bytes());
        encode_address(bytes, &input.owner);
    }
    Ok(())
}

fn decode_legacy_inputs(reader: &mut Reader<'_>) -> Result<Vec<TransactionV2LegacyInput>> {
    let count = reader.count(MAX_V2_INPUTS, "legacy input count")?;
    let mut inputs = Vec::with_capacity(count);
    for _ in 0..count {
        let outpoint_id = match reader.u8("legacy input ID kind")? {
            0 => LegacyTransactionId::Hash(reader.array::<32>("legacy input hash")?),
            1 => LegacyTransactionId::Signature(
                reader.array::<64>("legacy input transaction signature")?,
            ),
            kind => bail!("unsupported legacy input ID kind {kind}"),
        };
        inputs.push(TransactionV2LegacyInput {
            outpoint_id,
            outpoint_index: reader.u32("legacy input output index")?,
            owner: decode_address(reader, "legacy input owner")?,
        });
    }
    Ok(inputs)
}

fn encode_outputs(bytes: &mut Vec<u8>, outputs: &[TransactionV2Output]) -> Result<()> {
    encode_count(bytes, outputs.len(), "output count")?;
    for output in outputs {
        encode_address(bytes, &output.address);
        bytes.extend_from_slice(&output.amount.to_be_bytes());
    }
    Ok(())
}

fn decode_outputs(reader: &mut Reader<'_>) -> Result<Vec<TransactionV2Output>> {
    let count = reader.count(MAX_V2_OUTPUTS, "output count")?;
    let mut outputs = Vec::with_capacity(count);
    for _ in 0..count {
        outputs.push(TransactionV2Output {
            address: decode_address(reader, "output address")?,
            amount: reader.u64("output amount")?,
        });
    }
    Ok(outputs)
}

fn encode_address(bytes: &mut Vec<u8>, address: &VersionedAddress) {
    bytes.push(address.version.wire_id());
    bytes.extend_from_slice(&address.payload);
}

fn decode_address(reader: &mut Reader<'_>, label: &str) -> Result<VersionedAddress> {
    let version_id = reader.u8(label)?;
    let version = AddressVersion::from_wire_id(version_id)
        .with_context(|| format!("unsupported address version {version_id}"))?;
    Ok(VersionedAddress {
        version,
        payload: reader.array::<32>(label)?,
    })
}

fn encode_optional_array<const N: usize>(bytes: &mut Vec<u8>, value: &Option<[u8; N]>) {
    match value {
        Some(value) => {
            bytes.push(1);
            bytes.extend_from_slice(value);
        }
        None => bytes.push(0),
    }
}

fn decode_optional_array<const N: usize>(
    reader: &mut Reader<'_>,
    label: &str,
) -> Result<Option<[u8; N]>> {
    match reader.u8(label)? {
        0 => Ok(None),
        1 => Ok(Some(reader.array::<N>(label)?)),
        marker => bail!("{label} has invalid presence marker {marker}"),
    }
}

fn encode_count(bytes: &mut Vec<u8>, count: usize, label: &str) -> Result<()> {
    let count = u32::try_from(count).with_context(|| format!("{label} exceeds u32"))?;
    bytes.extend_from_slice(&count.to_be_bytes());
    Ok(())
}

fn encode_bytes(bytes: &mut Vec<u8>, value: &[u8], label: &str) -> Result<()> {
    encode_count(bytes, value.len(), label)?;
    bytes.extend_from_slice(value);
    Ok(())
}

enum UnsignedDecoded {
    Migration {
        inputs: Vec<TransactionV2LegacyInput>,
        outputs: Vec<TransactionV2Output>,
        fee: u64,
    },
    Transfer {
        inputs: Vec<TransactionV2Input>,
        outputs: Vec<TransactionV2Output>,
        fee: u64,
    },
    Burn {
        inputs: Vec<TransactionV2Input>,
        change: Vec<TransactionV2Output>,
        amount: u64,
        fee: u64,
        anchor: Option<[u8; 32]>,
    },
    Mine {
        recipient: VersionedAddress,
        anchor: [u8; 32],
        salt: u64,
        nonce: u64,
        difficulty_bits: u32,
        proof_header: Option<[u8; STRATUM_PROOF_HEADER_BYTES]>,
        proof_hash: [u8; 32],
    },
}

impl UnsignedDecoded {
    fn with_authorizations(
        self,
        authorizations: Vec<V2SpendingAuthorization>,
    ) -> Result<TransactionV2> {
        Ok(match self {
            Self::Migration {
                inputs,
                outputs,
                fee,
            } => TransactionV2::Migration {
                inputs,
                outputs,
                fee,
                authorizations,
            },
            Self::Transfer {
                inputs,
                outputs,
                fee,
            } => TransactionV2::Transfer {
                inputs,
                outputs,
                fee,
                authorizations,
            },
            Self::Burn {
                inputs,
                change,
                amount,
                fee,
                anchor,
            } => TransactionV2::Burn {
                inputs,
                change,
                amount,
                fee,
                anchor,
                authorizations,
            },
            Self::Mine {
                recipient,
                anchor,
                salt,
                nonce,
                difficulty_bits,
                proof_header,
                proof_hash,
            } => {
                if !authorizations.is_empty() {
                    bail!("mine transaction v2 cannot contain spending authorizations");
                }
                TransactionV2::Mine {
                    recipient,
                    anchor,
                    salt,
                    nonce,
                    difficulty_bits,
                    proof_header,
                    proof_hash,
                }
            }
        })
    }
}

struct Reader<'a> {
    remaining: &'a [u8],
}

impl<'a> Reader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { remaining: bytes }
    }

    fn take(&mut self, length: usize, label: &str) -> Result<&'a [u8]> {
        if self.remaining.len() < length {
            bail!("transaction v2 {label} is truncated");
        }
        let (value, remaining) = self.remaining.split_at(length);
        self.remaining = remaining;
        Ok(value)
    }

    fn u8(&mut self, label: &str) -> Result<u8> {
        Ok(self.take(1, label)?[0])
    }

    fn u16(&mut self, label: &str) -> Result<u16> {
        Ok(u16::from_be_bytes(self.array(label)?))
    }

    fn u32(&mut self, label: &str) -> Result<u32> {
        Ok(u32::from_be_bytes(self.array(label)?))
    }

    fn u64(&mut self, label: &str) -> Result<u64> {
        Ok(u64::from_be_bytes(self.array(label)?))
    }

    fn array<const N: usize>(&mut self, label: &str) -> Result<[u8; N]> {
        Ok(self
            .take(N, label)?
            .try_into()
            .expect("reader returned requested fixed length"))
    }

    fn count(&mut self, maximum: usize, label: &str) -> Result<usize> {
        let count = self.u32(label)? as usize;
        if count > maximum {
            bail!("transaction v2 {label} exceeds {maximum}");
        }
        Ok(count)
    }

    fn length_prefixed(&mut self, maximum: usize, label: &str) -> Result<&'a [u8]> {
        let length = self.u32(label)? as usize;
        if length > maximum {
            bail!("transaction v2 {label} exceeds {maximum} bytes");
        }
        self.take(length, label)
    }

    fn finish(self) -> Result<()> {
        if !self.remaining.is_empty() {
            bail!("transaction v2 contains trailing bytes");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use ed25519_dalek::{Signer, SigningKey};
    use ml_dsa::{Keypair, MlDsa44, Seed, SigningKey as MlDsaSigningKey};

    use super::*;
    use crate::domain::hex::{decode_hex, hex_encode};

    fn domain() -> TransactionV2Domain {
        TransactionV2Domain::new("iuna-v2-test", [0x22; 32]).unwrap()
    }

    fn hybrid_public_key() -> ProtocolPublicKey {
        let signing_key = SigningKey::from_bytes(&[7; 32]);
        let mut public_key = signing_key.verifying_key().to_bytes().to_vec();
        let ml_dsa_signing_key = MlDsaSigningKey::<MlDsa44>::from_seed(&Seed::from([9; 32]));
        public_key.extend_from_slice(&ml_dsa_signing_key.verifying_key().encode());
        ProtocolPublicKey::new(SignatureScheme::HybridEd25519MlDsa44, public_key).unwrap()
    }

    fn hybrid_authorization(payload: &[u8]) -> V2SpendingAuthorization {
        let signing_key = SigningKey::from_bytes(&[7; 32]);
        let public_key = hybrid_public_key();
        let mut signature = signing_key.sign(payload).to_bytes().to_vec();
        let ml_dsa_signing_key = MlDsaSigningKey::<MlDsa44>::from_seed(&Seed::from([9; 32]));
        signature.extend_from_slice(
            &ml_dsa_signing_key
                .expanded_key()
                .sign_deterministic(payload, &[])
                .unwrap()
                .encode(),
        );
        V2SpendingAuthorization::new(
            public_key,
            ProtocolSignature::new(SignatureScheme::HybridEd25519MlDsa44, signature).unwrap(),
        )
        .unwrap()
    }

    fn ed25519_authorization(payload: &[u8]) -> V2SpendingAuthorization {
        let signing_key = SigningKey::from_bytes(&[7; 32]);
        V2SpendingAuthorization::new(
            ProtocolPublicKey::new(
                SignatureScheme::Ed25519,
                signing_key.verifying_key().to_bytes().to_vec(),
            )
            .unwrap(),
            ProtocolSignature::new(
                SignatureScheme::Ed25519,
                signing_key.sign(payload).to_bytes().to_vec(),
            )
            .unwrap(),
        )
        .unwrap()
    }

    fn unsigned_transfer(owner: VersionedAddress) -> TransactionV2 {
        TransactionV2::Transfer {
            inputs: vec![TransactionV2Input {
                outpoint_txid: [0x11; 32],
                outpoint_index: 7,
                owner,
            }],
            outputs: vec![TransactionV2Output {
                address: VersionedAddress {
                    version: AddressVersion::HybridKeyCommitment,
                    payload: [0x33; 32],
                },
                amount: 5,
            }],
            fee: 1,
            authorizations: Vec::new(),
        }
    }

    #[test]
    fn v2_activates_at_the_fixed_consensus_height() {
        assert_eq!(TRANSACTION_V2_ACTIVATION_HEIGHT, Some(3_000));
        assert!(!transaction_v2_is_active(0));
        assert!(!transaction_v2_is_active(2_999));
        assert!(ensure_transaction_v2_active(2_999).is_err());
        assert!(transaction_v2_is_active(3_000));
        assert!(transaction_v2_is_active(u64::MAX));
        ensure_transaction_v2_active(3_000).unwrap();
    }

    #[test]
    fn hybrid_transfer_roundtrips_and_has_a_hash_id() {
        let public_key = hybrid_public_key();
        let owner = hybrid_key_commitment_address(&public_key).unwrap();
        let mut transaction = unsigned_transfer(owner);
        let signing_bytes = transaction.signing_bytes(&domain()).unwrap();
        let authorization = hybrid_authorization(&signing_bytes);
        if let TransactionV2::Transfer { authorizations, .. } = &mut transaction {
            authorizations.push(authorization);
        }

        assert_eq!(
            transaction.authorizations()[0].committed_address().unwrap(),
            owner
        );
        transaction.validate_authorization_commitments().unwrap();
        transaction.verify_authorizations(&domain()).unwrap();
        let encoded = transaction.encode(&domain()).unwrap();
        let (decoded_domain, decoded) = TransactionV2::decode(&encoded).unwrap();
        assert_eq!(decoded_domain, domain());
        assert_eq!(decoded, transaction);
        assert_eq!(decoded.transaction_id(&domain()).unwrap().len(), 32);

        let mut missing_authorization = transaction.clone();
        if let TransactionV2::Transfer { authorizations, .. } = &mut missing_authorization {
            authorizations.clear();
        }
        assert!(
            missing_authorization
                .validate_authorization_commitments()
                .is_err()
        );

        let mut wrong_owner = transaction.clone();
        if let TransactionV2::Transfer { inputs, .. } = &mut wrong_owner {
            inputs[0].owner.payload[0] ^= 1;
        }
        assert!(wrong_owner.validate_authorization_commitments().is_err());

        let mut invalid_post_quantum_signature = transaction.clone();
        if let TransactionV2::Transfer { authorizations, .. } = &mut invalid_post_quantum_signature
        {
            let public_key = authorizations[0].public_key().clone();
            let mut signature = authorizations[0].signature().as_bytes().to_vec();
            signature[64] ^= 1;
            authorizations[0] = V2SpendingAuthorization::new(
                public_key,
                ProtocolSignature::new(SignatureScheme::HybridEd25519MlDsa44, signature).unwrap(),
            )
            .unwrap();
        }
        assert!(
            invalid_post_quantum_signature
                .verify_authorizations(&domain())
                .is_err()
        );
    }

    #[test]
    fn version_zero_input_can_migrate_to_a_hybrid_output() {
        let signing_key = SigningKey::from_bytes(&[7; 32]);
        let legacy_owner = VersionedAddress {
            version: AddressVersion::Ed25519PublicKey,
            payload: signing_key.verifying_key().to_bytes(),
        };
        let hybrid_recipient = hybrid_key_commitment_address(&hybrid_public_key()).unwrap();
        let mut transaction = TransactionV2::Migration {
            inputs: vec![TransactionV2LegacyInput {
                outpoint_id: LegacyTransactionId::Signature([0x11; 64]),
                outpoint_index: 7,
                owner: legacy_owner,
            }],
            outputs: vec![TransactionV2Output {
                address: hybrid_recipient,
                amount: 5,
            }],
            fee: 1,
            authorizations: Vec::new(),
        };
        let authorization = ed25519_authorization(&transaction.signing_bytes(&domain()).unwrap());
        if let TransactionV2::Migration { authorizations, .. } = &mut transaction {
            authorizations.push(authorization);
        }

        transaction.verify_authorizations(&domain()).unwrap();
        let encoded = transaction.encode(&domain()).unwrap();
        let (decoded_domain, decoded) = TransactionV2::decode(&encoded).unwrap();
        assert_eq!(decoded_domain, domain());
        assert_eq!(decoded, transaction);
        assert_eq!(
            transaction.authorizations()[0]
                .authorized_address()
                .unwrap(),
            legacy_owner
        );

        let mut wrong_scheme = transaction;
        let wrong_scheme_payload = wrong_scheme.signing_bytes(&domain()).unwrap();
        if let TransactionV2::Migration { authorizations, .. } = &mut wrong_scheme {
            *authorizations = vec![hybrid_authorization(&wrong_scheme_payload)];
        }
        assert!(wrong_scheme.verify_authorizations(&domain()).is_err());
    }

    #[test]
    fn decoder_fails_closed_for_versions_lengths_and_trailing_bytes() {
        let transaction = TransactionV2::Mine {
            recipient: VersionedAddress {
                version: AddressVersion::Ed25519PublicKey,
                payload: [3; 32],
            },
            anchor: [4; 32],
            salt: 5,
            nonce: 6,
            difficulty_bits: 7,
            proof_header: None,
            proof_hash: [8; 32],
        };
        let encoded = transaction.encode(&domain()).unwrap();
        assert_eq!(TransactionV2::decode(&encoded).unwrap().1, transaction);
        assert_eq!(
            hex_encode(&encoded),
            concat!(
                "49554e412d54582d5632", // IUNA-TX-V2
                "0002",                 // wire version
                "0000000c",
                "69756e612d76322d74657374", // iuna-v2-test
                "2222222222222222222222222222222222222222222222222222222222222222",
                "03", // mine
                "00", // address version 0
                "0303030303030303030303030303030303030303030303030303030303030303",
                "0404040404040404040404040404040404040404040404040404040404040404",
                "0000000000000005", // salt
                "0000000000000006", // nonce
                "00000007",         // difficulty
                "00",               // no proof header
                "0808080808080808080808080808080808080808080808080808080808080808",
                "00000000", // no spending authorizations
            )
        );
        assert_eq!(
            hex_encode(transaction.transaction_id(&domain()).unwrap()),
            "cd611549a0d156e10f9ffae00058ddd2f372f1d46564158a5ccf1621b79beaef"
        );

        let mut unknown_version = encoded.clone();
        unknown_version[TRANSACTION_V2_TAG.len() + 1] = 3;
        assert!(TransactionV2::decode(&unknown_version).is_err());

        let mut trailing = encoded.clone();
        trailing.push(0);
        assert!(TransactionV2::decode(&trailing).is_err());
        assert!(TransactionV2::decode(&encoded[..encoded.len() - 1]).is_err());
    }

    #[test]
    fn transaction_v2_fuzz_corpus_contains_valid_decoder_and_verifier_seeds() {
        for seed in [
            include_str!("../../fuzz/corpus/transaction_v2/valid_mine.hex"),
            include_str!("../../fuzz/corpus/transaction_v2/valid_hybrid_transfer.hex"),
        ] {
            let encoded = decode_hex(seed.trim().strip_prefix("hex:").unwrap()).unwrap();
            let (domain, transaction) = TransactionV2::decode(&encoded).unwrap();
            transaction.verify_authorizations(&domain).unwrap();
        }
    }

    #[test]
    fn transaction_id_commits_to_authorization_bytes() {
        let first = TransactionV2::Mine {
            recipient: VersionedAddress {
                version: AddressVersion::Ed25519PublicKey,
                payload: [3; 32],
            },
            anchor: [4; 32],
            salt: 5,
            nonce: 6,
            difficulty_bits: 7,
            proof_header: None,
            proof_hash: [8; 32],
        };
        let mut second = first.clone();
        if let TransactionV2::Mine { proof_hash, .. } = &mut second {
            proof_hash[0] ^= 1;
        }
        assert_ne!(
            first.transaction_id(&domain()).unwrap(),
            second.transaction_id(&domain()).unwrap()
        );
    }
}
