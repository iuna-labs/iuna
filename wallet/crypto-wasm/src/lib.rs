use ed25519_dalek::{Signer, SigningKey};
use ml_dsa::{Keypair, MlDsa44, Seed, SigningKey as MlDsaSigningKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use wasm_bindgen::prelude::*;

const TX_TAG: &[u8] = b"IUNA-TX-V2";
const ADDRESS_TAG: &[u8] = b"IUNA-ADDRESS-V1";
const ML_SEED_DOMAIN: &str = "iuna-wallet-ml-dsa44-seed-v1";
const ML_CHILD_DOMAIN: &str = "iuna-wallet-ml-dsa44-child-seed-v1";
const ED_SEED_DOMAIN: &str = "iuna-wallet-seed";
const BECH32M: u32 = 0x2bc8_30a3;
const CHARSET: &[u8; 32] = b"qpzry9x8gf2tvdw0s3jn54khce6mua7l";
const HYBRID_PUBLIC_KEY_BYTES: usize = 1_344;
const HYBRID_SIGNATURE_BYTES: usize = 2_484;
const MAX_BLOCK_BYTES: usize = 1_000_000;

#[derive(Clone, Copy)]
struct Address {
    version: u8,
    payload: [u8; 32],
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct DerivedAddress {
    index: u32,
    address: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TransferRequest {
    seed: String,
    chain_id: String,
    genesis_hash: String,
    recipient_address: String,
    amount: String,
    fee_rate: String,
    change_index: u32,
    utxos: Vec<HybridUtxo>,
}

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
struct HybridUtxo {
    address_index: u32,
    outpoint: Outpoint,
    output: Output,
}

#[derive(Deserialize, Clone)]
struct Outpoint {
    txid: String,
    index: u32,
}

#[derive(Deserialize, Clone)]
struct Output {
    amount: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct MigrationRequest {
    seed: String,
    chain_id: String,
    genesis_hash: String,
    destination_index: u32,
    fee_rate: String,
    utxos: Vec<LegacyUtxo>,
}

#[derive(Deserialize, Clone)]
struct LegacyUtxo {
    outpoint: Outpoint,
    output: Output,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BuiltTransaction {
    envelope: String,
    transaction_id: String,
    fee: String,
    input_count: usize,
}

#[wasm_bindgen]
pub fn derive_external_addresses(
    seed: &str,
    count: u32,
    network_id: &str,
) -> Result<String, JsValue> {
    if count == 0 || count > 10_000 {
        return Err(js_error("address count must be between 1 and 10000"));
    }
    let addresses = (0..count)
        .map(|index| {
            let (_, _, address) = hybrid_key(seed, index);
            DerivedAddress {
                index,
                address: encode_address(address, network_id),
            }
        })
        .collect::<Vec<_>>();
    serde_json::to_string(&addresses).map_err(js_error)
}

#[wasm_bindgen]
pub fn build_transfer(request_json: &str) -> Result<String, JsValue> {
    let request: TransferRequest = serde_json::from_str(request_json).map_err(js_error)?;
    let amount = parse_u64(&request.amount, "amount")?;
    let fee_rate = parse_positive_u64(&request.fee_rate, "fee rate")?;
    if amount == 0 {
        return Err(js_error("amount must be greater than zero"));
    }
    let recipient = decode_address(&request.recipient_address, &request.chain_id)?;
    if recipient.version != 1 {
        return Err(js_error("hybrid transfers require an address-v1 recipient"));
    }
    let genesis_hash = decode_array::<32>(&request.genesis_hash, "genesis hash")?;
    let mut available = request.utxos;
    available.sort_by(|left, right| {
        right
            .output
            .amount
            .cmp(&left.output.amount)
            .then_with(|| left.outpoint.txid.cmp(&right.outpoint.txid))
            .then_with(|| left.outpoint.index.cmp(&right.outpoint.index))
    });

    let mut fee = 1_u64;
    let (selected, total, outputs) = loop {
        let required = amount
            .checked_add(fee)
            .ok_or_else(|| js_error("amount plus fee overflows"))?;
        let mut total = 0_u64;
        let mut selected = Vec::new();
        for utxo in &available {
            total = total
                .checked_add(utxo.output.amount)
                .ok_or_else(|| js_error("input total overflows"))?;
            selected.push(utxo.clone());
            if total >= required {
                break;
            }
        }
        if total < required {
            return Err(js_error(
                "insufficient hybrid funds for the amount and network fee",
            ));
        }
        let mut outputs = vec![(recipient, amount)];
        let change = total - required;
        if change > 0 {
            let (_, _, change_address) = hybrid_key(&request.seed, request.change_index);
            outputs.push((change_address, change));
        }
        let size = transfer_encoded_size(&request.chain_id, selected.len(), outputs.len());
        if size > MAX_BLOCK_BYTES {
            return Err(js_error(
                "transaction exceeds the block budget; send a smaller amount or consolidate first",
            ));
        }
        let required_fee = fee_rate
            .checked_mul(size as u64)
            .ok_or_else(|| js_error("fee overflows"))?
            .max(1);
        if fee >= required_fee {
            break (selected, total, outputs);
        }
        fee = required_fee;
    };
    if total < amount + fee {
        return Err(js_error("insufficient hybrid funds after fee calculation"));
    }

    let mut signing = encode_prefix(&request.chain_id, genesis_hash)?;
    signing.push(1);
    push_u32(&mut signing, selected.len())?;
    for input in &selected {
        signing.extend_from_slice(&decode_array::<32>(
            &input.outpoint.txid,
            "v2 transaction ID",
        )?);
        signing.extend_from_slice(&input.outpoint.index.to_be_bytes());
        let (_, _, owner) = hybrid_key(&request.seed, input.address_index);
        encode_address_bytes(&mut signing, owner);
    }
    encode_outputs(&mut signing, &outputs)?;
    signing.extend_from_slice(&fee.to_be_bytes());

    let mut envelope = signing.clone();
    push_u32(&mut envelope, selected.len())?;
    for input in &selected {
        let (public_key, ml_seed, _) = hybrid_key(&request.seed, input.address_index);
        let signature = hybrid_signature(&request.seed, ml_seed, &signing)?;
        encode_authorization(&mut envelope, 2, &public_key, &signature)?;
    }
    built_json(envelope, fee, selected.len())
}

#[wasm_bindgen]
pub fn build_migration(request_json: &str) -> Result<String, JsValue> {
    let request: MigrationRequest = serde_json::from_str(request_json).map_err(js_error)?;
    let fee_rate = parse_positive_u64(&request.fee_rate, "fee rate")?;
    if request.utxos.is_empty() {
        return Err(js_error("no legacy outputs are available for migration"));
    }
    if request.utxos.len() > 1_000 {
        return Err(js_error("a migration can contain at most 1000 inputs"));
    }
    let genesis_hash = decode_array::<32>(&request.genesis_hash, "genesis hash")?;
    let total = request.utxos.iter().try_fold(0_u64, |sum, utxo| {
        sum.checked_add(utxo.output.amount)
            .ok_or_else(|| js_error("input total overflows"))
    })?;
    let encoded_size = migration_encoded_size(&request.chain_id, &request.utxos);
    if encoded_size > MAX_BLOCK_BYTES {
        return Err(js_error(
            "migration exceeds the block budget; migrate fewer legacy outputs at a time",
        ));
    }
    let fee = fee_rate
        .checked_mul(encoded_size as u64)
        .ok_or_else(|| js_error("fee overflows"))?
        .max(1);
    let migrated = total
        .checked_sub(fee)
        .filter(|amount| *amount > 0)
        .ok_or_else(|| js_error("migration fee exceeds the available legacy balance"))?;
    let ed_seed = derive_seed(ED_SEED_DOMAIN, &request.seed);
    let ed_public = SigningKey::from_bytes(&ed_seed).verifying_key().to_bytes();
    let (_, _, destination) = hybrid_key(&request.seed, request.destination_index);

    let mut signing = encode_prefix(&request.chain_id, genesis_hash)?;
    signing.push(4);
    push_u32(&mut signing, request.utxos.len())?;
    for input in &request.utxos {
        let id = decode_hex(&input.outpoint.txid, "legacy transaction ID")?;
        match id.len() {
            32 => signing.push(0),
            64 => signing.push(1),
            _ => {
                return Err(js_error(
                    "legacy transaction ID must contain 32 or 64 bytes",
                ));
            }
        }
        signing.extend_from_slice(&id);
        signing.extend_from_slice(&input.outpoint.index.to_be_bytes());
        encode_address_bytes(
            &mut signing,
            Address {
                version: 0,
                payload: ed_public,
            },
        );
    }
    encode_outputs(&mut signing, &[(destination, migrated)])?;
    signing.extend_from_slice(&fee.to_be_bytes());

    let signature = SigningKey::from_bytes(&ed_seed).sign(&signing).to_bytes();
    let mut envelope = signing;
    push_u32(&mut envelope, request.utxos.len())?;
    for _ in &request.utxos {
        encode_authorization(&mut envelope, 0, &ed_public, &signature)?;
    }
    built_json(envelope, fee, request.utxos.len())
}

fn hybrid_key(seed: &str, index: u32) -> ([u8; HYBRID_PUBLIC_KEY_BYTES], [u8; 32], Address) {
    let ed_seed = derive_seed(ED_SEED_DOMAIN, seed);
    let parent_ml_seed = derive_seed(ML_SEED_DOMAIN, seed);
    let ml_seed = if index == 0 {
        parent_ml_seed
    } else {
        derive_child_seed(parent_ml_seed, index)
    };
    let ed_public = SigningKey::from_bytes(&ed_seed).verifying_key().to_bytes();
    let ml_public = ml_public_key(ml_seed);
    let mut public_key = [0_u8; HYBRID_PUBLIC_KEY_BYTES];
    public_key[..32].copy_from_slice(&ed_public);
    public_key[32..].copy_from_slice(&ml_public);
    let mut hasher = Sha256::new();
    hasher.update(ADDRESS_TAG);
    hasher.update([2]);
    hasher.update(32_u32.to_be_bytes());
    hasher.update(ed_public);
    hasher.update(1_312_u32.to_be_bytes());
    hasher.update(ml_public);
    let address = Address {
        version: 1,
        payload: hasher.finalize().into(),
    };
    (public_key, ml_seed, address)
}

fn hybrid_signature(
    seed: &str,
    ml_seed: [u8; 32],
    payload: &[u8],
) -> Result<[u8; HYBRID_SIGNATURE_BYTES], JsValue> {
    let ed_seed = derive_seed(ED_SEED_DOMAIN, seed);
    let ed = SigningKey::from_bytes(&ed_seed).sign(payload).to_bytes();
    let ml_key = MlDsaSigningKey::<MlDsa44>::from_seed(&Seed::from(ml_seed));
    let ml = ml_key
        .expanded_key()
        .sign_deterministic(payload, &[])
        .map_err(|_| js_error("failed to create ML-DSA-44 signature"))?
        .encode();
    let mut signature = [0_u8; HYBRID_SIGNATURE_BYTES];
    signature[..64].copy_from_slice(&ed);
    signature[64..].copy_from_slice(ml.as_slice());
    Ok(signature)
}

fn ml_public_key(seed: [u8; 32]) -> [u8; 1_312] {
    MlDsaSigningKey::<MlDsa44>::from_seed(&Seed::from(seed))
        .verifying_key()
        .encode()
        .as_slice()
        .try_into()
        .expect("fixed ML-DSA public key")
}

fn derive_seed(domain: &str, seed: &str) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(domain.as_bytes());
    hasher.update(b":");
    hasher.update(seed.as_bytes());
    hasher.finalize().into()
}

fn derive_child_seed(parent: [u8; 32], index: u32) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(ML_CHILD_DOMAIN.as_bytes());
    hasher.update(b":external:");
    hasher.update(index.to_be_bytes());
    hasher.update(b":");
    hasher.update(parent);
    hasher.finalize().into()
}

fn encode_prefix(chain_id: &str, genesis_hash: [u8; 32]) -> Result<Vec<u8>, JsValue> {
    if chain_id.is_empty() || chain_id.len() > 64 || !chain_id.is_ascii() {
        return Err(js_error("invalid transaction v2 chain ID"));
    }
    let mut bytes = Vec::new();
    bytes.extend_from_slice(TX_TAG);
    bytes.extend_from_slice(&2_u16.to_be_bytes());
    push_bytes(&mut bytes, chain_id.as_bytes())?;
    bytes.extend_from_slice(&genesis_hash);
    Ok(bytes)
}

fn encode_outputs(bytes: &mut Vec<u8>, outputs: &[(Address, u64)]) -> Result<(), JsValue> {
    push_u32(bytes, outputs.len())?;
    for (address, amount) in outputs {
        if *amount == 0 {
            return Err(js_error("transaction outputs must be greater than zero"));
        }
        encode_address_bytes(bytes, *address);
        bytes.extend_from_slice(&amount.to_be_bytes());
    }
    Ok(())
}

fn encode_address_bytes(bytes: &mut Vec<u8>, address: Address) {
    bytes.push(address.version);
    bytes.extend_from_slice(&address.payload);
}

fn encode_authorization(
    bytes: &mut Vec<u8>,
    scheme: u8,
    public_key: &[u8],
    signature: &[u8],
) -> Result<(), JsValue> {
    bytes.push(scheme);
    push_bytes(bytes, public_key)?;
    push_bytes(bytes, signature)
}

fn transfer_encoded_size(chain_id: &str, inputs: usize, outputs: usize) -> usize {
    TX_TAG.len()
        + 2
        + 4
        + chain_id.len()
        + 32
        + 1
        + 4
        + inputs * 69
        + 4
        + outputs * 41
        + 8
        + 4
        + inputs * (1 + 4 + HYBRID_PUBLIC_KEY_BYTES + 4 + HYBRID_SIGNATURE_BYTES)
}

fn migration_encoded_size(chain_id: &str, inputs: &[LegacyUtxo]) -> usize {
    let input_bytes = inputs
        .iter()
        .map(|input| 1 + input.outpoint.txid.len() / 2 + 4 + 33)
        .sum::<usize>();
    TX_TAG.len()
        + 2
        + 4
        + chain_id.len()
        + 32
        + 1
        + 4
        + input_bytes
        + 4
        + 41
        + 8
        + 4
        + inputs.len() * (1 + 4 + 32 + 4 + 64)
}

fn built_json(envelope: Vec<u8>, fee: u64, input_count: usize) -> Result<String, JsValue> {
    let transaction_id = hex(&Sha256::digest(&envelope));
    serde_json::to_string(&BuiltTransaction {
        envelope: hex(&envelope),
        transaction_id,
        fee: fee.to_string(),
        input_count,
    })
    .map_err(js_error)
}

fn encode_address(address: Address, network_id: &str) -> String {
    let hrp = if network_id.contains("testnet") || network_id.contains("e2e") {
        "tiuna"
    } else {
        "iuna"
    };
    let mut data = vec![address.version];
    data.extend(convert_bits(&address.payload, 8, 5, true).expect("fixed payload converts"));
    let mut values = hrp_expand(hrp);
    values.extend_from_slice(&data);
    values.extend_from_slice(&[0; 6]);
    let checksum = polymod(&values) ^ BECH32M;
    let encoded = data
        .into_iter()
        .chain((0..6).map(|index| ((checksum >> (5 * (5 - index))) & 31) as u8))
        .map(|value| CHARSET[value as usize] as char)
        .collect::<String>();
    format!("{hrp}1{encoded}")
}

fn decode_address(value: &str, network_id: &str) -> Result<Address, JsValue> {
    let value = value.trim();
    if value.is_empty() || value.len() > 90 || !value.is_ascii() {
        return Err(js_error("address length or encoding is invalid"));
    }
    let has_lower = value.bytes().any(|byte| byte.is_ascii_lowercase());
    let has_upper = value.bytes().any(|byte| byte.is_ascii_uppercase());
    if has_lower && has_upper {
        return Err(js_error(
            "address must not mix uppercase and lowercase characters",
        ));
    }
    let expected = if network_id.contains("testnet") || network_id.contains("e2e") {
        "tiuna"
    } else {
        "iuna"
    };
    let normalized = value.to_ascii_lowercase();
    let separator = normalized
        .rfind('1')
        .ok_or_else(|| js_error("address is missing its separator"))?;
    if &normalized[..separator] != expected {
        return Err(js_error("address belongs to another network"));
    }
    let data = normalized[separator + 1..]
        .bytes()
        .map(|byte| {
            CHARSET
                .iter()
                .position(|candidate| *candidate == byte)
                .map(|index| index as u8)
                .ok_or_else(|| js_error("address contains an invalid character"))
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut checksum_values = hrp_expand(expected);
    checksum_values.extend_from_slice(&data);
    if polymod(&checksum_values) != BECH32M || data.len() < 7 {
        return Err(js_error("address checksum is invalid"));
    }
    let payload = &data[..data.len() - 6];
    let version = *payload
        .first()
        .ok_or_else(|| js_error("address payload is empty"))?;
    if version > 1 {
        return Err(js_error("unsupported address version"));
    }
    let bytes = convert_bits(&payload[1..], 5, 8, false)?;
    if bytes.len() != 32 {
        return Err(js_error("address payload must contain 32 bytes"));
    }
    Ok(Address {
        version,
        payload: bytes.try_into().expect("checked address length"),
    })
}

fn hrp_expand(hrp: &str) -> Vec<u8> {
    hrp.bytes()
        .map(|byte| byte >> 5)
        .chain(std::iter::once(0))
        .chain(hrp.bytes().map(|byte| byte & 31))
        .collect()
}

fn polymod(values: &[u8]) -> u32 {
    let generators = [0x3b6a57b2, 0x26508e6d, 0x1ea119fa, 0x3d4233dd, 0x2a1462b3];
    let mut checksum = 1_u32;
    for value in values {
        let top = checksum >> 25;
        checksum = ((checksum & 0x1ff_ffff) << 5) ^ u32::from(*value);
        for (index, generator) in generators.iter().enumerate() {
            if (top >> index) & 1 == 1 {
                checksum ^= generator;
            }
        }
    }
    checksum
}

fn convert_bits(data: &[u8], from: u32, to: u32, pad: bool) -> Result<Vec<u8>, JsValue> {
    let mut accumulator = 0_u32;
    let mut bits = 0_u32;
    let mut result = Vec::new();
    let max = (1_u32 << to) - 1;
    for value in data {
        if u32::from(*value) >> from != 0 {
            return Err(js_error("invalid address data"));
        }
        accumulator =
            ((accumulator << from) | u32::from(*value)) & ((1_u32 << (from + to - 1)) - 1);
        bits += from;
        while bits >= to {
            bits -= to;
            result.push(((accumulator >> bits) & max) as u8);
        }
    }
    if pad && bits > 0 {
        result.push(((accumulator << (to - bits)) & max) as u8);
    } else if !pad && (bits >= from || ((accumulator << (to - bits)) & max) != 0) {
        return Err(js_error("invalid address padding"));
    }
    Ok(result)
}

fn push_u32(bytes: &mut Vec<u8>, value: usize) -> Result<(), JsValue> {
    let value = u32::try_from(value).map_err(|_| js_error("value exceeds the wire limit"))?;
    bytes.extend_from_slice(&value.to_be_bytes());
    Ok(())
}

fn push_bytes(bytes: &mut Vec<u8>, value: &[u8]) -> Result<(), JsValue> {
    push_u32(bytes, value.len())?;
    bytes.extend_from_slice(value);
    Ok(())
}

fn parse_u64(value: &str, label: &str) -> Result<u64, JsValue> {
    value
        .parse::<u64>()
        .map_err(|_| js_error(format!("{label} is invalid")))
}

fn parse_positive_u64(value: &str, label: &str) -> Result<u64, JsValue> {
    parse_u64(value, label).and_then(|value| {
        if value == 0 {
            Err(js_error(format!("{label} must be greater than zero")))
        } else {
            Ok(value)
        }
    })
}

fn decode_array<const N: usize>(value: &str, label: &str) -> Result<[u8; N], JsValue> {
    decode_hex(value, label)?
        .try_into()
        .map_err(|_| js_error(format!("{label} must contain {N} bytes")))
}

fn decode_hex(value: &str, label: &str) -> Result<Vec<u8>, JsValue> {
    if value.len() % 2 != 0 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(js_error(format!("{label} is not hexadecimal")));
    }
    (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).map_err(js_error))
        .collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn js_error(error: impl std::fmt::Display) -> JsValue {
    JsValue::from_str(&error.to_string())
}
