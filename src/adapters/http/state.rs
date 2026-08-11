use std::{collections::BTreeMap, net::SocketAddr, path::PathBuf, sync::Arc};

use tokio::sync::Mutex;

use crate::{
    adapters::{
        chain_store::SqliteChainStore, config_store::UiConfig, p2p::GossipNetwork,
        ui_data_store::SqliteUiDataStore,
    },
    app::{SharedNode, SharedPeerBook, StratumStatus},
    domain::{BurnLeaderRank, OutPoint, RevealedBlindedTransaction, TxOutput},
};

#[derive(Clone)]
pub(super) struct HttpState {
    pub(super) node: SharedNode,
    pub(super) peers: SharedPeerBook,
    pub(super) gossip: GossipNetwork,
    pub(super) ui_config: Arc<Mutex<UiConfig>>,
    pub(super) config_path: PathBuf,
    pub(super) chain_store: SqliteChainStore,
    pub(super) ui_data_store: SqliteUiDataStore,
    pub(super) wallet_path: PathBuf,
    pub(super) stratum: StratumStatus,
    pub(super) auth_sessions: Arc<Mutex<BTreeMap<String, AuthSession>>>,
    pub(super) auth_backoff: Arc<Mutex<BTreeMap<String, AuthBackoff>>>,
    pub(super) ui_cache: Arc<Mutex<UiChainCache>>,
    pub(super) ui_data_refresh: Arc<Mutex<()>>,
}

#[derive(Clone)]
pub(super) struct AuthSession {
    pub(super) expires_at: u64,
    pub(super) wallet_password: String,
}

#[derive(Clone, Debug)]
pub(super) struct AuthClientKey(pub(super) String);

#[derive(Clone, Debug, Default)]
pub(super) struct AuthBackoff {
    pub(super) failed_attempts: u32,
    pub(super) locked_until_ms: Option<u64>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct UiChainCache {
    pub(super) tip_hash: Option<String>,
    pub(super) outputs: BTreeMap<OutPoint, TxOutput>,
    pub(super) revealed_by_height: BTreeMap<u64, Vec<RevealedBlindedTransaction>>,
    pub(super) burn_leader_ranks_by_hash: BTreeMap<String, Vec<BurnLeaderRank>>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct UiChainView {
    pub(super) outputs: BTreeMap<OutPoint, TxOutput>,
    pub(super) revealed_by_height: BTreeMap<u64, Vec<RevealedBlindedTransaction>>,
    pub(super) burn_leader_ranks_by_hash: BTreeMap<String, Vec<BurnLeaderRank>>,
}

pub struct ServeOptions {
    pub config_path: PathBuf,
    pub chain_store: SqliteChainStore,
    pub ui_data_store: SqliteUiDataStore,
    pub wallet_path: PathBuf,
    pub stratum: StratumStatus,
    pub addr: SocketAddr,
}
