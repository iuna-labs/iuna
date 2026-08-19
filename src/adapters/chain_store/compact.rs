use anyhow::{Context, Result, bail};

use crate::domain::{
    Block, BurnBundleSection, BurnBundleSignature, ChainSnapshot, FinalizerMode, LaunchProfile,
    LeaderProof, MaskedBurn, OutPoint, Transaction, TxInput, TxOutput,
};

const COMPACT_SNAPSHOT_MAGIC: &[u8] = b"IUNA-SNAPSHOT";
const COMPACT_SNAPSHOT_VERSION: u8 = 4;
const MAX_COMPACT_GENESIS_ALLOCATIONS: usize = 100_000;
const MAX_COMPACT_SNAPSHOT_BLOCKS: usize = 10_000;
const MAX_COMPACT_VEC_ITEMS: usize = 10_000;
const MAX_COMPACT_BYTE_FIELD: usize = 8 * 1024 * 1024;

pub(super) fn encode_compact_snapshot(snapshot: &ChainSnapshot) -> Result<Vec<u8>> {
    let mut writer = CompactWriter::default();
    writer.bytes(COMPACT_SNAPSHOT_MAGIC);
    writer.u8(COMPACT_SNAPSHOT_VERSION);
    writer.varint(snapshot.genesis_allocations.len() as u64);
    for (address, amount) in &snapshot.genesis_allocations {
        writer.hex(address)?;
        writer.varint(*amount);
    }
    writer.varint(snapshot.vdf_rounds);
    encode_launch_profile(&mut writer, &snapshot.launch_profile);
    writer.varint(snapshot.blocks.len() as u64);
    let mut expected_prev_hash = "0".repeat(64);
    for (height, block) in snapshot.blocks.iter().enumerate() {
        if block.height != height as u64 {
            bail!(
                "chain snapshot block height {} does not match compact position {}",
                block.height,
                height
            );
        }
        if block.prev_hash != expected_prev_hash {
            bail!(
                "chain snapshot block {} has non-canonical previous hash",
                height
            );
        }
        encode_block_body(&mut writer, block)?;
        expected_prev_hash = block.hash.clone();
    }
    Ok(writer.into_inner())
}

pub(super) fn decode_compact_snapshot(bytes: &[u8]) -> Result<ChainSnapshot> {
    let mut reader = CompactReader::new(bytes);
    reader.magic(COMPACT_SNAPSHOT_MAGIC)?;
    let version = reader.u8()?;
    if version != COMPACT_SNAPSHOT_VERSION {
        bail!("unsupported compact chain snapshot version {version}");
    }
    let genesis_count =
        reader.bounded_usize("genesis allocation count", MAX_COMPACT_GENESIS_ALLOCATIONS)?;
    let mut genesis_allocations = std::collections::BTreeMap::new();
    for _ in 0..genesis_count {
        let address = reader.hex()?;
        let amount = reader.varint()?;
        genesis_allocations.insert(address, amount);
    }
    let vdf_rounds = reader.varint()?;
    let launch_profile = decode_launch_profile(&mut reader)?;
    let block_count = reader.bounded_usize("block count", MAX_COMPACT_SNAPSHOT_BLOCKS)?;
    let mut blocks = Vec::with_capacity(block_count);
    let mut prev_hash = "0".repeat(64);
    for height in 0..block_count {
        let block = decode_block_body(&mut reader, height as u64, prev_hash)?;
        prev_hash = block.hash.clone();
        blocks.push(block);
    }
    reader.finish()?;
    Ok(ChainSnapshot {
        genesis_allocations,
        vdf_rounds,
        launch_profile,
        blocks,
    })
}

fn encode_launch_profile(writer: &mut CompactWriter, profile: &LaunchProfile) {
    writer.string(&profile.profile_id);
    writer.varint(profile.ticket_maturity_delay_heights);
    writer.varint(profile.ticket_expiry_window_heights);
    writer.varint(u64::from(profile.mine_difficulty_bits));
    writer.varint(profile.max_pending_transactions as u64);
    writer.varint(profile.max_block_transactions as u64);
    writer.varint(profile.max_block_bytes as u64);
}

fn decode_launch_profile(reader: &mut CompactReader<'_>) -> Result<LaunchProfile> {
    Ok(LaunchProfile {
        profile_id: reader.string()?,
        ticket_maturity_delay_heights: reader.varint()?,
        ticket_expiry_window_heights: reader.varint()?,
        mine_difficulty_bits: reader.u32()?,
        max_pending_transactions: reader.usize()?,
        max_block_transactions: reader.usize()?,
        max_block_bytes: reader.usize()?,
    })
}

fn encode_block_body(writer: &mut CompactWriter, block: &Block) -> Result<()> {
    writer.varint(block.timestamp_ms);
    writer.hex(&block.miner)?;
    writer.u8(match block.finalizer_mode {
        FinalizerMode::Ticket => 0,
        FinalizerMode::Recovery => 1,
    });
    writer.varint(u64::from(block.finalizer_rank));
    writer.varint(block.reward);
    writer.varint(block.vdf_rounds);
    writer.string(&block.vdf_output);
    writer.bool(block.leader_proof.is_some());
    if let Some(proof) = &block.leader_proof {
        writer.hexish(&proof.ticket_id)?;
        writer.hex(&proof.public_key)?;
        writer.hex(&proof.signature)?;
    }
    encode_burn_bundle_section(writer, &block.burn_bundle_section)?;
    writer.varint(block.transactions.len() as u64);
    for transaction in &block.transactions {
        encode_transaction(writer, transaction)?;
    }
    writer.hex(&block.hash)?;
    Ok(())
}

fn decode_block_body(
    reader: &mut CompactReader<'_>,
    height: u64,
    prev_hash: String,
) -> Result<Block> {
    let timestamp_ms = reader.varint()?;
    let miner = reader.hex()?;
    let finalizer_mode = match reader.u8()? {
        0 => FinalizerMode::Ticket,
        1 => FinalizerMode::Recovery,
        other => bail!("invalid finalizer mode tag {other}"),
    };
    let finalizer_rank = reader.u32()?;
    let reward = reader.varint()?;
    let vdf_rounds = reader.varint()?;
    let vdf_output = reader.string()?;
    let leader_proof = if reader.bool()? {
        Some(LeaderProof {
            ticket_id: reader.hexish()?,
            public_key: reader.hex()?,
            signature: reader.hex()?,
        })
    } else {
        None
    };
    let burn_bundle_section = decode_burn_bundle_section(reader)?;
    let transactions = decode_vec(
        reader,
        "block transaction count",
        MAX_COMPACT_VEC_ITEMS,
        decode_transaction,
    )?;
    let hash = reader.hex()?;
    Ok(Block {
        height,
        prev_hash,
        timestamp_ms,
        miner,
        finalizer_mode,
        finalizer_rank,
        reward,
        vdf_rounds,
        vdf_output,
        leader_proof,
        burn_bundle_section,
        transactions,
        hash,
    })
}

fn encode_burn_bundle_section(
    writer: &mut CompactWriter,
    section: &BurnBundleSection,
) -> Result<()> {
    writer.varint(section.signatures.len() as u64);
    for signature in &section.signatures {
        writer.varint(u64::from(signature.slot));
        writer.hex(&signature.member)?;
        writer.hex(&signature.signature)?;
    }
    writer.varint(section.burns.len() as u64);
    for masked in &section.burns {
        encode_transaction(writer, &masked.burn)?;
        writer.u8(masked.bundle_mask);
    }
    Ok(())
}

fn decode_burn_bundle_section(reader: &mut CompactReader<'_>) -> Result<BurnBundleSection> {
    let signatures = decode_vec(
        reader,
        "burn bundle signature count",
        MAX_COMPACT_VEC_ITEMS,
        |reader| {
            Ok(BurnBundleSignature {
                slot: u8::try_from(reader.varint()?).context("burn bundle slot does not fit u8")?,
                member: reader.hex()?,
                signature: reader.hex()?,
            })
        },
    )?;
    let burns = decode_vec(
        reader,
        "burn bundle burn count",
        MAX_COMPACT_VEC_ITEMS,
        |reader| {
            Ok(MaskedBurn {
                burn: decode_transaction(reader)?,
                bundle_mask: reader.u8()?,
            })
        },
    )?;
    Ok(BurnBundleSection { signatures, burns })
}

fn encode_transaction(writer: &mut CompactWriter, transaction: &Transaction) -> Result<()> {
    match transaction {
        Transaction::Transfer {
            inputs,
            outputs,
            fee,
            signature,
        } => {
            writer.u8(0);
            encode_inputs(writer, inputs)?;
            encode_outputs(writer, outputs)?;
            writer.varint(*fee);
            writer.hex(signature)?;
        }
        Transaction::Burn {
            inputs,
            change,
            amount,
            fee,
            signature,
        } => {
            writer.u8(1);
            encode_inputs(writer, inputs)?;
            encode_outputs(writer, change)?;
            writer.varint(*amount);
            writer.varint(*fee);
            writer.hexish(signature)?;
        }
        Transaction::Mine {
            recipient,
            anchor,
            salt,
            nonce,
            difficulty_bits,
            proof_header,
            signature,
        } => {
            writer.u8(2);
            writer.hex(recipient)?;
            writer.hex(anchor)?;
            writer.varint(*salt);
            writer.varint(*nonce);
            writer.varint(u64::from(*difficulty_bits));
            writer.bool(proof_header.is_some());
            if let Some(proof_header) = proof_header {
                writer.hex(proof_header)?;
            }
            writer.hex(signature)?;
        }
    }
    Ok(())
}

fn decode_transaction(reader: &mut CompactReader<'_>) -> Result<Transaction> {
    match reader.u8()? {
        0 => Ok(Transaction::Transfer {
            inputs: decode_inputs(reader)?,
            outputs: decode_outputs(reader)?,
            fee: reader.varint()?,
            signature: reader.hex()?,
        }),
        1 => Ok(Transaction::Burn {
            inputs: decode_inputs(reader)?,
            change: decode_outputs(reader)?,
            amount: reader.varint()?,
            fee: reader.varint()?,
            signature: reader.hexish()?,
        }),
        2 => {
            let recipient = reader.hex()?;
            let anchor = reader.hex()?;
            let salt = reader.varint()?;
            let nonce = reader.varint()?;
            let difficulty_bits = reader.u32()?;
            let proof_header = if reader.bool()? {
                Some(reader.hex()?)
            } else {
                None
            };
            let signature = reader.hex()?;
            Ok(Transaction::Mine {
                recipient,
                anchor,
                salt,
                nonce,
                difficulty_bits,
                proof_header,
                signature,
            })
        }
        other => bail!("invalid transaction tag {other}"),
    }
}

fn encode_inputs(writer: &mut CompactWriter, inputs: &[TxInput]) -> Result<()> {
    writer.varint(inputs.len() as u64);
    for input in inputs {
        writer.hexish(&input.outpoint.txid)?;
        writer.varint(u64::from(input.outpoint.index));
        writer.hex(&input.owner)?;
        writer.hexish(&input.signature)?;
    }
    Ok(())
}

fn decode_inputs(reader: &mut CompactReader<'_>) -> Result<Vec<TxInput>> {
    decode_vec(
        reader,
        "transaction input count",
        MAX_COMPACT_VEC_ITEMS,
        |reader| {
            Ok(TxInput {
                outpoint: OutPoint {
                    txid: reader.hexish()?,
                    index: reader.u32()?,
                },
                owner: reader.hex()?,
                signature: reader.hexish()?,
            })
        },
    )
}

fn encode_outputs(writer: &mut CompactWriter, outputs: &[TxOutput]) -> Result<()> {
    writer.varint(outputs.len() as u64);
    for output in outputs {
        writer.hex(&output.address)?;
        writer.varint(output.amount);
    }
    Ok(())
}

fn decode_outputs(reader: &mut CompactReader<'_>) -> Result<Vec<TxOutput>> {
    decode_vec(
        reader,
        "transaction output count",
        MAX_COMPACT_VEC_ITEMS,
        |reader| {
            Ok(TxOutput {
                address: reader.hex()?,
                amount: reader.varint()?,
            })
        },
    )
}

fn decode_vec<T>(
    reader: &mut CompactReader<'_>,
    label: &str,
    max_len: usize,
    mut decode: impl FnMut(&mut CompactReader<'_>) -> Result<T>,
) -> Result<Vec<T>> {
    let len = reader.bounded_usize(label, max_len)?;
    let mut values = Vec::with_capacity(len);
    for _ in 0..len {
        values.push(decode(reader)?);
    }
    Ok(values)
}

#[derive(Default)]
struct CompactWriter {
    bytes: Vec<u8>,
}

impl CompactWriter {
    fn into_inner(self) -> Vec<u8> {
        self.bytes
    }

    fn bytes(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }

    fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    fn bool(&mut self, value: bool) {
        self.u8(u8::from(value));
    }

    fn varint(&mut self, mut value: u64) {
        while value >= 0x80 {
            self.u8((value as u8) | 0x80);
            value >>= 7;
        }
        self.u8(value as u8);
    }

    fn string(&mut self, value: &str) {
        self.varint(value.len() as u64);
        self.bytes(value.as_bytes());
    }

    fn hex(&mut self, value: &str) -> Result<()> {
        let bytes = decode_hex(value)?;
        self.varint(bytes.len() as u64);
        self.bytes(&bytes);
        Ok(())
    }

    fn hexish(&mut self, value: &str) -> Result<()> {
        if value.len() % 2 == 0 && value.as_bytes().iter().all(|byte| byte.is_ascii_hexdigit()) {
            self.u8(1);
            self.hex(value)?;
        } else {
            self.u8(0);
            self.string(value);
        }
        Ok(())
    }
}

struct CompactReader<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl<'a> CompactReader<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, offset: 0 }
    }

    fn finish(&self) -> Result<()> {
        if self.offset != self.bytes.len() {
            bail!("compact chain snapshot has trailing bytes");
        }
        Ok(())
    }

    fn magic(&mut self, magic: &[u8]) -> Result<()> {
        let bytes = self.take(magic.len())?;
        if bytes != magic {
            bail!("invalid compact chain snapshot magic");
        }
        Ok(())
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(len)
            .context("compact chain snapshot offset overflow")?;
        if end > self.bytes.len() {
            bail!("unexpected end of compact chain snapshot");
        }
        let bytes = &self.bytes[self.offset..end];
        self.offset = end;
        Ok(bytes)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn bool(&mut self) -> Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            other => bail!("invalid compact bool tag {other}"),
        }
    }

    fn varint(&mut self) -> Result<u64> {
        let mut value = 0_u64;
        let mut shift = 0_u32;
        loop {
            let byte = self.u8()?;
            value |= u64::from(byte & 0x7f)
                .checked_shl(shift)
                .context("compact varint shift overflow")?;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
            shift += 7;
            if shift >= 64 {
                bail!("compact varint is too large");
            }
        }
    }

    fn usize(&mut self) -> Result<usize> {
        self.varint()?
            .try_into()
            .context("compact integer does not fit usize")
    }

    fn bounded_usize(&mut self, label: &str, max: usize) -> Result<usize> {
        let len = self.usize()?;
        if len > max {
            bail!("compact {label} {len} exceeds limit {max}");
        }
        Ok(len)
    }

    fn u32(&mut self) -> Result<u32> {
        self.varint()?
            .try_into()
            .context("compact integer does not fit u32")
    }

    fn string(&mut self) -> Result<String> {
        let len = self.bounded_usize("string length", MAX_COMPACT_BYTE_FIELD)?;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).context("compact string is not valid UTF-8")
    }

    fn hex(&mut self) -> Result<String> {
        let len = self.bounded_usize("byte field length", MAX_COMPACT_BYTE_FIELD)?;
        Ok(hex_encode(self.take(len)?))
    }

    fn hexish(&mut self) -> Result<String> {
        match self.u8()? {
            0 => self.string(),
            1 => self.hex(),
            other => bail!("invalid compact hexish tag {other}"),
        }
    }
}

fn decode_hex(input: &str) -> Result<Vec<u8>> {
    if input.len() % 2 != 0 {
        bail!("hex string has odd length");
    }
    let mut bytes = Vec::with_capacity(input.len() / 2);
    for pair in input.as_bytes().chunks_exact(2) {
        let high = hex_value(pair[0])?;
        let low = hex_value(pair[1])?;
        bytes.push((high << 4) | low);
    }
    Ok(bytes)
}

fn hex_value(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => bail!("invalid hex character"),
    }
}

fn hex_encode(bytes: impl AsRef<[u8]>) -> String {
    bytes
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::panic;

    use crate::domain::LaunchProfile;

    use super::{
        COMPACT_SNAPSHOT_MAGIC, COMPACT_SNAPSHOT_VERSION, CompactWriter, MAX_COMPACT_BYTE_FIELD,
        MAX_COMPACT_GENESIS_ALLOCATIONS, MAX_COMPACT_SNAPSHOT_BLOCKS, MAX_COMPACT_VEC_ITEMS,
        decode_compact_snapshot, encode_launch_profile,
    };

    fn snapshot_prefix(block_count: u64) -> Vec<u8> {
        let mut writer = CompactWriter::default();
        writer.bytes(COMPACT_SNAPSHOT_MAGIC);
        writer.u8(COMPACT_SNAPSHOT_VERSION);
        writer.varint(0);
        writer.varint(1);
        encode_launch_profile(&mut writer, &LaunchProfile::default());
        writer.varint(block_count);
        writer.into_inner()
    }

    fn empty_snapshot_bytes() -> Vec<u8> {
        snapshot_prefix(0)
    }

    fn block_body_prefix(writer: &mut CompactWriter) {
        writer.varint(1);
        writer.hex(&"0".repeat(64)).unwrap();
    }

    fn block_body_through_leader_proof_flag(writer: &mut CompactWriter) {
        block_body_prefix(writer);
        writer.u8(0);
        writer.varint(0);
        writer.varint(0);
        writer.varint(0);
        writer.string("0:0");
    }

    fn block_body_through_transaction_count(writer: &mut CompactWriter, tx_count: u64) {
        block_body_through_leader_proof_flag(writer);
        writer.bool(false);
        writer.varint(0);
        writer.varint(0);
        writer.varint(tx_count);
    }

    fn assert_decode_error_contains(bytes: &[u8], expected: &str) {
        let error = decode_compact_snapshot(bytes).unwrap_err().to_string();
        assert!(
            error.contains(expected),
            "expected error containing {expected:?}, got {error:?}"
        );
    }

    #[test]
    fn compact_snapshot_decoder_rejects_huge_lengths_before_allocation() {
        let mut huge_genesis = Vec::new();
        let mut writer = CompactWriter::default();
        writer.bytes(COMPACT_SNAPSHOT_MAGIC);
        writer.u8(COMPACT_SNAPSHOT_VERSION);
        writer.varint(MAX_COMPACT_GENESIS_ALLOCATIONS as u64 + 1);
        huge_genesis.extend(writer.into_inner());
        assert_decode_error_contains(&huge_genesis, "genesis allocation count");

        let huge_blocks = snapshot_prefix(MAX_COMPACT_SNAPSHOT_BLOCKS as u64 + 1);
        assert_decode_error_contains(&huge_blocks, "block count");

        let mut huge_transactions = snapshot_prefix(1);
        let mut writer = CompactWriter::default();
        block_body_through_transaction_count(&mut writer, MAX_COMPACT_VEC_ITEMS as u64 + 1);
        huge_transactions.extend(writer.into_inner());
        assert_decode_error_contains(&huge_transactions, "block transaction count");

        let mut huge_string = snapshot_prefix(1);
        let mut writer = CompactWriter::default();
        block_body_prefix(&mut writer);
        writer.u8(0);
        writer.varint(0);
        writer.varint(0);
        writer.varint(0);
        writer.varint(MAX_COMPACT_BYTE_FIELD as u64 + 1);
        huge_string.extend(writer.into_inner());
        assert_decode_error_contains(&huge_string, "string length");
    }

    #[test]
    fn compact_snapshot_decoder_rejects_oversized_varints() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(COMPACT_SNAPSHOT_MAGIC);
        bytes.push(COMPACT_SNAPSHOT_VERSION);
        bytes.extend_from_slice(&[0xff; 10]);

        assert_decode_error_contains(&bytes, "compact varint is too large");
    }

    #[test]
    fn compact_snapshot_decoder_rejects_trailing_bytes() {
        let mut bytes = empty_snapshot_bytes();
        bytes.push(0);

        assert_decode_error_contains(&bytes, "trailing bytes");
    }

    #[test]
    fn compact_snapshot_decoder_rejects_truncated_payloads() {
        let bytes = empty_snapshot_bytes();
        for len in 0..bytes.len() {
            assert!(
                decode_compact_snapshot(&bytes[..len]).is_err(),
                "truncated compact snapshot of length {len} decoded successfully"
            );
        }
    }

    #[test]
    fn compact_snapshot_decoder_rejects_invalid_tags() {
        let mut invalid_finalizer_mode = snapshot_prefix(1);
        let mut writer = CompactWriter::default();
        block_body_prefix(&mut writer);
        writer.u8(9);
        invalid_finalizer_mode.extend(writer.into_inner());
        assert_decode_error_contains(&invalid_finalizer_mode, "invalid finalizer mode tag 9");

        let mut invalid_bool = snapshot_prefix(1);
        let mut writer = CompactWriter::default();
        block_body_through_leader_proof_flag(&mut writer);
        writer.u8(2);
        invalid_bool.extend(writer.into_inner());
        assert_decode_error_contains(&invalid_bool, "invalid compact bool tag 2");

        let mut invalid_transaction = snapshot_prefix(1);
        let mut writer = CompactWriter::default();
        block_body_through_transaction_count(&mut writer, 1);
        writer.u8(99);
        invalid_transaction.extend(writer.into_inner());
        assert_decode_error_contains(&invalid_transaction, "invalid transaction tag 99");
    }

    #[test]
    fn compact_snapshot_decoder_random_bytes_do_not_panic() {
        let mut state = 0x5eed_5eed_1234_5678_u64;
        for len in 0..256 {
            let mut bytes = Vec::with_capacity(len);
            for _ in 0..len {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                bytes.push((state >> 32) as u8);
            }

            let result = panic::catch_unwind(|| {
                let _ = decode_compact_snapshot(&bytes);
            });
            assert!(result.is_ok(), "decoder panicked for random length {len}");
        }
    }
}
