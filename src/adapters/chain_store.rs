use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};

use serde::Serialize;

use crate::domain::{
    Amount, BlindedReveal, BlindedTransaction, Block, ChainSnapshot, FinalizerMode, LaunchProfile,
    LeaderProof, Ledger, MINE_REWARD, OutPoint, RevealBundle, Transaction, TxInput, TxOutput,
    revealed_blinded_transactions,
};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS chain_snapshots (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    height INTEGER NOT NULL,
    tip_hash TEXT NOT NULL,
    snapshot_blob BLOB NOT NULL,
    updated_at_ms INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS block_metrics (
    height INTEGER PRIMARY KEY,
    block_hash TEXT NOT NULL,
    timestamp_ms INTEGER NOT NULL,
    block_time_ms INTEGER,
    mine_difficulty_bits INTEGER NOT NULL,
    circulating_supply INTEGER NOT NULL,
    transaction_count INTEGER NOT NULL,
    transfer_count INTEGER NOT NULL,
    burn_count INTEGER NOT NULL,
    mine_count INTEGER NOT NULL,
    burned_amount INTEGER NOT NULL,
    total_burned_amount INTEGER NOT NULL,
    fees_amount INTEGER NOT NULL,
    reward_amount INTEGER NOT NULL,
    vdf_rounds INTEGER NOT NULL,
    finalizer_rank INTEGER NOT NULL
);
"#;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockMetricRow {
    pub height: u64,
    pub block_hash: String,
    pub timestamp_ms: u64,
    pub block_time_ms: Option<u64>,
    pub mine_difficulty_bits: u32,
    pub circulating_supply: Amount,
    pub transaction_count: u64,
    pub transfer_count: u64,
    pub burn_count: u64,
    pub mine_count: u64,
    pub burned_amount: Amount,
    pub total_burned_amount: Amount,
    pub fees_amount: Amount,
    pub reward_amount: Amount,
    pub vdf_rounds: u64,
    pub finalizer_rank: u32,
}

#[derive(Clone, Debug)]
pub struct SqliteChainStore {
    path: PathBuf,
}

impl SqliteChainStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create chain database directory {}",
                    parent.display()
                )
            })?;
        }

        let store = Self { path };
        store.with_connection(|connection| {
            connection
                .execute_batch(SCHEMA)
                .context("failed to initialize chain database schema")
        })?;
        Ok(store)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Result<Option<ChainSnapshot>> {
        self.with_connection(|connection| {
            let snapshot_blob = connection
                .query_row(
                    "SELECT snapshot_blob FROM chain_snapshots WHERE id = 1",
                    [],
                    |row| row.get::<_, Vec<u8>>(0),
                )
                .optional()
                .context("failed to load chain snapshot from database")?;

            snapshot_blob
                .map(|blob| {
                    decode_compact_snapshot(&blob)
                        .context("failed to parse compact chain snapshot from database")
                })
                .transpose()
        })
    }

    pub fn save(&self, snapshot: &ChainSnapshot) -> Result<()> {
        self.save_with_metrics(snapshot, false)
    }

    pub fn save_with_metrics(&self, snapshot: &ChainSnapshot, keep_metrics: bool) -> Result<()> {
        let (height, tip_hash) = snapshot_tip(snapshot).context("cannot persist empty chain")?;
        let snapshot_blob =
            encode_compact_snapshot(snapshot).context("failed to encode compact chain snapshot")?;
        let updated_at_ms = unix_ms();
        let metrics = if keep_metrics {
            Some(metrics_from_snapshot(snapshot)?)
        } else {
            None
        };

        self.with_connection_mut(|connection| {
            let transaction = connection
                .transaction()
                .context("failed to start chain persistence transaction")?;
            transaction
                .execute(
                    r#"
INSERT INTO chain_snapshots (id, height, tip_hash, snapshot_blob, updated_at_ms)
VALUES (1, ?1, ?2, ?3, ?4)
ON CONFLICT(id) DO UPDATE SET
    height = excluded.height,
    tip_hash = excluded.tip_hash,
    snapshot_blob = excluded.snapshot_blob,
    updated_at_ms = excluded.updated_at_ms
"#,
                    params![height, tip_hash, snapshot_blob, updated_at_ms],
                )
                .context("failed to persist chain snapshot")?;
            match metrics {
                Some(metrics) => replace_metrics(&transaction, &metrics)?,
                None => clear_metrics_in_transaction(&transaction)?,
            }
            transaction
                .commit()
                .context("failed to commit chain persistence transaction")?;
            Ok(())
        })
    }

    pub fn replace_metrics_for_snapshot(&self, snapshot: &ChainSnapshot) -> Result<()> {
        let metrics = metrics_from_snapshot(snapshot)?;
        self.with_connection_mut(|connection| {
            let transaction = connection
                .transaction()
                .context("failed to start metrics transaction")?;
            replace_metrics(&transaction, &metrics)?;
            transaction
                .commit()
                .context("failed to commit metrics transaction")?;
            Ok(())
        })
    }

    pub fn clear_metrics(&self) -> Result<()> {
        self.with_connection(|connection| {
            connection
                .execute("DELETE FROM block_metrics", [])
                .context("failed to delete block metrics")?;
            Ok(())
        })
    }

    pub fn load_metrics(&self) -> Result<Vec<BlockMetricRow>> {
        self.with_connection(|connection| {
            let mut statement = connection
                .prepare(
                    r#"
SELECT height, block_hash, timestamp_ms, block_time_ms, mine_difficulty_bits,
       circulating_supply, transaction_count, transfer_count, burn_count,
       mine_count, burned_amount, total_burned_amount, fees_amount, reward_amount,
       vdf_rounds, finalizer_rank
FROM block_metrics
ORDER BY height ASC
"#,
                )
                .context("failed to prepare block metrics query")?;
            let rows = statement
                .query_map([], |row| {
                    Ok(BlockMetricRow {
                        height: row.get(0)?,
                        block_hash: row.get(1)?,
                        timestamp_ms: row.get(2)?,
                        block_time_ms: row.get(3)?,
                        mine_difficulty_bits: row.get(4)?,
                        circulating_supply: row.get(5)?,
                        transaction_count: row.get(6)?,
                        transfer_count: row.get(7)?,
                        burn_count: row.get(8)?,
                        mine_count: row.get(9)?,
                        burned_amount: row.get(10)?,
                        total_burned_amount: row.get(11)?,
                        fees_amount: row.get(12)?,
                        reward_amount: row.get(13)?,
                        vdf_rounds: row.get(14)?,
                        finalizer_rank: row.get(15)?,
                    })
                })
                .context("failed to load block metrics")?;
            rows.collect::<std::result::Result<Vec<_>, _>>()
                .context("failed to read block metrics rows")
        })
    }

    fn with_connection<T>(&self, work: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let connection = Connection::open(&self.path)
            .with_context(|| format!("failed to open chain database {}", self.path.display()))?;
        connection
            .execute_batch(
                r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
"#,
            )
            .context("failed to configure chain database")?;
        work(&connection)
    }

    fn with_connection_mut<T>(&self, work: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut connection = Connection::open(&self.path)
            .with_context(|| format!("failed to open chain database {}", self.path.display()))?;
        connection
            .execute_batch(
                r#"
PRAGMA journal_mode = WAL;
PRAGMA synchronous = NORMAL;
"#,
            )
            .context("failed to configure chain database")?;
        work(&mut connection)
    }
}

fn replace_metrics(
    transaction: &rusqlite::Transaction<'_>,
    metrics: &[BlockMetricRow],
) -> Result<()> {
    clear_metrics_in_transaction(transaction)?;
    for metric in metrics {
        transaction
            .execute(
                r#"
INSERT INTO block_metrics (
    height, block_hash, timestamp_ms, block_time_ms, mine_difficulty_bits,
    circulating_supply, transaction_count, transfer_count, burn_count, mine_count,
    burned_amount, total_burned_amount, fees_amount, reward_amount, vdf_rounds, finalizer_rank
) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
"#,
                params![
                    metric.height,
                    metric.block_hash,
                    metric.timestamp_ms,
                    metric.block_time_ms,
                    metric.mine_difficulty_bits,
                    metric.circulating_supply,
                    metric.transaction_count,
                    metric.transfer_count,
                    metric.burn_count,
                    metric.mine_count,
                    metric.burned_amount,
                    metric.total_burned_amount,
                    metric.fees_amount,
                    metric.reward_amount,
                    metric.vdf_rounds,
                    metric.finalizer_rank,
                ],
            )
            .with_context(|| format!("failed to insert metrics for block {}", metric.height))?;
    }
    Ok(())
}

fn clear_metrics_in_transaction(transaction: &rusqlite::Transaction<'_>) -> Result<()> {
    transaction
        .execute("DELETE FROM block_metrics", [])
        .context("failed to clear old block metrics")?;
    Ok(())
}

const COMPACT_SNAPSHOT_MAGIC: &[u8] = b"IUNA-SNAPSHOT";
const COMPACT_SNAPSHOT_VERSION: u8 = 2;

fn encode_compact_snapshot(snapshot: &ChainSnapshot) -> Result<Vec<u8>> {
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

fn decode_compact_snapshot(bytes: &[u8]) -> Result<ChainSnapshot> {
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
    writer.varint(block.reveal_bundles.len() as u64);
    for bundle in &block.reveal_bundles {
        encode_reveal_bundle(writer, bundle)?;
    }
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
    let reveal_bundles = decode_vec(reader, decode_reveal_bundle)?;
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
        reveal_bundles,
        transactions,
        hash,
    })
}

fn encode_blinded_transaction(
    writer: &mut CompactWriter,
    transaction: &BlindedTransaction,
) -> Result<()> {
    writer.hex(&transaction.commitment)?;
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

fn encode_reveal_bundle(writer: &mut CompactWriter, bundle: &RevealBundle) -> Result<()> {
    writer.varint(bundle.height);
    writer.hex(&bundle.prev_hash)?;
    writer.varint(u64::from(bundle.slot));
    writer.hex(&bundle.member)?;
    writer.varint(bundle.reveals.len() as u64);
    for reveal in &bundle.reveals {
        encode_blinded_reveal(writer, reveal)?;
    }
    writer.hex(&bundle.signature)?;
    Ok(())
}

fn decode_reveal_bundle(reader: &mut CompactReader<'_>) -> Result<RevealBundle> {
    Ok(RevealBundle {
        height: reader.varint()?,
        prev_hash: reader.hex()?,
        slot: u8::try_from(reader.varint()?).context("reveal bundle slot does not fit u8")?,
        member: reader.hex()?,
        reveals: decode_vec(reader, decode_blinded_reveal)?,
        signature: reader.hex()?,
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

fn metrics_from_snapshot(snapshot: &ChainSnapshot) -> Result<Vec<BlockMetricRow>> {
    let ledger = Ledger::from_persisted_snapshot(snapshot.clone())
        .context("failed to rebuild ledger for metrics")?;
    let revealed = revealed_blinded_transactions(snapshot)?.into_iter().fold(
        std::collections::BTreeMap::<u64, Vec<crate::domain::RevealedBlindedTransaction>>::new(),
        |mut by_height, revealed| {
            by_height.entry(revealed.height).or_default().push(revealed);
            by_height
        },
    );
    let mut circulating_supply =
        snapshot
            .genesis_allocations
            .values()
            .try_fold(0_u64, |total, amount| {
                total
                    .checked_add(*amount)
                    .context("genesis allocation total overflows")
            })?;
    let mut total_burned_amount = 0_u64;
    let mut rows = Vec::with_capacity(snapshot.blocks.len());
    let mut previous_timestamp_ms = None;

    for block in &snapshot.blocks {
        let revealed_transactions = revealed.get(&block.height).cloned().unwrap_or_default();
        let mut transfer_count = 0_u64;
        let mut burn_count = 0_u64;
        let mut mine_count = 0_u64;
        let mut burned_amount = 0_u64;
        let mut burned_fee_amount = 0_u64;
        let mut mine_issued_amount = 0_u64;
        let mut fees_amount = 0_u64;

        for transaction in &block.transactions {
            fees_amount = fees_amount
                .checked_add(transaction.fee())
                .context("block metric fees overflow")?;
            match transaction {
                Transaction::Transfer { .. } => transfer_count += 1,
                Transaction::Burn { amount, .. } => {
                    burn_count += 1;
                    burned_amount = burned_amount
                        .checked_add(*amount)
                        .context("block metric burns overflow")?;
                }
                Transaction::Mine { .. } => {
                    mine_count += 1;
                    mine_issued_amount = mine_issued_amount
                        .checked_add(MINE_REWARD)
                        .and_then(|amount| amount.checked_add(transaction.fee()))
                        .context("block metric mine issuance overflow")?;
                }
            }
        }
        for revealed in &revealed_transactions {
            let transaction = &revealed.transaction;
            fees_amount = fees_amount
                .checked_add(transaction.fee())
                .context("block metric fees overflow")?;
            let committer_fee = transaction.fee() / 2;
            let executor_full_fee = transaction.fee() - committer_fee;
            let executor_fee = executor_full_fee
                .saturating_mul(block.included_reveal_bundle_count() as u64)
                / crate::domain::REVEAL_COMMITTEE_SIZE as u64;
            burned_fee_amount = burned_fee_amount
                .checked_add(executor_full_fee.saturating_sub(executor_fee))
                .context("block metric burned fees overflow")?;
            match transaction {
                Transaction::Transfer { .. } => transfer_count += 1,
                Transaction::Burn { amount, .. } => {
                    burn_count += 1;
                    burned_amount = burned_amount
                        .checked_add(*amount)
                        .context("block metric burns overflow")?;
                }
                Transaction::Mine { .. } => {
                    mine_count += 1;
                    mine_issued_amount = mine_issued_amount
                        .checked_add(MINE_REWARD)
                        .and_then(|amount| amount.checked_add(transaction.fee()))
                        .context("block metric mine issuance overflow")?;
                }
            }
        }
        total_burned_amount = total_burned_amount
            .checked_add(burned_amount)
            .and_then(|amount| amount.checked_add(burned_fee_amount))
            .context("total burned metric overflows")?;

        let issued_amount = block_issued_amount(block.height, block.reward, mine_issued_amount)
            .with_context(|| {
                format!("block metric issuance overflows at block {}", block.height)
            })?;
        circulating_supply = circulating_supply
            .checked_add(issued_amount)
            .with_context(|| {
                format!(
                    "circulating supply metric overflows while adding issuance at block {}",
                    block.height
                )
            })?;
        circulating_supply = circulating_supply
            .checked_sub(burned_amount)
            .and_then(|amount| amount.checked_sub(burned_fee_amount))
            .with_context(|| {
                format!(
                    "circulating supply metric underflows while subtracting burns at block {}",
                    block.height
                )
            })?;
        let block_time_ms =
            previous_timestamp_ms.map(|previous| block.timestamp_ms.saturating_sub(previous));
        previous_timestamp_ms = Some(block.timestamp_ms);
        rows.push(BlockMetricRow {
            height: block.height,
            block_hash: block.hash.clone(),
            timestamp_ms: block.timestamp_ms,
            block_time_ms,
            mine_difficulty_bits: ledger.mine_difficulty_bits_at_height(block.height),
            circulating_supply,
            transaction_count: (block.transactions.len() + revealed_transactions.len()) as u64,
            transfer_count,
            burn_count,
            mine_count,
            burned_amount,
            total_burned_amount,
            fees_amount,
            reward_amount: block.reward,
            vdf_rounds: block.vdf_rounds,
            finalizer_rank: block.finalizer_rank,
        });
    }
    Ok(rows)
}

fn block_issued_amount(
    height: u64,
    reward_amount: Amount,
    mine_issued_amount: Amount,
) -> Option<Amount> {
    let genesis_reward = if height == 0 { reward_amount } else { 0 };
    mine_issued_amount.checked_add(genesis_reward)
}

fn snapshot_tip(snapshot: &ChainSnapshot) -> Option<(u64, String)> {
    snapshot
        .blocks
        .last()
        .map(|block| (block.height, block.hash.clone()))
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use tempfile::tempdir;

    use crate::domain::{BLOCK_REWARD, GenesisBurn, Ledger, Wallet, run_vdf};

    use super::{
        BlockMetricRow, SqliteChainStore, decode_compact_snapshot, encode_compact_snapshot,
        replace_metrics,
    };

    #[test]
    fn sqlite_chain_store_roundtrips_snapshot() {
        let dir = tempdir().unwrap();
        let store = SqliteChainStore::open(dir.path().join("nested/chain.sqlite3")).unwrap();
        let wallet = Wallet::from_seed("alice");
        let mut genesis = BTreeMap::new();
        genesis.insert(wallet.address().to_string(), 1);
        let ledger =
            Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(wallet.address(), 1)], 1)
                .unwrap();

        store.save(&ledger.snapshot()).unwrap();

        assert_eq!(store.load().unwrap(), Some(ledger.snapshot()));
        store
            .with_connection(|connection| {
                let columns = connection
                    .prepare("PRAGMA table_info(chain_snapshots)")?
                    .query_map([], |row| row.get::<_, String>(1))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                assert!(columns.contains(&"snapshot_blob".to_string()));
                assert!(!columns.contains(&"snapshot_json".to_string()));
                Ok(())
            })
            .unwrap();
    }

    #[test]
    fn compact_snapshot_roundtrips_and_is_smaller_than_json() {
        let alice = Wallet::from_seed("compact-alice");
        let bob = Wallet::from_seed("compact-bob");
        let carol = Wallet::from_seed("compact-carol");
        let wallets = [alice.clone(), bob.clone()];
        let mut genesis = BTreeMap::new();
        genesis.insert(alice.address().to_string(), 10_000_000);
        genesis.insert(bob.address().to_string(), 10_000_000);
        genesis.insert(carol.address().to_string(), 10_000_000);
        let mut ledger = Ledger::new_with_genesis_burns(
            genesis,
            vec![
                GenesisBurn::new(alice.address(), 1_000_000),
                GenesisBurn::new(bob.address(), 1_000_000),
            ],
            1,
        )
        .unwrap();
        let blinded = ledger
            .build_blinded_burn(&carol, 3, 7, ledger.height() + 4)
            .unwrap();
        ledger
            .submit_blinded_transaction(blinded.transaction.clone())
            .unwrap();
        let leader = ledger.expected_leader_for_next_block().unwrap();
        let wallet = wallets
            .iter()
            .find(|wallet| wallet.address() == leader)
            .unwrap();
        let burn = ledger.build_burn(wallet, 1, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let block = ledger.mine_next_block(wallet, 1).unwrap();
        assert_eq!(
            block.blinded_transactions,
            vec![blinded.transaction.clone()]
        );
        ledger.apply_locally_mined_block(block).unwrap();
        ledger.submit_blinded_reveal(blinded.reveal).unwrap();
        let leader = ledger.expected_leader_for_next_block().unwrap();
        let wallet = wallets
            .iter()
            .find(|wallet| wallet.address() == leader)
            .unwrap();
        let burn = ledger.build_burn(wallet, 1, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let bundles = ledger
            .reveal_committee_for_next_block()
            .into_iter()
            .filter_map(|member| {
                let wallet = wallets
                    .iter()
                    .find(|wallet| wallet.address() == member.owner)
                    .unwrap();
                ledger.build_reveal_bundle(wallet).unwrap()
            })
            .collect::<Vec<_>>();
        assert!(!bundles.is_empty());
        let prepared = ledger
            .prepare_next_block_with_reveal_bundles(wallet.address(), 2, bundles)
            .unwrap();
        let vdf_output = run_vdf(prepared.vdf_seed(), prepared.vdf_rounds());
        let block = prepared.finish(wallet, vdf_output);
        assert_eq!(block.all_blinded_reveals().len(), 1);
        ledger.apply_locally_mined_block(block).unwrap();

        let snapshot = ledger.snapshot();
        let compact = encode_compact_snapshot(&snapshot).unwrap();
        let json = serde_json::to_vec(&snapshot).unwrap();

        assert_eq!(decode_compact_snapshot(&compact).unwrap(), snapshot);
        assert!(
            compact.len() < json.len(),
            "compact snapshot should be smaller than JSON: compact={} JSON={}",
            compact.len(),
            json.len()
        );
    }

    #[test]
    fn sqlite_chain_store_overwrites_latest_snapshot() {
        let dir = tempdir().unwrap();
        let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
        let wallet = Wallet::from_seed("alice");
        let mut genesis = BTreeMap::new();
        genesis.insert(wallet.address().to_string(), 2);
        let mut ledger =
            Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(wallet.address(), 1)], 1)
                .unwrap();
        store.save(&ledger.snapshot()).unwrap();

        let burn = ledger.build_burn(&wallet, 1, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let block = ledger.mine_next_block(&wallet, 1_000).unwrap();
        ledger.apply_locally_mined_block(block).unwrap();
        store.save(&ledger.snapshot()).unwrap();

        let restored = store.load().unwrap().unwrap();
        assert_eq!(restored.blocks.last().unwrap().height, 1);
        assert_eq!(restored, ledger.snapshot());
    }

    #[test]
    fn sqlite_chain_store_saves_and_clears_block_metrics() {
        let dir = tempdir().unwrap();
        let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
        let wallet = Wallet::from_seed("metrics-alice");
        let mut genesis = BTreeMap::new();
        genesis.insert(wallet.address().to_string(), 10);
        let mut ledger =
            Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(wallet.address(), 1)], 1)
                .unwrap();
        let burn = ledger.build_burn(&wallet, 2, 1).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let block = ledger.mine_next_block(&wallet, 1_000).unwrap();
        ledger.apply_locally_mined_block(block).unwrap();

        store.save_with_metrics(&ledger.snapshot(), true).unwrap();
        let metrics = store.load_metrics().unwrap();

        assert_eq!(metrics.last().unwrap().height, 1);
        assert_eq!(metrics.last().unwrap().burn_count, 1);
        assert_eq!(metrics.last().unwrap().burned_amount, 2);
        assert_eq!(metrics.last().unwrap().fees_amount, 1);
        assert_eq!(metrics.last().unwrap().circulating_supply, BLOCK_REWARD + 7);

        store.clear_metrics().unwrap();
        assert!(store.load_metrics().unwrap().is_empty());
    }

    #[test]
    fn sqlite_chain_store_metrics_include_revealed_blinded_burns() {
        let dir = tempdir().unwrap();
        let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
        let alice = Wallet::from_seed("metrics-blinded-alice");
        let bob = Wallet::from_seed("metrics-blinded-bob");
        let carol = Wallet::from_seed("metrics-blinded-carol");
        let wallets = [alice.clone(), bob.clone()];
        let mut genesis = BTreeMap::new();
        genesis.insert(alice.address().to_string(), 10_000_000);
        genesis.insert(bob.address().to_string(), 10_000_000);
        genesis.insert(carol.address().to_string(), 10_000_000);
        let mut ledger = Ledger::new_with_genesis_burns(
            genesis,
            vec![
                GenesisBurn::new(alice.address(), 1_000_000),
                GenesisBurn::new(bob.address(), 1_000_000),
            ],
            1,
        )
        .unwrap();
        let blinded = ledger
            .build_blinded_burn(&carol, 3, 7, ledger.height() + 4)
            .unwrap();
        ledger
            .submit_blinded_transaction(blinded.transaction)
            .unwrap();
        let leader = ledger.expected_leader_for_next_block().unwrap();
        let wallet = wallets
            .iter()
            .find(|wallet| wallet.address() == leader)
            .unwrap();
        let burn = ledger.build_burn(wallet, 1, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let block = ledger.mine_next_block(wallet, 1).unwrap();
        ledger.apply_locally_mined_block(block).unwrap();
        ledger.submit_blinded_reveal(blinded.reveal).unwrap();
        let leader = ledger.expected_leader_for_next_block().unwrap();
        let wallet = wallets
            .iter()
            .find(|wallet| wallet.address() == leader)
            .unwrap();
        let burn = ledger.build_burn(wallet, 1, 0).unwrap();
        ledger.submit_transaction(burn).unwrap();
        let bundles = ledger
            .reveal_committee_for_next_block()
            .into_iter()
            .filter_map(|member| {
                let wallet = wallets
                    .iter()
                    .find(|wallet| wallet.address() == member.owner)
                    .unwrap();
                ledger.build_reveal_bundle(wallet).unwrap()
            })
            .collect::<Vec<_>>();
        assert!(!bundles.is_empty());
        let prepared = ledger
            .prepare_next_block_with_reveal_bundles(wallet.address(), 2, bundles)
            .unwrap();
        let vdf_output = run_vdf(prepared.vdf_seed(), prepared.vdf_rounds());
        let block = prepared.finish(wallet, vdf_output);
        assert_eq!(block.all_blinded_reveals().len(), 1);
        ledger.apply_locally_mined_block(block).unwrap();

        store.save_with_metrics(&ledger.snapshot(), true).unwrap();
        let metrics = store.load_metrics().unwrap();
        let last = metrics.last().unwrap();
        let supply_from_balances = ledger.status().balances.values().copied().sum::<u64>();

        assert_eq!(last.burn_count, 2);
        assert_eq!(last.burned_amount, 4);
        assert_eq!(last.fees_amount, 7);
        assert_eq!(last.circulating_supply, supply_from_balances);
    }

    #[test]
    fn sqlite_chain_store_roundtrips_vdf_round_metrics_above_legacy_u32_limit() {
        let dir = tempdir().unwrap();
        let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
        let vdf_rounds = u64::from(u32::MAX) + 42;

        store
            .with_connection_mut(|connection| {
                let transaction = connection.transaction().unwrap();
                replace_metrics(
                    &transaction,
                    &[BlockMetricRow {
                        height: 1,
                        block_hash: "hash".to_string(),
                        timestamp_ms: 1_000,
                        block_time_ms: Some(415_000),
                        mine_difficulty_bits: 12,
                        circulating_supply: 100,
                        transaction_count: 0,
                        transfer_count: 0,
                        burn_count: 0,
                        mine_count: 0,
                        burned_amount: 0,
                        total_burned_amount: 0,
                        fees_amount: 0,
                        reward_amount: 0,
                        vdf_rounds,
                        finalizer_rank: 0,
                    }],
                )?;
                transaction.commit().unwrap();
                Ok(())
            })
            .unwrap();

        let metrics = store.load_metrics().unwrap();
        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].vdf_rounds, vdf_rounds);
    }

    #[test]
    fn sqlite_chain_store_metrics_include_genesis_reward_when_burn_consumes_allocation() {
        let dir = tempdir().unwrap();
        let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
        let wallet = Wallet::from_seed("metrics-genesis-reward");
        let mut genesis = BTreeMap::new();
        genesis.insert(wallet.address().to_string(), 1);
        let ledger =
            Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(wallet.address(), 1)], 1)
                .unwrap();

        store.save_with_metrics(&ledger.snapshot(), true).unwrap();
        let metrics = store.load_metrics().unwrap();

        assert_eq!(metrics.len(), 1);
        assert_eq!(metrics[0].height, 0);
        assert_eq!(metrics[0].burned_amount, 1);
        assert_eq!(metrics[0].reward_amount, BLOCK_REWARD);
        assert_eq!(metrics[0].circulating_supply, BLOCK_REWARD);
    }

    #[test]
    fn sqlite_chain_store_disabled_metrics_save_deletes_old_metrics() {
        let dir = tempdir().unwrap();
        let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
        let wallet = Wallet::from_seed("metrics-cleanup");
        let mut genesis = BTreeMap::new();
        genesis.insert(wallet.address().to_string(), 10);
        let ledger =
            Ledger::new_with_genesis_burns(genesis, vec![GenesisBurn::new(wallet.address(), 1)], 1)
                .unwrap();

        store.save_with_metrics(&ledger.snapshot(), true).unwrap();
        assert!(!store.load_metrics().unwrap().is_empty());

        store.save_with_metrics(&ledger.snapshot(), false).unwrap();
        assert!(store.load_metrics().unwrap().is_empty());
    }

    #[test]
    fn sqlite_chain_store_reports_invalid_compact_snapshot() {
        let dir = tempdir().unwrap();
        let store = SqliteChainStore::open(dir.path().join("chain.sqlite3")).unwrap();
        store
            .with_connection(|connection| {
                connection.execute(
                    r#"
INSERT INTO chain_snapshots (id, height, tip_hash, snapshot_blob, updated_at_ms)
VALUES (1, 9, 'bad-tip', x'00010203', 0)
"#,
                    [],
                )?;
                Ok(())
            })
            .unwrap();

        let error = store.load().unwrap_err();

        assert!(
            format!("{error:#}").contains("failed to parse compact chain snapshot from database"),
            "{error:#}"
        );
    }
}
