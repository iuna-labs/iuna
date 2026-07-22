use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::Path,
};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

const CONFIG_FILE_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct UiConfig {
    pub setup_complete: bool,
    pub peers: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize)]
struct ConfigFile {
    version: u32,
    setup_complete: bool,
    #[serde(default)]
    peers: Vec<String>,
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
        setup_complete: config.setup_complete,
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

    Ok(UiConfig {
        setup_complete: stored.setup_complete,
        peers: stored.peers,
    })
}

fn create_config_file(path: &Path) -> Result<File> {
    OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .with_context(|| format!("failed to create config file {}", path.display()))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{UiConfig, load_or_create, save};

    #[test]
    fn creates_default_config_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("config.json");

        let config = load_or_create(&path).unwrap();

        assert!(!config.setup_complete);
        let stored = fs::read_to_string(path).unwrap();
        assert!(stored.contains("\"version\": 1"));
        assert!(stored.contains("\"setup_complete\": false"));
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
                peers: vec!["127.0.0.1:9444".to_string()],
            },
        )
        .unwrap();
        let config = load_or_create(&path).unwrap();

        assert!(config.setup_complete);
        assert_eq!(config.peers, vec!["127.0.0.1:9444"]);
    }
}
