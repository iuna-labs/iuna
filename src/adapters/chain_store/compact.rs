use std::{collections::BTreeMap, sync::Arc};

use anyhow::{Context, Result, bail};

use crate::domain::{
    Amount, Block, BurnBundleSection, BurnBundleSignature, ChainSnapshot, FinalizerMode,
    LaunchProfile, LeaderProof, MaskedBurn, OutPoint, Transaction, TxInput, TxOutput,
};

const COMPACT_SNAPSHOT_MAGIC: &[u8] = b"IUNA-SNAPSHOT";
const MIN_SUPPORTED_COMPACT_SNAPSHOT_VERSION: u8 = 6;
const COMPACT_SNAPSHOT_VERSION: u8 = 7;
const VDF_SOLUTION_PREFIX: &str = "classgroup-wesolowski-bqfc-v1:";
const MAX_COMPACT_GENESIS_ALLOCATIONS: usize = 100_000;
const MAX_COMPACT_SNAPSHOT_BLOCKS: usize = 10_000;
const MAX_COMPACT_VEC_ITEMS: usize = 10_000;
const MAX_COMPACT_BYTE_FIELD: usize = 8 * 1024 * 1024;

pub(super) fn legacy_compact_snapshot_version(bytes: &[u8]) -> Option<u8> {
    let version_offset = COMPACT_SNAPSHOT_MAGIC.len();
    if !bytes.starts_with(COMPACT_SNAPSHOT_MAGIC) || bytes.len() <= version_offset {
        return None;
    }
    let version = bytes[version_offset];
    (version < MIN_SUPPORTED_COMPACT_SNAPSHOT_VERSION).then_some(version)
}

#[derive(Clone, Debug, Default)]
struct EncodeTables {
    addresses: BTreeMap<String, u64>,
    protocol_ids: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct CompactBlockContext {
    tables: EncodeTables,
    size_breakdowns: Arc<BTreeMap<String, CompactBlockSizeBreakdown>>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub(crate) struct CompactBlockSizeBreakdown {
    pub(crate) total_bytes: usize,
    pub(crate) header_and_proof_bytes: usize,
    pub(crate) transaction_bytes: usize,
    pub(crate) transfer_bytes: usize,
    pub(crate) burn_bytes: usize,
    pub(crate) mine_bytes: usize,
    pub(crate) burn_bundle_bytes: usize,
}

impl CompactBlockContext {
    pub(crate) fn for_chain(
        genesis_allocations: &BTreeMap<String, Amount>,
        blocks: &[Block],
    ) -> Result<Self> {
        let mut context = Self::default();
        for address in genesis_allocations.keys() {
            context.tables.register_address(address);
        }
        for block in blocks {
            context.append_block_with_size_breakdown(block)?;
        }
        Ok(context)
    }

    pub(crate) fn block_size_bytes(&self, block: &Block) -> Result<usize> {
        Ok(self.block_size_breakdown(block)?.total_bytes)
    }

    pub(crate) fn block_size_breakdown(&self, block: &Block) -> Result<CompactBlockSizeBreakdown> {
        let mut tables = self.tables.clone();
        let mut writer = CompactWriter::default();
        encode_block_body_with_size_breakdown(&mut writer, block, &mut tables)
    }

    pub(crate) fn append_block(&mut self, block: &Block) -> Result<()> {
        self.append_block_with_size_breakdown(block).map(|_| ())
    }

    pub(crate) fn append_block_with_size_breakdown(
        &mut self,
        block: &Block,
    ) -> Result<CompactBlockSizeBreakdown> {
        let mut tables = self.tables.clone();
        let mut writer = CompactWriter::default();
        let breakdown = encode_block_body_with_size_breakdown(&mut writer, block, &mut tables)?;
        tables.register_protocol_id(&block.hash);
        self.tables = tables;
        Arc::make_mut(&mut self.size_breakdowns).insert(block.hash.clone(), breakdown.clone());
        Ok(breakdown)
    }

    pub(crate) fn append_trusted_block(&mut self, block: &Block) -> Result<()> {
        self.append_block(block)
    }

    pub(crate) fn stored_block_size_breakdown(
        &self,
        block_hash: &str,
    ) -> Option<&CompactBlockSizeBreakdown> {
        self.size_breakdowns.get(block_hash)
    }
}

#[derive(Default)]
struct DecodeTables {
    addresses: Vec<String>,
    address_indices: BTreeMap<String, u64>,
    protocol_ids: Vec<String>,
    protocol_id_indices: BTreeMap<String, u64>,
}

impl EncodeTables {
    fn register_address(&mut self, value: &str) {
        let next = self.addresses.len() as u64;
        self.addresses.entry(value.to_string()).or_insert(next);
    }

    fn register_protocol_id(&mut self, value: &str) {
        let next = self.protocol_ids.len() as u64;
        self.protocol_ids.entry(value.to_string()).or_insert(next);
    }
}

impl DecodeTables {
    fn register_address(&mut self, value: &str) {
        if !self.address_indices.contains_key(value) {
            let index = self.addresses.len() as u64;
            self.addresses.push(value.to_string());
            self.address_indices.insert(value.to_string(), index);
        }
    }

    fn register_protocol_id(&mut self, value: &str) {
        if !self.protocol_id_indices.contains_key(value) {
            let index = self.protocol_ids.len() as u64;
            self.protocol_ids.push(value.to_string());
            self.protocol_id_indices.insert(value.to_string(), index);
        }
    }
}

pub(super) fn encode_compact_snapshot(snapshot: &ChainSnapshot) -> Result<Vec<u8>> {
    let mut writer = CompactWriter::default();
    let mut tables = EncodeTables::default();
    writer.bytes(COMPACT_SNAPSHOT_MAGIC);
    let version = COMPACT_SNAPSHOT_VERSION;
    writer.u8(version);
    writer.varint(snapshot.genesis_allocations.len() as u64);
    for (address, amount) in &snapshot.genesis_allocations {
        writer.address(address, &mut tables)?;
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
            bail!("chain snapshot block {height} has non-canonical previous hash");
        }
        if block.hash != block.compute_hash() {
            bail!("chain snapshot block {height} has a non-canonical hash");
        }
        encode_block_body(&mut writer, block, &mut tables)?;
        tables.register_protocol_id(&block.hash);
        expected_prev_hash = block.hash.clone();
    }
    Ok(writer.into_inner())
}

pub(super) fn decode_compact_snapshot(bytes: &[u8]) -> Result<ChainSnapshot> {
    let mut reader = CompactReader::new(bytes);
    let mut tables = DecodeTables::default();
    reader.magic(COMPACT_SNAPSHOT_MAGIC)?;
    let version = reader.u8()?;
    if !(MIN_SUPPORTED_COMPACT_SNAPSHOT_VERSION..=COMPACT_SNAPSHOT_VERSION).contains(&version) {
        bail!("unsupported compact chain snapshot version {version}");
    }
    let genesis_count =
        reader.bounded_usize("genesis allocation count", MAX_COMPACT_GENESIS_ALLOCATIONS)?;
    let mut genesis_allocations = std::collections::BTreeMap::new();
    for _ in 0..genesis_count {
        let address = reader.address(&mut tables)?;
        let amount = reader.varint()?;
        genesis_allocations.insert(address, amount);
    }
    let vdf_rounds = reader.varint()?;
    let launch_profile = decode_launch_profile(&mut reader)?;
    let block_count = reader.bounded_usize("block count", MAX_COMPACT_SNAPSHOT_BLOCKS)?;
    let mut blocks = Vec::with_capacity(block_count);
    let mut prev_hash = "0".repeat(64);
    for height in 0..block_count {
        let block = decode_block_body(&mut reader, height as u64, prev_hash, &mut tables)?;
        tables.register_protocol_id(&block.hash);
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
    writer.varint(profile.burn_lineage_maturity_heights);
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
        burn_lineage_maturity_heights: reader.varint()?,
        max_pending_transactions: reader.usize()?,
        max_block_transactions: reader.usize()?,
        max_block_bytes: reader.usize()?,
    })
}

fn encode_block_body(
    writer: &mut CompactWriter,
    block: &Block,
    tables: &mut EncodeTables,
) -> Result<()> {
    encode_block_body_with_size_breakdown(writer, block, tables).map(|_| ())
}

fn encode_block_body_with_size_breakdown(
    writer: &mut CompactWriter,
    block: &Block,
    tables: &mut EncodeTables,
) -> Result<CompactBlockSizeBreakdown> {
    let block_start = writer.bytes.len();
    writer.varint(block.timestamp_ms);
    writer.address(&block.miner, tables)?;
    writer.u8(match block.finalizer_mode {
        FinalizerMode::Ticket => 0,
        FinalizerMode::Recovery => 1,
    });
    writer.varint(u64::from(block.finalizer_rank));
    writer.varint(block.reward);
    writer.varint(block.vdf_rounds);
    writer.compact_vdf_output(&block.vdf_output)?;
    writer.bool(block.leader_proof.is_some());
    if let Some(proof) = &block.leader_proof {
        writer.protocol_id_ref(&proof.ticket_id, tables)?;
        writer.address(&proof.public_key, tables)?;
        writer.fixed_hex::<64>(&proof.signature, "leader signature")?;
    }
    writer.varint(block.transactions.len() as u64);
    let header_end = writer.bytes.len();
    let transaction_start = writer.bytes.len();
    let mut transfer_bytes = 0_usize;
    let mut burn_bytes = 0_usize;
    let mut mine_bytes = 0_usize;
    for transaction in &block.transactions {
        let item_start = writer.bytes.len();
        encode_transaction(writer, transaction, tables)?;
        let item_bytes = writer.bytes.len().saturating_sub(item_start);
        match transaction {
            Transaction::Transfer { .. } => {
                transfer_bytes = transfer_bytes.saturating_add(item_bytes)
            }
            Transaction::Burn { .. } => burn_bytes = burn_bytes.saturating_add(item_bytes),
            Transaction::Mine { .. } => mine_bytes = mine_bytes.saturating_add(item_bytes),
        }
        tables.register_protocol_id(transaction.signature());
    }
    let burn_bundle_start = writer.bytes.len();
    encode_burn_bundle_section(writer, block, tables)?;
    let block_end = writer.bytes.len();
    Ok(CompactBlockSizeBreakdown {
        total_bytes: block_end.saturating_sub(block_start),
        header_and_proof_bytes: header_end.saturating_sub(block_start),
        transaction_bytes: burn_bundle_start.saturating_sub(transaction_start),
        transfer_bytes,
        burn_bytes,
        mine_bytes,
        burn_bundle_bytes: block_end.saturating_sub(burn_bundle_start),
    })
}

fn decode_block_body(
    reader: &mut CompactReader<'_>,
    height: u64,
    prev_hash: String,
    tables: &mut DecodeTables,
) -> Result<Block> {
    let timestamp_ms = reader.varint()?;
    let miner = reader.address(tables)?;
    let finalizer_mode = match reader.u8()? {
        0 => FinalizerMode::Ticket,
        1 => FinalizerMode::Recovery,
        other => bail!("invalid finalizer mode tag {other}"),
    };
    let finalizer_rank = reader.u32()?;
    let reward = reader.varint()?;
    let vdf_rounds = reader.varint()?;
    let vdf_output = reader.compact_vdf_output()?;
    let leader_proof = if reader.bool()? {
        Some(LeaderProof {
            ticket_id: reader.protocol_id_ref(tables)?,
            public_key: reader.address(tables)?,
            signature: reader.fixed_hex::<64>()?,
        })
    } else {
        None
    };
    let transaction_count =
        reader.bounded_usize("block transaction count", MAX_COMPACT_VEC_ITEMS)?;
    let mut transactions = Vec::with_capacity(transaction_count);
    for _ in 0..transaction_count {
        let transaction = decode_transaction(reader, tables)?;
        tables.register_protocol_id(transaction.signature());
        transactions.push(transaction);
    }
    let burn_bundle_section = decode_burn_bundle_section(reader, &transactions, tables)?;
    let mut block = Block {
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
        hash: String::new(),
    };
    block.hash = block.compute_hash();
    Ok(block)
}

fn encode_burn_bundle_section(
    writer: &mut CompactWriter,
    block: &Block,
    tables: &mut EncodeTables,
) -> Result<()> {
    let section = &block.burn_bundle_section;
    writer.varint(section.signatures.len() as u64);
    for signature in &section.signatures {
        writer.varint(u64::from(signature.slot));
        writer.address(&signature.member, tables)?;
        writer.fixed_hex::<64>(&signature.signature, "burn bundle signature")?;
    }
    writer.varint(section.burns.len() as u64);
    for masked in &section.burns {
        let transaction_index = block
            .transactions
            .iter()
            .position(|transaction| transaction == &masked.burn)
            .context("burn bundle transaction is missing from block transactions")?;
        writer.varint(transaction_index as u64);
        writer.u8(masked.bundle_mask);
    }
    Ok(())
}

fn decode_burn_bundle_section(
    reader: &mut CompactReader<'_>,
    transactions: &[Transaction],
    tables: &mut DecodeTables,
) -> Result<BurnBundleSection> {
    let signatures = decode_vec(
        reader,
        "burn bundle signature count",
        MAX_COMPACT_VEC_ITEMS,
        |reader| {
            Ok(BurnBundleSignature {
                slot: u8::try_from(reader.varint()?).context("burn bundle slot does not fit u8")?,
                member: reader.address(tables)?,
                signature: reader.fixed_hex::<64>()?,
            })
        },
    )?;
    let burns = decode_vec(
        reader,
        "burn bundle burn count",
        MAX_COMPACT_VEC_ITEMS,
        |reader| {
            let transaction_index = reader.bounded_usize(
                "burn bundle transaction index",
                transactions.len().saturating_sub(1),
            )?;
            let burn = transactions
                .get(transaction_index)
                .context("burn bundle transaction index is out of bounds")?
                .clone();
            if !burn.is_burn() {
                bail!("burn bundle transaction index does not reference a burn");
            }
            Ok(MaskedBurn {
                burn,
                bundle_mask: reader.u8()?,
            })
        },
    )?;
    Ok(BurnBundleSection { signatures, burns })
}

fn encode_transaction(
    writer: &mut CompactWriter,
    transaction: &Transaction,
    tables: &mut EncodeTables,
) -> Result<()> {
    match transaction {
        Transaction::Transfer {
            inputs,
            outputs,
            fee,
            signature,
        } => {
            writer.u8(0);
            let owner = common_input_owner(inputs)?;
            encode_outpoints(writer, inputs, tables)?;
            writer.address(owner, tables)?;
            encode_outputs(writer, outputs, tables)?;
            writer.varint(*fee);
            writer.fixed_hex::<64>(signature, "transfer signature")?;
            ensure_input_signatures(inputs, signature)?;
        }
        Transaction::Burn {
            inputs,
            change,
            amount,
            fee,
            anchor,
            signature,
        } => {
            writer.u8(1);
            let owner = common_input_owner(inputs)?;
            let genesis = inputs.iter().all(|input| input.signature == "genesis");
            if !genesis {
                ensure_input_signatures(inputs, signature)?;
            }
            let change_mode = match change.as_slice() {
                [] => 0,
                [output] if output.address == owner => 1,
                _ => 2,
            };
            writer.u8(change_mode | (u8::from(genesis) << 2) | (u8::from(anchor.is_some()) << 3));
            encode_outpoints(writer, inputs, tables)?;
            writer.address(owner, tables)?;
            match change_mode {
                0 => {}
                1 => writer.varint(change[0].amount),
                2 => encode_outputs(writer, change, tables)?,
                _ => unreachable!(),
            }
            writer.varint(*amount);
            writer.varint(*fee);
            if let Some(anchor) = anchor {
                writer.protocol_id_ref(anchor, tables)?;
            }
            if genesis {
                writer.fixed_hex::<32>(signature, "genesis burn signature")?;
            } else {
                writer.fixed_hex::<64>(signature, "burn signature")?;
            }
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
            writer.address(recipient, tables)?;
            writer.protocol_id_ref(anchor, tables)?;
            writer.varint(*salt);
            writer.varint(*nonce);
            writer.varint(u64::from(*difficulty_bits));
            writer.bool(proof_header.is_some());
            if let Some(proof_header) = proof_header {
                writer.fixed_hex::<80>(proof_header, "mine proof header")?;
            }
            writer.fixed_hex::<32>(signature, "mine signature")?;
        }
    }
    Ok(())
}

fn decode_transaction(
    reader: &mut CompactReader<'_>,
    tables: &mut DecodeTables,
) -> Result<Transaction> {
    match reader.u8()? {
        0 => {
            let outpoints = decode_outpoints(reader, tables)?;
            let owner = reader.address(tables)?;
            let outputs = decode_outputs(reader, tables)?;
            let fee = reader.varint()?;
            let signature = reader.fixed_hex::<64>()?;
            Ok(Transaction::Transfer {
                inputs: signed_inputs(outpoints, &owner, &signature),
                outputs,
                fee,
                signature,
            })
        }
        1 => {
            let mode = reader.u8()?;
            if mode & !0b1111 != 0 || mode & 0b11 > 2 {
                bail!("invalid compact burn mode {mode}");
            }
            let genesis = mode & 0b100 != 0;
            let anchored = mode & 0b1000 != 0;
            let outpoints = decode_outpoints(reader, tables)?;
            let owner = reader.address(tables)?;
            let change = match mode & 0b11 {
                0 => Vec::new(),
                1 => vec![TxOutput {
                    address: owner.clone(),
                    amount: reader.varint()?,
                }],
                2 => decode_outputs(reader, tables)?,
                _ => unreachable!(),
            };
            let amount = reader.varint()?;
            let fee = reader.varint()?;
            let anchor = if anchored {
                Some(reader.protocol_id_ref(tables)?)
            } else {
                None
            };
            let signature = if genesis {
                reader.fixed_hex::<32>()?
            } else {
                reader.fixed_hex::<64>()?
            };
            let input_signature = if genesis { "genesis" } else { &signature };
            Ok(Transaction::Burn {
                inputs: signed_inputs(outpoints, &owner, input_signature),
                change,
                amount,
                fee,
                anchor,
                signature,
            })
        }
        2 => {
            let recipient = reader.address(tables)?;
            let anchor = reader.protocol_id_ref(tables)?;
            let salt = reader.varint()?;
            let nonce = reader.varint()?;
            let difficulty_bits = reader.u32()?;
            let proof_header = if reader.bool()? {
                Some(reader.fixed_hex::<80>()?)
            } else {
                None
            };
            let signature = reader.fixed_hex::<32>()?;
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

fn common_input_owner(inputs: &[TxInput]) -> Result<&str> {
    let owner = inputs
        .first()
        .map(|input| input.owner.as_str())
        .context("stored transaction has no inputs")?;
    if inputs.iter().any(|input| input.owner != owner) {
        bail!("stored transaction inputs have different owners");
    }
    Ok(owner)
}

fn ensure_input_signatures(inputs: &[TxInput], signature: &str) -> Result<()> {
    if inputs.iter().any(|input| input.signature != signature) {
        bail!("stored transaction input signature differs from transaction signature");
    }
    Ok(())
}

fn encode_outpoints(
    writer: &mut CompactWriter,
    inputs: &[TxInput],
    tables: &mut EncodeTables,
) -> Result<()> {
    writer.varint(inputs.len() as u64);
    for input in inputs {
        writer.protocol_id_ref(&input.outpoint.txid, tables)?;
        writer.varint(u64::from(input.outpoint.index));
    }
    Ok(())
}

fn decode_outpoints(
    reader: &mut CompactReader<'_>,
    tables: &mut DecodeTables,
) -> Result<Vec<OutPoint>> {
    decode_vec(
        reader,
        "transaction input count",
        MAX_COMPACT_VEC_ITEMS,
        |reader| {
            Ok(OutPoint {
                txid: reader.protocol_id_ref(tables)?,
                index: reader.u32()?,
            })
        },
    )
}

fn signed_inputs(outpoints: Vec<OutPoint>, owner: &str, signature: &str) -> Vec<TxInput> {
    outpoints
        .into_iter()
        .map(|outpoint| TxInput {
            outpoint,
            owner: owner.to_string(),
            signature: signature.to_string(),
        })
        .collect()
}

fn encode_outputs(
    writer: &mut CompactWriter,
    outputs: &[TxOutput],
    tables: &mut EncodeTables,
) -> Result<()> {
    writer.varint(outputs.len() as u64);
    for output in outputs {
        writer.address(&output.address, tables)?;
        writer.varint(output.amount);
    }
    Ok(())
}

fn decode_outputs(
    reader: &mut CompactReader<'_>,
    tables: &mut DecodeTables,
) -> Result<Vec<TxOutput>> {
    decode_vec(
        reader,
        "transaction output count",
        MAX_COMPACT_VEC_ITEMS,
        |reader| {
            Ok(TxOutput {
                address: reader.address(tables)?,
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

    fn fixed_hex<const N: usize>(&mut self, value: &str, label: &str) -> Result<()> {
        let bytes = decode_hex(value).with_context(|| format!("invalid {label}"))?;
        if bytes.len() != N {
            bail!("invalid {label}: expected {N} bytes, got {}", bytes.len());
        }
        self.bytes(&bytes);
        Ok(())
    }

    fn address(&mut self, value: &str, tables: &mut EncodeTables) -> Result<()> {
        if let Some(index) = tables.addresses.get(value) {
            self.u8(0);
            self.varint(*index);
        } else {
            self.u8(1);
            self.fixed_hex::<32>(value, "address")?;
            tables.register_address(value);
        }
        Ok(())
    }

    fn protocol_id_ref(&mut self, value: &str, tables: &mut EncodeTables) -> Result<()> {
        if let Some(index) = tables.protocol_ids.get(value) {
            self.u8(0);
            self.varint(*index);
            return Ok(());
        }
        let bytes = decode_hex(value).context("invalid protocol id")?;
        match bytes.len() {
            32 => self.u8(1),
            64 => self.u8(2),
            length => bail!("invalid protocol id: expected 32 or 64 bytes, got {length}"),
        }
        self.bytes(&bytes);
        tables.register_protocol_id(value);
        Ok(())
    }

    fn compact_vdf_output(&mut self, value: &str) -> Result<()> {
        if let Some(hex) = value.strip_prefix(VDF_SOLUTION_PREFIX) {
            self.u8(2);
            self.hex(hex)?;
        } else if value.len() % 2 == 0
            && !value.is_empty()
            && value.as_bytes().iter().all(|byte| byte.is_ascii_hexdigit())
        {
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

    fn fixed_hex<const N: usize>(&mut self) -> Result<String> {
        Ok(hex_encode(self.take(N)?))
    }

    fn address(&mut self, tables: &mut DecodeTables) -> Result<String> {
        match self.u8()? {
            0 => {
                let index = self.usize()?;
                tables
                    .addresses
                    .get(index)
                    .cloned()
                    .context("compact address reference is out of bounds")
            }
            1 => {
                let value = self.fixed_hex::<32>()?;
                if tables.address_indices.contains_key(&value) {
                    bail!("compact address is encoded twice instead of referenced");
                }
                tables.register_address(&value);
                Ok(value)
            }
            other => bail!("invalid compact address tag {other}"),
        }
    }

    fn protocol_id_ref(&mut self, tables: &mut DecodeTables) -> Result<String> {
        match self.u8()? {
            0 => {
                let index = self.usize()?;
                tables
                    .protocol_ids
                    .get(index)
                    .cloned()
                    .context("compact protocol id reference is out of bounds")
            }
            tag @ (1 | 2) => {
                let value = if tag == 1 {
                    self.fixed_hex::<32>()?
                } else {
                    self.fixed_hex::<64>()?
                };
                if tables.protocol_id_indices.contains_key(&value) {
                    bail!("compact protocol id is encoded twice instead of referenced");
                }
                tables.register_protocol_id(&value);
                Ok(value)
            }
            other => bail!("invalid compact protocol id tag {other}"),
        }
    }

    fn compact_vdf_output(&mut self) -> Result<String> {
        match self.u8()? {
            0 => self.string(),
            1 => self.hex(),
            2 => Ok(format!("{VDF_SOLUTION_PREFIX}{}", self.hex()?)),
            other => bail!("invalid compact VDF output tag {other}"),
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
    use std::{collections::BTreeMap, panic};

    use crate::domain::{
        Block, BurnBundleSection, FinalizerMode, LaunchProfile, Ledger, MICRO_IUNA, MaskedBurn,
        OutPoint, Transaction, TxInput, TxOutput, Wallet,
    };

    use super::{
        COMPACT_SNAPSHOT_MAGIC, COMPACT_SNAPSHOT_VERSION, CompactBlockContext, CompactReader,
        CompactWriter, DecodeTables, EncodeTables, MAX_COMPACT_BYTE_FIELD,
        MAX_COMPACT_GENESIS_ALLOCATIONS, MAX_COMPACT_SNAPSHOT_BLOCKS, MAX_COMPACT_VEC_ITEMS,
        MIN_SUPPORTED_COMPACT_SNAPSHOT_VERSION, decode_block_body, decode_compact_snapshot,
        decode_launch_profile, decode_transaction, encode_block_body, encode_compact_snapshot,
        encode_launch_profile, encode_transaction,
    };

    #[test]
    fn compact_snapshot_v7_roundtrips_default_and_local_profiles() {
        let wallet = Wallet::from_seed("compact-profile-wire-version");
        let allocations = BTreeMap::from([(wallet.address().to_string(), MICRO_IUNA)]);
        let default_snapshot = Ledger::new(allocations.clone(), 1).snapshot();
        let default_bytes = encode_compact_snapshot(&default_snapshot).unwrap();
        assert_eq!(
            default_bytes[COMPACT_SNAPSHOT_MAGIC.len()],
            COMPACT_SNAPSHOT_VERSION
        );
        assert_eq!(
            decode_compact_snapshot(&default_bytes).unwrap(),
            default_snapshot
        );

        let local_ledger = Ledger::new_with_genesis_burns_and_profile(
            allocations,
            Vec::new(),
            1,
            LaunchProfile::local_testnet(),
        )
        .unwrap();
        let local_snapshot = local_ledger.snapshot();
        let local_bytes = encode_compact_snapshot(&local_snapshot).unwrap();
        assert_eq!(
            local_bytes[COMPACT_SNAPSHOT_MAGIC.len()],
            COMPACT_SNAPSHOT_VERSION
        );
        assert_eq!(
            decode_compact_snapshot(&local_bytes).unwrap(),
            local_snapshot
        );
    }

    #[test]
    fn compact_launch_profile_roundtrips_local_lineage_maturity() {
        let expected = LaunchProfile::local_testnet();
        let mut writer = CompactWriter::default();
        encode_launch_profile(&mut writer, &expected);
        let bytes = writer.into_inner();
        let mut reader = CompactReader::new(&bytes);

        let decoded = decode_launch_profile(&mut reader).unwrap();

        reader.finish().unwrap();
        assert_eq!(decoded, expected);
    }

    fn input(owner: &str, signature: &str) -> TxInput {
        TxInput {
            outpoint: OutPoint {
                txid: "1".repeat(128),
                index: 0,
            },
            owner: owner.to_string(),
            signature: signature.to_string(),
        }
    }

    fn assert_transaction_roundtrip(transaction: &Transaction) -> usize {
        let mut writer = CompactWriter::default();
        encode_transaction(&mut writer, transaction, &mut EncodeTables::default()).unwrap();
        let bytes = writer.into_inner();
        let mut reader = CompactReader::new(&bytes);
        let decoded = decode_transaction(&mut reader, &mut DecodeTables::default()).unwrap();
        reader.finish().unwrap();
        assert_eq!(&decoded, transaction);
        bytes.len()
    }

    #[test]
    fn compact_transactions_roundtrip_and_prioritize_burn_storage() {
        let owner = "2".repeat(64);
        let signature = "3".repeat(128);
        let burn = Transaction::Burn {
            inputs: vec![input(&owner, &signature)],
            change: vec![TxOutput {
                address: owner.clone(),
                amount: 10,
            }],
            amount: 20,
            fee: 1,
            anchor: None,
            signature: signature.clone(),
        };
        let transfer = Transaction::Transfer {
            inputs: vec![input(&owner, &signature)],
            outputs: vec![TxOutput {
                address: "4".repeat(64),
                amount: 10,
            }],
            fee: 1,
            signature,
        };
        let mine = Transaction::Mine {
            recipient: owner,
            anchor: "5".repeat(64),
            salt: 1,
            nonce: 1,
            difficulty_bits: 1,
            proof_header: None,
            signature: "6".repeat(64),
        };

        assert_eq!(assert_transaction_roundtrip(&burn), 169);
        assert_eq!(assert_transaction_roundtrip(&transfer), 201);
        assert_eq!(assert_transaction_roundtrip(&mine), 103);
    }

    #[test]
    fn compact_transaction_roundtrips_tip_bound_burn() {
        let owner = "2".repeat(64);
        let signature = "3".repeat(128);
        let anchor = "4".repeat(64);
        let burn = Transaction::Burn {
            inputs: vec![input(&owner, &signature)],
            change: Vec::new(),
            amount: 20,
            fee: 1,
            anchor: Some(anchor.clone()),
            signature,
        };
        let mut encode_tables = EncodeTables::default();
        encode_tables.register_protocol_id(&anchor);
        let mut writer = CompactWriter::default();
        encode_transaction(&mut writer, &burn, &mut encode_tables).unwrap();
        let bytes = writer.into_inner();
        let mut decode_tables = DecodeTables::default();
        decode_tables.register_protocol_id(&anchor);
        let mut reader = CompactReader::new(&bytes);

        let decoded = decode_transaction(&mut reader, &mut decode_tables).unwrap();

        reader.finish().unwrap();
        assert_eq!(decoded, burn);
    }

    #[test]
    fn repeated_burn_references_shrink_to_small_varints() {
        let owner = "2".repeat(64);
        let signature = "3".repeat(128);
        let spent_txid = "1".repeat(128);
        let burn = Transaction::Burn {
            inputs: vec![input(&owner, &signature)],
            change: vec![TxOutput {
                address: owner.clone(),
                amount: 10,
            }],
            amount: 20,
            fee: 1,
            anchor: None,
            signature,
        };
        let mut tables = EncodeTables::default();
        tables.register_address(&owner);
        tables.register_protocol_id(&spent_txid);
        let mut writer = CompactWriter::default();
        encode_transaction(&mut writer, &burn, &mut tables).unwrap();

        assert_eq!(writer.into_inner().len(), 75);
    }

    #[test]
    fn burn_bundle_section_stores_transaction_index_instead_of_burn_copy() {
        let owner = "2".repeat(64);
        let signature = "3".repeat(128);
        let burn = Transaction::Burn {
            inputs: vec![input(&owner, &signature)],
            change: Vec::new(),
            amount: 20,
            fee: 1,
            anchor: None,
            signature,
        };
        let block_with_section = |burn_bundle_section| {
            let mut block = Block {
                height: 0,
                prev_hash: "0".repeat(64),
                timestamp_ms: 1,
                miner: owner.clone(),
                finalizer_mode: FinalizerMode::Ticket,
                finalizer_rank: 0,
                reward: 0,
                vdf_rounds: 1,
                vdf_output: "vdf".to_string(),
                leader_proof: None,
                burn_bundle_section,
                transactions: vec![burn.clone()],
                hash: String::new(),
            };
            block.hash = block.compute_hash();
            block
        };
        let without = block_with_section(BurnBundleSection::default());
        let with = block_with_section(BurnBundleSection {
            signatures: Vec::new(),
            burns: vec![MaskedBurn {
                burn: burn.clone(),
                bundle_mask: 0b10,
            }],
        });
        let encode = |block: &Block| {
            let mut writer = CompactWriter::default();
            encode_block_body(&mut writer, block, &mut EncodeTables::default()).unwrap();
            writer.into_inner()
        };
        let without_bytes = encode(&without);
        let with_bytes = encode(&with);
        assert_eq!(with_bytes.len() - without_bytes.len(), 2);

        let mut context = CompactBlockContext::default();
        let breakdown = context.append_block_with_size_breakdown(&with).unwrap();
        assert_eq!(breakdown.total_bytes, with_bytes.len());
        assert_eq!(breakdown.burn_bundle_bytes, 4);
        assert!(breakdown.burn_bytes > 0);
        assert_eq!(breakdown.transfer_bytes, 0);
        assert_eq!(breakdown.mine_bytes, 0);
        assert_eq!(
            breakdown.total_bytes,
            breakdown.header_and_proof_bytes
                + breakdown.transaction_bytes
                + breakdown.burn_bundle_bytes
        );
        assert_eq!(
            context.stored_block_size_breakdown(&with.hash),
            Some(&breakdown)
        );

        let mut reader = CompactReader::new(&with_bytes);
        let decoded = decode_block_body(
            &mut reader,
            with.height,
            with.prev_hash.clone(),
            &mut DecodeTables::default(),
        )
        .unwrap();
        reader.finish().unwrap();
        assert_eq!(decoded, with);
    }

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
        writer.u8(1);
        writer
            .fixed_hex::<32>(&"0".repeat(64), "test miner")
            .unwrap();
    }

    fn block_body_through_leader_proof_flag(writer: &mut CompactWriter) {
        block_body_prefix(writer);
        writer.u8(0);
        writer.varint(0);
        writer.varint(0);
        writer.varint(0);
        writer.u8(0);
        writer.string("0:0");
    }

    fn block_body_through_transaction_count(writer: &mut CompactWriter, tx_count: u64) {
        block_body_through_leader_proof_flag(writer);
        writer.bool(false);
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
        writer.u8(0);
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
    fn compact_snapshot_decoder_rejects_pre_reset_versions() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(COMPACT_SNAPSHOT_MAGIC);
        bytes.push(MIN_SUPPORTED_COMPACT_SNAPSHOT_VERSION - 1);

        assert_decode_error_contains(&bytes, "unsupported compact chain snapshot version 5");
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
