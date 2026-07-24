#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::Path,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::domain::{Amount, DEFAULT_MINE_FEE, DEFAULT_TRANSACTION_FEE, MICRO_LUUN};

const CONFIG_FILE_VERSION: u32 = 1;
const AMOUNT_UNIT_MICROLUUN: &str = "microluun";
pub const DEFAULT_BURN_FEE: Amount = DEFAULT_TRANSACTION_FEE;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiConfig {
    pub setup_complete: bool,
    #[serde(skip_serializing, default)]
    pub auth_password_hash: Option<String>,
    pub mining_enabled: bool,
    pub pow_mining_enabled: bool,
    pub burn_per_block: Amount,
    pub burn_fee: Amount,
    pub pow_mine_fee: Amount,
    pub peers: Vec<String>,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            setup_complete: false,
            auth_password_hash: None,
            mining_enabled: false,
            pow_mining_enabled: false,
            burn_per_block: 0,
            burn_fee: DEFAULT_BURN_FEE,
            pow_mine_fee: DEFAULT_MINE_FEE,
            peers: Vec::new(),
        }
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct ConfigFile {
    version: u32,
    #[serde(default)]
    amount_unit: Option<String>,
    setup_complete: bool,
    #[serde(default)]
    auth_password_hash: Option<String>,
    #[serde(default)]
    mining_enabled: Option<bool>,
    #[serde(default)]
    pow_mining_enabled: bool,
    #[serde(default)]
    burn_per_block: Amount,
    #[serde(default = "default_burn_fee")]
    burn_fee: Amount,
    #[serde(default)]
    pow_mine_fee: Option<Amount>,
    #[serde(default)]
    peers: Vec<String>,
}

fn default_burn_fee() -> Amount {
    1
}

pub fn load_or_create(path: &Path) -> Result<UiConfig> {
    if path.exists() {
        return load(path);
    }

    let config = UiConfig::default();
    save(path, &config)?;
    Ok(config)
}

pub fn save(path: &Path, config: &UiConfig) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create config directory {}", parent.display()))?;
    }

    let stored = ConfigFile {
        version: CONFIG_FILE_VERSION,
        amount_unit: Some(AMOUNT_UNIT_MICROLUUN.to_string()),
        setup_complete: config.setup_complete,
        auth_password_hash: config.auth_password_hash.clone(),
        mining_enabled: Some(config.mining_enabled),
        pow_mining_enabled: config.pow_mining_enabled,
        burn_per_block: config.burn_per_block,
        burn_fee: config.burn_fee,
        pow_mine_fee: Some(config.pow_mine_fee),
        peers: config.peers.clone(),
    };
    let bytes = serde_json::to_vec_pretty(&stored).context("failed to serialize config file")?;
    let mut file = create_config_file(path)?;
    file.write_all(&bytes)
        .with_context(|| format!("failed to write config file {}", path.display()))?;
    file.write_all(b"\n")
        .with_context(|| format!("failed to write config file {}", path.display()))?;
    Ok(())
}

fn load(path: &Path) -> Result<UiConfig> {
    let bytes =
        fs::read(path).with_context(|| format!("failed to read config file {}", path.display()))?;
    let stored: ConfigFile = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse config file {}", path.display()))?;

    if stored.version != CONFIG_FILE_VERSION {
        bail!(
            "unsupported config file version {} in {}",
            stored.version,
            path.display()
        );
    }

    let scale = if stored.amount_unit.as_deref() == Some(AMOUNT_UNIT_MICROLUUN) {
        1
    } else {
        MICRO_LUUN
    };

    Ok(UiConfig {
        setup_complete: stored.setup_complete,
        auth_password_hash: stored.auth_password_hash,
        mining_enabled: stored.mining_enabled.unwrap_or(stored.burn_per_block > 0),
        pow_mining_enabled: stored.pow_mining_enabled,
        burn_per_block: stored.burn_per_block.saturating_mul(scale),
        burn_fee: stored.burn_fee.saturating_mul(scale),
        pow_mine_fee: stored
            .pow_mine_fee
            .map(|fee| fee.saturating_mul(scale))
            .unwrap_or(DEFAULT_MINE_FEE),
        peers: stored.peers,
    })
}

fn create_config_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    options
        .open(path)
        .with_context(|| format!("failed to create config file {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use crate::domain::MICRO_LUUN;

    use super::{DEFAULT_BURN_FEE, DEFAULT_MINE_FEE, UiConfig, load_or_create, save};

    #[test]
    fn creates_default_config_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");

        let config = load_or_create(&path).unwrap();

        assert!(!config.setup_complete);
        let stored = fs::read_to_string(path).unwrap();
        assert!(stored.contains("\"version\": 1"));
        assert!(stored.contains("\"amount_unit\": \"microluun\""));
        assert!(stored.contains("\"setup_complete\": false"));
        assert!(stored.contains("\"auth_password_hash\": null"));
        assert!(stored.contains("\"mining_enabled\": false"));
        assert!(stored.contains("\"pow_mining_enabled\": false"));
        assert!(stored.contains("\"burn_per_block\": 0"));
        assert!(stored.contains("\"burn_fee\": 1000000"));
        assert!(stored.contains("\"pow_mine_fee\": 10000"));
        assert!(stored.contains("\"peers\": []"));
    }

    #[test]
    fn saves_and_loads_setup_completion() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");

        save(
            &path,
            &UiConfig {
                setup_complete: true,
                auth_password_hash: Some("auth-hash".to_string()),
                mining_enabled: true,
                pow_mining_enabled: true,
                burn_per_block: 50 * MICRO_LUUN,
                burn_fee: 3 * MICRO_LUUN,
                pow_mine_fee: 2 * MICRO_LUUN,
                peers: vec!["127.0.0.1:9444".to_string()],
            },
        )
        .unwrap();
        let config = load_or_create(&path).unwrap();

        assert!(config.setup_complete);
        assert_eq!(config.auth_password_hash.as_deref(), Some("auth-hash"));
        assert!(config.mining_enabled);
        assert!(config.pow_mining_enabled);
        assert_eq!(config.burn_per_block, 50 * MICRO_LUUN);
        assert_eq!(config.burn_fee, 3 * MICRO_LUUN);
        assert_eq!(config.pow_mine_fee, 2 * MICRO_LUUN);
        assert_eq!(config.peers, vec!["127.0.0.1:9444"]);
    }

    #[test]
    fn ui_config_json_does_not_expose_password_hash() {
        let json = serde_json::to_string(&UiConfig {
            auth_password_hash: Some("secret-password-hash".to_string()),
            ..UiConfig::default()
        })
        .unwrap();

        assert!(!json.contains("secret-password-hash"));
        assert!(!json.contains("auth_password_hash"));
    }

    #[test]
    fn loads_old_config_without_burn_rate_as_zero() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            r#"{
  "version": 1,
  "setup_complete": true,
  "peers": ["127.0.0.1:9444"]
}
"#,
        )
        .unwrap();

        let config = load_or_create(&path).unwrap();

        assert!(config.setup_complete);
        assert!(!config.mining_enabled);
        assert!(!config.pow_mining_enabled);
        assert_eq!(config.burn_per_block, 0);
        assert_eq!(config.burn_fee, DEFAULT_BURN_FEE);
        assert_eq!(config.pow_mine_fee, DEFAULT_MINE_FEE);
        assert_eq!(config.peers, vec!["127.0.0.1:9444"]);
    }

    #[test]
    fn loads_old_config_with_burn_rate_as_mining_enabled() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");
        fs::write(
            &path,
            r#"{
  "version": 1,
  "setup_complete": true,
  "burn_per_block": 2,
  "burn_fee": 1,
  "peers": []
}
"#,
        )
        .unwrap();

        let config = load_or_create(&path).unwrap();

        assert!(config.mining_enabled);
        assert_eq!(config.burn_per_block, 2 * MICRO_LUUN);
        assert_eq!(config.burn_fee, MICRO_LUUN);
        assert_eq!(config.pow_mine_fee, DEFAULT_MINE_FEE);
    }
}
