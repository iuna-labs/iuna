use anyhow::{Context, Result, bail};

use crate::domain::{
    BlindedReveal, BlindedTransaction, Block, ChainSnapshot, FinalizerMode, LaunchProfile,
    LeaderProof, MaskedBlindedReveal, OutPoint, RevealBundleSection, RevealBundleSignature,
    Transaction, TxInput, TxOutput,
};

const COMPACT_SNAPSHOT_MAGIC: &[u8] = b"IUNA-SNAPSHOT";
const COMPACT_SNAPSHOT_VERSION: u8 = 3;

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
    let genesis_count = reader.usize()?;
    let mut genesis_allocations = std::collections::BTreeMap::new();
    for _ in 0..genesis_count {
        let address = reader.hex()?;
        let amount = reader.varint()?;
        genesis_allocations.insert(address, amount);
    }
    let vdf_rounds = reader.varint()?;
    let launch_profile = decode_launch_profile(&mut reader)?;
    let block_count = reader.usize()?;
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
    writer.varint(block.blinded_transactions.len() as u64);
    for transaction in &block.blinded_transactions {
        encode_blinded_transaction(writer, transaction)?;
    }
    encode_reveal_bundle_section(writer, &block.reveal_bundle_section)?;
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
    let blinded_transactions = decode_vec(reader, decode_blinded_transaction)?;
    let reveal_bundle_section = decode_reveal_bundle_section(reader)?;
    let transactions = decode_vec(reader, decode_transaction)?;
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
        blinded_transactions,
        reveal_bundle_section,
        transactions,
        hash,
    })
}

fn encode_blinded_transaction(
    writer: &mut CompactWriter,
    transaction: &BlindedTransaction,
) -> Result<()> {
    writer.hex(&transaction.commitment)?;
    encode_inputs(writer, &transaction.inputs)?;
    writer.varint(transaction.fee);
    writer.varint(u64::from(transaction.encrypted_size));
    writer.varint(transaction.expires_at_height);
    writer.hex(&transaction.nonce)?;
    writer.hex(&transaction.ciphertext)?;
    writer.hex(&transaction.payload_hash)?;
    Ok(())
}

fn decode_blinded_transaction(reader: &mut CompactReader<'_>) -> Result<BlindedTransaction> {
    Ok(BlindedTransaction {
        commitment: reader.hex()?,
        inputs: decode_inputs(reader)?,
        fee: reader.varint()?,
        encrypted_size: reader.u32()?,
        expires_at_height: reader.varint()?,
        nonce: reader.hex()?,
        ciphertext: reader.hex()?,
        payload_hash: reader.hex()?,
    })
}

fn encode_blinded_reveal(writer: &mut CompactWriter, reveal: &BlindedReveal) -> Result<()> {
    writer.hex(&reveal.commitment)?;
    writer.hex(&reveal.key)?;
    Ok(())
}

fn decode_blinded_reveal(reader: &mut CompactReader<'_>) -> Result<BlindedReveal> {
    Ok(BlindedReveal {
        commitment: reader.hex()?,
        key: reader.hex()?,
    })
}

fn encode_reveal_bundle_section(
    writer: &mut CompactWriter,
    section: &RevealBundleSection,
) -> Result<()> {
    writer.varint(section.signatures.len() as u64);
    for signature in &section.signatures {
        writer.varint(u64::from(signature.slot));
        writer.hex(&signature.member)?;
        writer.hex(&signature.signature)?;
    }
    writer.varint(section.reveals.len() as u64);
    for masked in &section.reveals {
        encode_blinded_reveal(writer, &masked.reveal)?;
        writer.u8(masked.bundle_mask);
    }
    Ok(())
}

fn decode_reveal_bundle_section(reader: &mut CompactReader<'_>) -> Result<RevealBundleSection> {
    let signatures = decode_vec(reader, |reader| {
        Ok(RevealBundleSignature {
            slot: u8::try_from(reader.varint()?).context("reveal bundle slot does not fit u8")?,
            member: reader.hex()?,
            signature: reader.hex()?,
        })
    })?;
    let reveals = decode_vec(reader, |reader| {
        Ok(MaskedBlindedReveal {
            reveal: decode_blinded_reveal(reader)?,
            bundle_mask: reader.u8()?,
        })
    })?;
    Ok(RevealBundleSection {
        signatures,
        reveals,
    })
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
    decode_vec(reader, |reader| {
        Ok(TxInput {
            outpoint: OutPoint {
                txid: reader.hexish()?,
                index: reader.u32()?,
            },
            owner: reader.hex()?,
            signature: reader.hexish()?,
        })
    })
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
    decode_vec(reader, |reader| {
        Ok(TxOutput {
            address: reader.hex()?,
            amount: reader.varint()?,
        })
    })
}

fn decode_vec<T>(
    reader: &mut CompactReader<'_>,
    mut decode: impl FnMut(&mut CompactReader<'_>) -> Result<T>,
) -> Result<Vec<T>> {
    let len = reader.usize()?;
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

    fn u32(&mut self) -> Result<u32> {
        self.varint()?
            .try_into()
            .context("compact integer does not fit u32")
    }

    fn string(&mut self) -> Result<String> {
        let len = self.usize()?;
        let bytes = self.take(len)?;
        String::from_utf8(bytes.to_vec()).context("compact string is not valid UTF-8")
    }

    fn hex(&mut self) -> Result<String> {
        let len = self.usize()?;
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
