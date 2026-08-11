use std::{
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    str::FromStr,
};

use anyhow::{Context, Result, bail};
use iuna::{adapters::config_store, domain::Amount};

use super::{GENESIS_INITIAL_BURN_FEE, GENESIS_INITIAL_BURN_PER_BLOCK};

pub(crate) fn configured_p2p_announce_addr(
    opts: &CliOptions,
    ui_config: &config_store::UiConfig,
) -> Result<Option<SocketAddr>> {
    if let Some(addr) = opts.p2p_announce_addr {
        return Ok(Some(addr));
    }
    ui_config
        .p2p_announce_addr
        .as_deref()
        .map(|addr| {
            addr.parse()
                .with_context(|| format!("invalid configured P2P announce address {addr}"))
        })
        .transpose()
}

pub(crate) fn configured_p2p_bind_addr(
    opts: &CliOptions,
    ui_config: &config_store::UiConfig,
) -> SocketAddr {
    if opts.p2p_addr_configured || ui_config.p2p_accept_inbound {
        return SocketAddr::from((Ipv4Addr::UNSPECIFIED, ui_config.p2p_bind_port));
    }
    opts.p2p_addr
}

pub(crate) fn apply_cli_p2p_config_overrides(
    opts: &CliOptions,
    ui_config: &mut config_store::UiConfig,
) -> bool {
    let mut dirty = false;
    if opts.p2p_addr_configured {
        let bind_port = opts.p2p_addr.port();
        if ui_config.p2p_bind_port != bind_port {
            ui_config.p2p_bind_port = bind_port;
            dirty = true;
        }
    }
    if let Some(addr) = opts.p2p_announce_addr {
        let announce_addr = addr.to_string();
        if !ui_config.p2p_accept_inbound
            || ui_config.p2p_announce_addr.as_deref() != Some(&announce_addr)
        {
            ui_config.p2p_accept_inbound = true;
            ui_config.p2p_announce_addr = Some(announce_addr);
            dirty = true;
        }
    }
    dirty
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ChainMode {
    Setup,
    Genesis,
    Join,
}

#[derive(Debug)]
pub(crate) struct CliOptions {
    pub(crate) wallet_path: Option<PathBuf>,
    pub(crate) chain_db_path: Option<PathBuf>,
    pub(crate) http_addr: SocketAddr,
    pub(crate) p2p_addr: SocketAddr,
    pub(crate) p2p_addr_configured: bool,
    pub(crate) p2p_announce_addr: Option<SocketAddr>,
    pub(crate) stratum_addr: Option<SocketAddr>,
    pub(crate) peers: Vec<String>,
    pub(crate) join_peers: Vec<String>,
    pub(crate) chain_mode: ChainMode,
    pub(crate) data_dir: PathBuf,
    pub(crate) debug: bool,
}

impl CliOptions {
    pub(crate) fn parse() -> Result<Option<Self>> {
        Self::parse_from(std::env::args().skip(1))
    }

    pub(crate) fn parse_from(args: impl IntoIterator<Item = String>) -> Result<Option<Self>> {
        let mut opts = Self {
            wallet_path: None,
            chain_db_path: None,
            http_addr: SocketAddr::from_str("127.0.0.1:18661")?,
            p2p_addr: SocketAddr::from_str("127.0.0.1:9444")?,
            p2p_addr_configured: false,
            p2p_announce_addr: None,
            stratum_addr: None,
            peers: Vec::new(),
            join_peers: Vec::new(),
            chain_mode: ChainMode::Setup,
            data_dir: default_data_dir(),
            debug: false,
        };

        let raw_args = args.into_iter().collect::<Vec<_>>();
        let mut args = raw_args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--genesis" => {
                    if opts.chain_mode == ChainMode::Join {
                        bail!("choose either --genesis or --join, not both");
                    }
                    opts.chain_mode = ChainMode::Genesis;
                }
                "--wallet" => {
                    opts.wallet_path = Some(PathBuf::from(next_value(&mut args, "--wallet")?))
                }
                "--chain-db" => {
                    opts.chain_db_path = Some(PathBuf::from(next_value(&mut args, "--chain-db")?))
                }
                "--wallet-seed" => {
                    bail!(
                        "--wallet-seed was removed; wallets are stored in --wallet <path> or ~/.iuna/wallet.json"
                    )
                }
                "--http" => {
                    opts.http_addr = next_value(&mut args, "--http")?
                        .parse()
                        .context("invalid --http address")?;
                }
                "--p2p" => {
                    opts.p2p_addr = next_value(&mut args, "--p2p")?
                        .parse()
                        .context("invalid --p2p address")?;
                    opts.p2p_addr_configured = true;
                }
                "--p2p-announce" => {
                    opts.p2p_announce_addr = Some(
                        next_value(&mut args, "--p2p-announce")?
                            .parse()
                            .context("invalid --p2p-announce address")?,
                    );
                }
                "--stratum" => {
                    opts.stratum_addr = Some(
                        next_value(&mut args, "--stratum")?
                            .parse()
                            .context("invalid --stratum address")?,
                    );
                }
                "--join" => {
                    if opts.chain_mode == ChainMode::Genesis {
                        bail!("choose either --genesis or --join, not both");
                    }
                    let peer = next_value(&mut args, "--join")?;
                    opts.chain_mode = ChainMode::Join;
                    opts.peers.push(peer.clone());
                    opts.join_peers.push(peer);
                }
                "--data-dir" => opts.data_dir = PathBuf::from(next_value(&mut args, "--data-dir")?),
                "--debug" => opts.debug = true,
                "--help" | "-h" => {
                    print_help();
                    std::process::exit(0);
                }
                other => bail!("unknown argument {other}; pass --help for usage"),
            }
        }

        if opts.chain_mode == ChainMode::Genesis && !opts.join_peers.is_empty() {
            bail!("choose either --genesis or --join, not both");
        }

        Ok(Some(opts))
    }

    pub(crate) fn wallet_path(&self) -> PathBuf {
        self.wallet_path
            .clone()
            .unwrap_or_else(|| self.data_dir.join("wallet.json"))
    }

    pub(crate) fn chain_db_path(&self) -> PathBuf {
        self.chain_db_path
            .clone()
            .unwrap_or_else(|| self.data_dir.join("chain.sqlite3"))
    }

    pub(crate) fn config_path(&self) -> PathBuf {
        self.data_dir.join("config.json")
    }

    pub(crate) fn has_chain(&self) -> bool {
        self.chain_mode != ChainMode::Setup
    }
}

fn next_value(args: &mut impl Iterator<Item = String>, flag: &str) -> Result<String> {
    args.next()
        .with_context(|| format!("missing value after {flag}"))
}

pub(crate) fn validate_wallet_for_mode(
    opts: &CliOptions,
    wallet_path: &Path,
    wallet_file_exists: bool,
) -> Result<()> {
    if opts.chain_mode == ChainMode::Genesis && wallet_file_exists {
        bail!(
            "--genesis requires a fresh wallet path, but {} already exists; start without --genesis to reuse it or choose an empty --data-dir/--wallet",
            wallet_path.display()
        );
    }
    Ok(())
}

pub(crate) fn initial_burn_per_block(
    opts: &CliOptions,
    ui_config: &config_store::UiConfig,
) -> Amount {
    match opts.chain_mode {
        ChainMode::Genesis => GENESIS_INITIAL_BURN_PER_BLOCK,
        ChainMode::Setup | ChainMode::Join => ui_config.burn_per_block,
    }
}

pub(crate) fn initial_burn_fee(opts: &CliOptions, ui_config: &config_store::UiConfig) -> Amount {
    match opts.chain_mode {
        ChainMode::Genesis => GENESIS_INITIAL_BURN_FEE,
        ChainMode::Setup | ChainMode::Join => ui_config.burn_fee,
    }
}

fn print_help() {
    println!("{}", help_text());
}

pub(crate) fn help_text() -> &'static str {
    "iuna\n\n\
         Usage:\n\
           iuna [options]\n\
           iuna --genesis [options]\n\
           iuna --join <addr:port> [options]\n\n\
         Options:\n\
           --genesis                     Create a new chain with a fresh setup wallet\n\
           --wallet <path>               Wallet file (default <data-dir>/wallet.json)\n\
           --chain-db <path>             Chain SQLite database (default <data-dir>/chain.sqlite3)\n\
           --http <addr:port>            HTTP management UI address (default 127.0.0.1:18661)\n\
           --p2p <addr:port>             Inbound P2P listener address when public node is enabled\n\
           --p2p-announce <addr:port>    Public P2P address to gossip; enables inbound P2P\n\
           --stratum <addr:port>         Stratum V1 listener for SHA-256 ASIC miners\n\
           --join <addr:port>            Fetch chain snapshot from this peer before finalization\n\
           --data-dir <path>             Local wallet directory (default ~/.iuna)\n\
           --debug                       Print verbose runtime logs\n\n\
         Environment:\n\
           IUNA_DEV_SKIP_SEED_VERIFY=1 Show a setup button to skip seed verification\n"
}

pub(crate) fn default_data_dir() -> PathBuf {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .or_else(|| std::env::var_os("USERPROFILE").filter(|home| !home.is_empty()))
        .map(PathBuf::from)
        .map(|home| home.join(".iuna"))
        .unwrap_or_else(|| PathBuf::from(".iuna"))
}
