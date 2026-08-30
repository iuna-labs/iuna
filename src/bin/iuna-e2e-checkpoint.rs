use std::{env, path::PathBuf};

use anyhow::{Context, Result, bail};
use iuna::adapters::{chain_store::SqliteChainStore, ui_data_store::SqliteUiDataStore};

fn main() -> Result<()> {
    let mut args = env::args_os().skip(1);
    let source = PathBuf::from(args.next().context("missing source chain database")?);
    let chain_destination =
        PathBuf::from(args.next().context("missing destination chain database")?);
    let ui_destination = PathBuf::from(args.next().context("missing destination UI database")?);
    let height = args
        .next()
        .context("missing checkpoint height")?
        .into_string()
        .map_err(|_| anyhow::anyhow!("checkpoint height is not valid UTF-8"))?
        .parse::<u64>()
        .context("checkpoint height is not an unsigned integer")?;
    if args.next().is_some() {
        bail!("usage: iuna-e2e-checkpoint SOURCE_DB CHAIN_DB UI_DB HEIGHT");
    }
    for destination in [&chain_destination, &ui_destination] {
        if destination.exists() {
            bail!("destination already exists: {}", destination.display());
        }
    }

    let source_store = SqliteChainStore::open(&source)?;
    let mut snapshot = source_store
        .load()?
        .with_context(|| format!("source has no chain snapshot: {}", source.display()))?;
    let source_height = snapshot
        .blocks
        .last()
        .context("source chain snapshot is empty")?
        .height;
    if source_height < height {
        bail!("source height {source_height} is below checkpoint height {height}");
    }

    snapshot.blocks.truncate(
        usize::try_from(height)
            .context("checkpoint height does not fit in memory")?
            .checked_add(1)
            .context("checkpoint height overflow")?,
    );
    SqliteChainStore::open(&chain_destination)?.save(&snapshot)?;
    SqliteUiDataStore::open(&ui_destination)?.project_snapshot(&snapshot, false)?;

    let tip = snapshot.blocks.last().context("checkpoint is empty")?;
    println!(
        "materialized height {} ({}) from source height {}",
        tip.height, tip.hash, source_height
    );
    Ok(())
}
