use std::{
    env, fs,
    io::Read,
    path::{Path, PathBuf},
    process,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use iuna::{
    adapters::chain_store::SqliteChainStore,
    domain::{Block, FinalizerMode},
};
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Serialize)]
struct TicketResume {
    height: u64,
    hash: String,
    rank: u32,
    delay_blocks: u64,
    delay_ms: u64,
}

#[derive(Serialize)]
struct RecoveryEvidence {
    height: u64,
    hash: String,
    miner: String,
    timestamp_ms: u64,
    gap_from_parent_ms: u64,
    next_ticket: Option<TicketResume>,
    immediate_ticket_resume: bool,
}

#[derive(Serialize)]
struct AuditReport {
    format: u8,
    source: PathBuf,
    captured_at_ms: u64,
    database_copy_sha256: String,
    verified_under_current_consensus_ruleset: bool,
    profile_id: String,
    height: u64,
    tip_hash: String,
    genesis_hash: String,
    recovery_count: usize,
    recoveries: Vec<RecoveryEvidence>,
}

fn main() -> Result<()> {
    let mut args = env::args_os().skip(1);
    let source = PathBuf::from(
        args.next()
            .context("usage: iuna-chain-audit CHAIN_DB [--output REPORT.json]")?,
    );
    let mut output = None;
    while let Some(argument) = args.next() {
        if argument == "--output" {
            output = Some(PathBuf::from(args.next().context("missing --output path")?));
        } else {
            bail!("unknown argument: {}", argument.to_string_lossy());
        }
    }

    let report = audit_snapshot(&source)?;
    let json = serde_json::to_string_pretty(&report)? + "\n";
    if let Some(output) = output {
        fs::write(&output, json)
            .with_context(|| format!("failed to write audit report {}", output.display()))?;
        println!(
            "wrote {} recovery events to {}",
            report.recovery_count,
            output.display()
        );
    } else {
        print!("{json}");
    }
    Ok(())
}

fn audit_snapshot(source: &Path) -> Result<AuditReport> {
    if !source.is_file() {
        bail!("chain database does not exist: {}", source.display());
    }
    let companion = ["-wal", "-journal"]
        .into_iter()
        .map(|suffix| path_with_suffix(source, suffix))
        .find(|path| path.exists());
    if let Some(companion) = companion {
        bail!(
            "refusing a database with an active SQLite companion; stop the node or create a SQLite backup first: {}",
            companion.display()
        );
    }

    let before = fs::metadata(source).context("failed to inspect source database")?;
    let temporary = env::temp_dir().join(format!(
        "iuna-chain-audit-{}-{}.sqlite3",
        process::id(),
        now_ms()?
    ));
    fs::copy(source, &temporary).with_context(|| {
        format!(
            "failed to copy source database {} to {}",
            source.display(),
            temporary.display()
        )
    })?;
    let after = fs::metadata(source).context("failed to re-inspect source database")?;
    if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
        cleanup_temporary_database(&temporary);
        bail!("source database changed while it was copied; retry on a stable snapshot");
    }

    let result = audit_copy(source, &temporary);
    cleanup_temporary_database(&temporary);
    result
}

fn audit_copy(source: &Path, copy: &Path) -> Result<AuditReport> {
    let database_copy_sha256 = file_sha256(copy)?;
    let loaded = SqliteChainStore::open(copy)?
        .load_with_verification_status()?
        .context("chain database contains no snapshot")?;
    let verified_under_current_consensus_ruleset = loaded.revalidation_from_height.is_none();
    let snapshot = loaded.snapshot;
    let tip = snapshot.blocks.last().context("chain snapshot is empty")?;
    let genesis = snapshot.blocks.first().context("chain snapshot is empty")?;
    let recoveries = snapshot
        .blocks
        .iter()
        .enumerate()
        .filter(|(_, block)| block.finalizer_mode == FinalizerMode::Recovery)
        .map(|(index, block)| recovery_evidence(&snapshot.blocks, index, block))
        .collect::<Vec<_>>();

    Ok(AuditReport {
        format: 1,
        source: source.to_path_buf(),
        captured_at_ms: now_ms()?,
        database_copy_sha256,
        verified_under_current_consensus_ruleset,
        profile_id: snapshot.launch_profile.profile_id.clone(),
        height: tip.height,
        tip_hash: tip.hash.clone(),
        genesis_hash: genesis.hash.clone(),
        recovery_count: recoveries.len(),
        recoveries,
    })
}

fn recovery_evidence(blocks: &[Block], index: usize, recovery: &Block) -> RecoveryEvidence {
    let parent_timestamp = index
        .checked_sub(1)
        .and_then(|parent| blocks.get(parent))
        .map(|block| block.timestamp_ms)
        .unwrap_or(recovery.timestamp_ms);
    let next_ticket_block = blocks[index.saturating_add(1)..]
        .iter()
        .find(|block| block.finalizer_mode == FinalizerMode::Ticket);
    let next_ticket = next_ticket_block.map(|block| TicketResume {
        height: block.height,
        hash: block.hash.clone(),
        rank: block.finalizer_rank,
        delay_blocks: block.height.saturating_sub(recovery.height),
        delay_ms: block.timestamp_ms.saturating_sub(recovery.timestamp_ms),
    });
    let immediate_ticket_resume = blocks.get(index.saturating_add(1)).is_some_and(|block| {
        block.finalizer_mode == FinalizerMode::Ticket && block.prev_hash == recovery.hash
    });

    RecoveryEvidence {
        height: recovery.height,
        hash: recovery.hash.clone(),
        miner: recovery.miner.clone(),
        timestamp_ms: recovery.timestamp_ms,
        gap_from_parent_ms: recovery.timestamp_ms.saturating_sub(parent_timestamp),
        next_ticket,
        immediate_ticket_resume,
    }
}

fn cleanup_temporary_database(path: &Path) {
    let _ = fs::remove_file(path);
    for suffix in ["-wal", "-shm"] {
        let companion = path_with_suffix(path, suffix);
        let _ = fs::remove_file(companion);
    }
}

fn path_with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

fn file_sha256(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn now_ms() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_millis()
        .try_into()
        .context("timestamp does not fit in u64")
}
