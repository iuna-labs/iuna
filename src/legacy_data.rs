//! One-time, non-destructive handling of data from the old node.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension};

use iuna::consensus::{GENESIS_HASH, NETWORK_ID};

const LEGACY_CHAIN_TABLES: &[&str] = &[
    "chain_snapshots",
    "chain_metadata",
    "chain_segments",
    "chain_block_index",
    "chain_verification",
    "pending_transactions_v2",
];

/// Archives recognized old files before the new stores are opened.
///
/// A backup is never overwritten. Repeated migrations use `.backup.1`, `.backup.2`, and so on.
pub(crate) fn archive(data_dir: &Path) -> Result<Vec<(PathBuf, PathBuf)>> {
    let chain_path = data_dir.join("chain.sqlite3");
    let legacy_chain = is_legacy_chain(&chain_path)?;
    let wallet_path = data_dir.join("wallet.json");
    let legacy_wallet = is_legacy_wallet(&wallet_path)?;

    let mut databases = Vec::new();
    let mut files = Vec::new();
    if legacy_chain {
        databases.push(chain_path);
    }
    if legacy_wallet {
        files.extend([wallet_path, data_dir.join("wallet.password")]);
    }
    // These filenames are exclusive to the old node. The new UI projection is `ui.sqlite3`,
    // while settings, peers, mining and mempool each have new dedicated representations.
    databases.push(data_dir.join("ui_data.sqlite3"));
    files.push(data_dir.join("config.json"));

    let mut moved = Vec::new();
    for database in databases {
        archive_database(&database, &mut moved)?;
    }
    for source in files {
        if source.exists() {
            let backup = available_backup_path(&source);
            move_file(&source, &backup, &mut moved)?;
        }
    }
    Ok(moved)
}

/// A WAL file can hold committed transactions, and SQLite only finds it next to its database as
/// `<database>-wal`. The companions therefore follow the database's backup name, and they move
/// first: a WAL left behind would be replayed into the new database created at the old path.
fn archive_database(database: &Path, moved: &mut Vec<(PathBuf, PathBuf)>) -> Result<()> {
    let backup = available_backup_path(database);
    for suffix in ["-wal", "-shm"] {
        let companion = with_suffix(database, suffix);
        if !companion.exists() {
            continue;
        }
        let target = if database.exists() {
            with_suffix(&backup, suffix)
        } else {
            available_backup_path(&companion)
        };
        move_file(&companion, &target, moved)?;
    }
    if database.exists() {
        move_file(database, &backup, moved)?;
    }
    Ok(())
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

fn move_file(source: &Path, backup: &Path, moved: &mut Vec<(PathBuf, PathBuf)>) -> Result<()> {
    fs::rename(source, backup).with_context(|| {
        format!(
            "archiving legacy data {} as {}",
            source.display(),
            backup.display()
        )
    })?;
    moved.push((source.to_path_buf(), backup.to_path_buf()));
    Ok(())
}

/// Whether the chain database belongs to the old protocol or to another network or genesis. Either
/// way the store would refuse to load it, so it is set aside instead of stopping the node.
fn is_legacy_chain(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .with_context(|| format!("inspecting existing chain database {}", path.display()))?;
    // A database may contain old, unused tables after an earlier startup. Once metadata exists
    // it decides, and only a chain of this network and genesis is active data.
    if table_exists(&connection, "consensus_chain_metadata")? {
        let identity = connection
            .query_row(
                "SELECT network_id, genesis_hash FROM consensus_chain_metadata WHERE id = 1",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Vec<u8>>(1)?)),
            )
            .optional()
            .context("checking existing chain metadata")?;
        if let Some((network, genesis)) = identity {
            return Ok(network != NETWORK_ID || genesis.as_slice() != GENESIS_HASH);
        }
    }
    LEGACY_CHAIN_TABLES
        .iter()
        .map(|table| table_exists(&connection, table))
        .collect::<Result<Vec<_>>>()
        .map(|exists| exists.into_iter().any(|exists| exists))
}

fn table_exists(connection: &Connection, name: &str) -> Result<bool> {
    connection
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
            [name],
            |_| Ok(()),
        )
        .optional()
        .map(|row| row.is_some())
        .context("inspecting chain database schema")
}

fn is_legacy_wallet(path: &Path) -> Result<bool> {
    if !path.exists() {
        return Ok(false);
    }
    let bytes = fs::read(path)
        .with_context(|| format!("reading existing wallet file {}", path.display()))?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .with_context(|| format!("inspecting existing wallet file {}", path.display()))?;
    Ok(value.get("address").is_some() && value.get("public_key").is_none())
}

fn available_backup_path(source: &Path) -> PathBuf {
    let file_name = source
        .file_name()
        .expect("legacy data paths always have a filename")
        .to_string_lossy();
    for suffix in 0_u64.. {
        let extension = if suffix == 0 {
            ".backup".to_owned()
        } else {
            format!(".backup.{suffix}")
        };
        let candidate = source.with_file_name(format!("{file_name}{extension}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!("u64 backup suffix space exhausted")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archives_legacy_node_data_without_overwriting_backups() {
        let directory = tempfile::tempdir().unwrap();
        let chain = directory.path().join("chain.sqlite3");
        Connection::open(&chain)
            .unwrap()
            .execute_batch("CREATE TABLE chain_metadata (id INTEGER PRIMARY KEY);")
            .unwrap();
        fs::write(
            directory.path().join("wallet.json"),
            r#"{"version":3,"address":"old","encryption":{}}"#,
        )
        .unwrap();
        fs::write(directory.path().join("config.json"), "{}").unwrap();
        fs::write(directory.path().join("chain.sqlite3.backup"), "keep").unwrap();

        let moved = archive(directory.path()).unwrap();

        assert_eq!(moved.len(), 3);
        assert!(!chain.exists());
        assert!(directory.path().join("chain.sqlite3.backup.1").exists());
        assert_eq!(
            fs::read_to_string(directory.path().join("chain.sqlite3.backup")).unwrap(),
            "keep"
        );
        assert!(directory.path().join("wallet.json.backup").exists());
        assert!(directory.path().join("config.json.backup").exists());
    }

    #[test]
    fn a_legacy_wal_stays_readable_with_its_archived_database() {
        let directory = tempfile::tempdir().unwrap();
        let chain = directory.path().join("chain.sqlite3");
        fs::write(directory.path().join("chain.sqlite3.backup"), "keep").unwrap();
        let writer = Connection::open(&chain).unwrap();
        writer
            .execute_batch(
                "PRAGMA journal_mode = WAL; PRAGMA wal_autocheckpoint = 0;
                 CREATE TABLE chain_metadata (id INTEGER PRIMARY KEY);
                 INSERT INTO chain_metadata (id) VALUES (7);",
            )
            .unwrap();
        // The old node crashed: its last commit lives only in the WAL.
        assert!(directory.path().join("chain.sqlite3-wal").exists());

        let moved = archive(directory.path()).unwrap();
        std::mem::forget(writer);

        let backup = directory.path().join("chain.sqlite3.backup.1");
        assert!(moved.iter().any(|(_, archived)| *archived == backup));
        assert!(directory.path().join("chain.sqlite3.backup.1-wal").exists());
        assert!(!directory.path().join("chain.sqlite3-wal").exists());
        let id: i64 = Connection::open(&backup)
            .unwrap()
            .query_row("SELECT id FROM chain_metadata", [], |row| row.get(0))
            .unwrap();
        assert_eq!(id, 7);
    }

    fn current_chain(directory: &Path) -> PathBuf {
        let chain = directory.join("chain.sqlite3");
        let store = iuna::adapters::consensus_store::ConsensusChainStore::open(&chain).unwrap();
        store
            .save(&iuna::consensus::Chain::from_genesis().unwrap())
            .unwrap();
        chain
    }

    #[test]
    fn sets_aside_a_chain_of_another_network_but_keeps_the_wallet() {
        let directory = tempfile::tempdir().unwrap();
        let chain = current_chain(directory.path());
        Connection::open(&chain)
            .unwrap()
            .execute(
                "UPDATE consensus_chain_metadata SET network_id = 'iuna-devnet'",
                [],
            )
            .unwrap();
        fs::write(
            directory.path().join("wallet.json"),
            r#"{"version":1,"public_key":"current"}"#,
        )
        .unwrap();

        let moved = archive(directory.path()).unwrap();

        assert_eq!(moved.len(), 1);
        assert!(!chain.exists());
        assert!(directory.path().join("chain.sqlite3.backup").exists());
        assert!(directory.path().join("wallet.json").exists());
    }

    #[test]
    fn leaves_current_data_untouched() {
        let directory = tempfile::tempdir().unwrap();
        let chain = current_chain(directory.path());
        Connection::open(&chain)
            .unwrap()
            .execute_batch("CREATE TABLE chain_metadata (id INTEGER PRIMARY KEY);")
            .unwrap();
        fs::write(
            directory.path().join("wallet.json"),
            r#"{"version":1,"public_key":"current"}"#,
        )
        .unwrap();

        assert!(archive(directory.path()).unwrap().is_empty());
        assert!(chain.exists());
        assert!(directory.path().join("wallet.json").exists());
    }

    #[test]
    fn ignores_unrecognized_files_instead_of_hiding_corruption() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(directory.path().join("chain.sqlite3"), "not sqlite").unwrap();
        fs::write(directory.path().join("wallet.json"), "not json").unwrap();

        assert!(archive(directory.path()).is_err());
        assert!(directory.path().join("chain.sqlite3").exists());
        assert!(directory.path().join("wallet.json").exists());
    }
}
