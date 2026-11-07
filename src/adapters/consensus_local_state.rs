//! Small JSON files in the data directory for node-local state that is not consensus data:
//! the pending mempool and the operator's Heartbeat mining preference.

use std::{
    fs, io,
    path::{Path, PathBuf},
    sync::Mutex,
};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

use crate::consensus::{TicketTransaction, Transaction};

/// A mempool snapshot is rebuilt from at most this many bytes; anything larger is ignored.
const MAX_MEMPOOL_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_PREFERENCE_FILE_BYTES: u64 = 4 * 1024;
const INSTANCE_ID_BYTES: usize = 16;
static MINING_WRITE_LOCK: Mutex<()> = Mutex::new(());

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct MempoolSnapshot {
    pub transactions: Vec<Transaction>,
    pub tickets: Vec<TicketTransaction>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MiningPreference {
    pub enabled: bool,
    pub workers: usize,
    #[serde(default)]
    pub ledger_enabled: bool,
    #[serde(default)]
    pub last_ledger_ticket_height: Option<u64>,
}

pub fn mempool_path(data_dir: &Path) -> PathBuf {
    data_dir.join("mempool.json")
}

/// Lives next to `wallet.json`, which is how the HTTP adapter knows the data directory.
pub fn mining_path(wallet_path: &Path) -> PathBuf {
    wallet_path.with_file_name("mining.json")
}

/// Returns a stable, non-secret identifier for this data directory.
///
/// The browser uses it to remember Initial Setup per node instead of per URL. Replacing a node's
/// data directory therefore cannot accidentally inherit another node's completed setup flag.
pub fn load_or_create_instance_id(data_dir: &Path) -> Result<String> {
    let path = data_dir.join("instance-id");
    match fs::read_to_string(&path) {
        Ok(value) => {
            let value = value.trim();
            if value.len() == INSTANCE_ID_BYTES * 2
                && value.bytes().all(|byte| byte.is_ascii_hexdigit())
            {
                return Ok(value.to_ascii_lowercase());
            }
            anyhow::bail!(
                "{} does not contain a valid node instance ID",
                path.display()
            );
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    }

    let mut bytes = [0_u8; INSTANCE_ID_BYTES];
    getrandom::getrandom(&mut bytes)
        .map_err(|error| anyhow::anyhow!("generating node instance ID failed: {error}"))?;
    let value = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let temporary = path.with_extension("tmp");
    fs::write(&temporary, &value).with_context(|| format!("writing {}", temporary.display()))?;
    fs::rename(&temporary, &path).with_context(|| format!("replacing {}", path.display()))?;
    Ok(value)
}

pub fn load_mempool(path: &Path) -> Result<MempoolSnapshot> {
    Ok(read_json(path, MAX_MEMPOOL_FILE_BYTES)?.unwrap_or_default())
}

pub fn save_mempool(path: &Path, snapshot: &MempoolSnapshot) -> Result<()> {
    write_json(path, snapshot)
}

pub fn load_mining(path: &Path) -> Result<Option<MiningPreference>> {
    read_json(path, MAX_PREFERENCE_FILE_BYTES)
}

pub fn save_mining(path: &Path, preference: MiningPreference) -> Result<()> {
    save_mining_from(path, || preference)
}

/// Serializes preference snapshots and builds them while holding the write lock, so concurrent
/// miner and operator updates cannot overwrite a newer toggle with stale atomic state.
pub fn save_mining_from(path: &Path, preference: impl FnOnce() -> MiningPreference) -> Result<()> {
    let _guard = MINING_WRITE_LOCK
        .lock()
        .map_err(|_| anyhow::anyhow!("mining preference lock poisoned"))?;
    write_json(path, &preference())
}

fn read_json<T: DeserializeOwned>(path: &Path, limit: u64) -> Result<Option<T>> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.len() > limit => {
            anyhow::bail!("{} exceeds {limit} bytes", path.display())
        }
        Ok(_) => {
            let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
            serde_json::from_slice(&bytes)
                .map(Some)
                .with_context(|| format!("decoding {}", path.display()))
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let temporary = path.with_extension("json.tmp");
    fs::write(&temporary, serde_json::to_vec(value)?)
        .with_context(|| format!("writing {}", temporary.display()))?;
    fs::rename(&temporary, path).with_context(|| format!("replacing {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_files_mean_empty_state_and_values_roundtrip() {
        let directory = tempfile::tempdir().unwrap();
        let mempool = mempool_path(directory.path());
        assert!(load_mempool(&mempool).unwrap().transactions.is_empty());
        save_mempool(&mempool, &MempoolSnapshot::default()).unwrap();
        assert!(load_mempool(&mempool).unwrap().tickets.is_empty());

        let mining = mining_path(&directory.path().join("wallet.json"));
        assert_eq!(load_mining(&mining).unwrap(), None);
        let preference = MiningPreference {
            enabled: true,
            workers: 3,
            ledger_enabled: true,
            last_ledger_ticket_height: Some(40),
        };
        save_mining(&mining, preference).unwrap();
        assert_eq!(load_mining(&mining).unwrap(), Some(preference));

        let instance_id = load_or_create_instance_id(directory.path()).unwrap();
        assert_eq!(instance_id.len(), INSTANCE_ID_BYTES * 2);
        assert_eq!(
            load_or_create_instance_id(directory.path()).unwrap(),
            instance_id
        );

        fs::write(&mining, "not json").unwrap();
        assert!(load_mining(&mining).is_err());
    }
}
