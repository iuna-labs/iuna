use std::{net::SocketAddr, path::Path, sync::Arc};

use anyhow::{Context, Result, bail};
use axum::{
    Form, Json,
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Redirect, Response},
};
use tokio::sync::Mutex;

use super::types::{
    ActionResponse, AddressBookDeleteForm, AddressBookForm, BurnSettingsForm, ChainResetForm,
    ConfigForm, FeeEstimateResponse, MetricsSettingsForm, P2pAnnounceForm, P2pInboundForm,
    PeerForm, PowMiningForm, RecoveryVdfSettingsForm, SeedPhraseForm, TransferForm,
    WalletSetupResponse,
};
use super::{
    HttpState, action_json, api_error, config_store, estimate_burn_fee, estimate_mine_fee,
    estimate_transfer_fee, fee_estimate_json, required_fee_per_byte_burn, transfer,
    validate_address, wallet_setup_json,
};
use crate::{
    adapters::{
        chain_store::SqliteChainStore, config_store::UiConfig, ui_data_store::SqliteUiDataStore,
    },
    app::GossipEnvelope,
    domain::Amount,
};

const CHAIN_RESET_CONFIRMATION: &str = "RESET";

pub(super) async fn apply_config_form(state: &HttpState, form: ConfigForm) -> Result<()> {
    let peer = form.peer.trim();
    if !peer.is_empty() {
        add_peer(state, peer.to_string()).await?;
    }
    if form.setup_complete && super::setup_requires_peer(state).await {
        let has_peer = !state.peers.lock().await.addresses().is_empty();
        if !has_peer {
            bail!("add a bootstrap peer before completing setup");
        }
    }
    let mut config = state.ui_config.lock().await;
    config.setup_complete = form.setup_complete;
    config_store::save(&state.config_path, &config)
}

pub(super) async fn api_wallet_generate_form(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Json<WalletSetupResponse> {
    wallet_setup_json(super::replace_setup_wallet_with_generated_seed(&state, &headers).await)
}

pub(super) async fn api_wallet_import_form(
    State(state): State<HttpState>,
    headers: HeaderMap,
    Form(form): Form<SeedPhraseForm>,
) -> Json<WalletSetupResponse> {
    wallet_setup_json(super::import_setup_wallet_seed(&state, &headers, &form.seed_phrase).await)
}

pub(super) async fn api_transfer_fee_estimate_form(
    State(state): State<HttpState>,
    Form(form): Form<TransferForm>,
) -> Json<FeeEstimateResponse> {
    fee_estimate_json(estimate_transfer_fee(&state, form).await)
}

pub(super) async fn api_burn_fee_estimate_form(
    State(state): State<HttpState>,
    Form(form): Form<BurnSettingsForm>,
) -> Json<FeeEstimateResponse> {
    fee_estimate_json(estimate_burn_fee(&state, form).await)
}

pub(super) async fn api_mine_fee_estimate_form(
    State(state): State<HttpState>,
    Form(_form): Form<std::collections::BTreeMap<String, String>>,
) -> Json<FeeEstimateResponse> {
    fee_estimate_json(estimate_mine_fee(&state).await)
}

pub(super) async fn api_burn_per_block_form(
    State(state): State<HttpState>,
    Form(form): Form<BurnSettingsForm>,
) -> Json<ActionResponse> {
    let enabled = form.enabled.unwrap_or(form.amount > 0);
    let result = match required_fee_per_byte_burn(&form) {
        Ok(fee_per_byte) => set_burn_settings(&state, enabled, form.amount, fee_per_byte).await,
        Err(error) => Err(error),
    };
    action_json(result)
}

pub(super) async fn api_pow_mining_form(
    State(state): State<HttpState>,
    Form(form): Form<PowMiningForm>,
) -> Json<ActionResponse> {
    action_json(set_pow_mining(&state, form.enabled, form.workers).await)
}

pub(super) async fn api_metrics_settings_form(
    State(state): State<HttpState>,
    Form(form): Form<MetricsSettingsForm>,
) -> Json<ActionResponse> {
    action_json(set_keep_track_of_metrics(&state, form.enabled).await)
}

pub(super) async fn api_recovery_vdf_settings_form(
    State(state): State<HttpState>,
    Form(form): Form<RecoveryVdfSettingsForm>,
) -> Json<ActionResponse> {
    action_json(set_recovery_vdf_top_rank_percent(&state, form.top_rank_percent).await)
}

pub(super) async fn api_chain_reset_form(
    State(state): State<HttpState>,
    Form(form): Form<ChainResetForm>,
) -> Json<ActionResponse> {
    action_json(reset_local_chain(&state, &form.confirm).await)
}

pub(super) async fn api_p2p_announce_form(
    State(state): State<HttpState>,
    Form(form): Form<P2pAnnounceForm>,
) -> Json<ActionResponse> {
    action_json(set_p2p_announce_addr(&state, form.addr).await)
}

pub(super) async fn api_p2p_inbound_form(
    State(state): State<HttpState>,
    Form(form): Form<P2pInboundForm>,
) -> Json<ActionResponse> {
    action_json(set_p2p_accept_inbound(&state, form.enabled, form.bind_port).await)
}

pub(super) async fn burn_per_block_form(
    State(state): State<HttpState>,
    Form(form): Form<BurnSettingsForm>,
) -> Response {
    let enabled = form.enabled.unwrap_or(form.amount > 0);
    let result = match required_fee_per_byte_burn(&form) {
        Ok(fee_per_byte) => set_burn_settings(&state, enabled, form.amount, fee_per_byte).await,
        Err(error) => Err(error),
    };
    match result {
        Ok(_) => Redirect::to("/").into_response(),
        Err(error) => api_error(error).into_response(),
    }
}

pub(super) async fn api_transfer_form(
    State(state): State<HttpState>,
    Form(form): Form<TransferForm>,
) -> Json<ActionResponse> {
    let result = transfer(&state, form).await;
    action_json(result)
}

pub(super) async fn transfer_form(
    State(state): State<HttpState>,
    Form(form): Form<TransferForm>,
) -> Response {
    match transfer(&state, form).await {
        Ok(_) => Redirect::to("/").into_response(),
        Err(error) => api_error(error).into_response(),
    }
}

pub(super) async fn api_peer_form(
    State(state): State<HttpState>,
    Form(form): Form<PeerForm>,
) -> Json<ActionResponse> {
    let result = add_peer(&state, form.peer).await;
    action_json(result)
}

pub(super) async fn api_peer_delete_form(
    State(state): State<HttpState>,
    Form(form): Form<PeerForm>,
) -> Json<ActionResponse> {
    let result = remove_peer(&state, form.peer).await;
    action_json(result)
}

pub(super) async fn api_address_book_form(
    State(state): State<HttpState>,
    Form(form): Form<AddressBookForm>,
) -> Json<ActionResponse> {
    action_json(upsert_address_book_entry(&state, form.address, form.name, form.old_address).await)
}

pub(super) async fn api_address_book_delete_form(
    State(state): State<HttpState>,
    Form(form): Form<AddressBookDeleteForm>,
) -> Json<ActionResponse> {
    action_json(remove_address_book_entry(&state, form.address).await)
}

pub(super) async fn peer_form(
    State(state): State<HttpState>,
    Form(form): Form<PeerForm>,
) -> Response {
    match add_peer(&state, form.peer).await {
        Ok(()) => Redirect::to("/").into_response(),
        Err(error) => api_error(error).into_response(),
    }
}

pub(super) async fn set_burn_settings(
    state: &HttpState,
    enabled: bool,
    amount: Amount,
    fee: Amount,
) -> Result<()> {
    if enabled && amount == 0 {
        bail!("IUNA per block must be greater than zero when finalization burns are on");
    }
    let result = {
        let mut node = state.node.lock().await;
        let result = node.set_automatic_burn_settings(enabled, amount, fee);
        let outbox = node.drain_outbox();
        (result, outbox)
    };

    match result.0 {
        Ok(_) => {
            persist_burn_settings_config(
                &state.ui_config,
                &state.config_path,
                enabled,
                amount,
                fee,
            )
            .await?;
            state.gossip.broadcast(result.1).await
        }
        Err(error) => Err(error),
    }
}

pub(super) async fn persist_burn_settings_config(
    ui_config: &Arc<Mutex<UiConfig>>,
    config_path: &Path,
    enabled: bool,
    amount: Amount,
    fee: Amount,
) -> Result<()> {
    let mut config = ui_config.lock().await;
    config.mining_enabled = enabled;
    config.burn_per_block = amount;
    config.burn_fee = fee;
    config_store::save(config_path, &config)
}

pub(super) async fn set_pow_mining(
    state: &HttpState,
    enabled: bool,
    workers: Option<u8>,
) -> Result<()> {
    let workers = match workers {
        Some(workers) => workers,
        None => state.ui_config.lock().await.pow_mining_workers,
    };
    {
        let mut node = state.node.lock().await;
        node.set_pow_mining_workers(workers);
        node.set_pow_mining_enabled(enabled);
    }
    persist_pow_mining_config(&state.ui_config, &state.config_path, enabled, workers).await
}

pub(super) async fn persist_pow_mining_config(
    ui_config: &Arc<Mutex<UiConfig>>,
    config_path: &Path,
    enabled: bool,
    workers: u8,
) -> Result<()> {
    let mut config = ui_config.lock().await;
    config.pow_mining_enabled = enabled;
    config.pow_mining_workers = config_store::clamp_pow_mining_workers(workers);
    config_store::save(config_path, &config)
}

pub(super) async fn set_recovery_vdf_top_rank_percent(
    state: &HttpState,
    percent: u8,
) -> Result<()> {
    let percent = percent.min(100);
    {
        let mut node = state.node.lock().await;
        node.set_recovery_vdf_top_rank_percent(percent);
    }
    let mut config = state.ui_config.lock().await;
    config.recovery_vdf_top_rank_percent = percent;
    config_store::save(&state.config_path, &config)
}

pub(super) async fn set_keep_track_of_metrics(state: &HttpState, enabled: bool) -> Result<()> {
    if enabled {
        let snapshot = {
            let node = state.node.lock().await;
            node.has_real_chain().then(|| node.chain_snapshot())
        };
        if let Some(snapshot) = snapshot {
            replace_metrics_for_snapshot(&state.ui_data_store, snapshot).await?;
        } else {
            clear_metrics(&state.ui_data_store).await?;
        }
    } else {
        clear_metrics(&state.ui_data_store).await?;
    }

    let mut config = state.ui_config.lock().await;
    config.keep_track_of_metrics = enabled;
    config_store::save(&state.config_path, &config)
}

pub(super) async fn reset_local_chain(state: &HttpState, confirmation: &str) -> Result<()> {
    if confirmation.trim() != CHAIN_RESET_CONFIRMATION {
        bail!("type RESET to confirm deleting the local chain");
    }

    {
        let mut node = state.node.lock().await;
        node.reset_chain_to_setup_placeholder();
    }
    {
        let mut cache = state.ui_cache.lock().await;
        *cache = super::UiChainCache::default();
    }
    clear_chain(&state.chain_store).await?;
    clear_ui_data(&state.ui_data_store).await?;
    state
        .gossip
        .broadcast(vec![GossipEnvelope::ChainSnapshotRequest])
        .await?;
    Ok(())
}

pub(super) async fn set_p2p_announce_addr(state: &HttpState, addr: String) -> Result<()> {
    let trimmed = addr.trim();
    let parsed = if trimmed.is_empty() {
        None
    } else {
        Some(
            trimmed
                .parse::<SocketAddr>()
                .with_context(|| format!("invalid P2P announce address {trimmed}"))?,
        )
    };

    let mut config = state.ui_config.lock().await;
    let mut next_config = config.clone();
    next_config.p2p_announce_addr = parsed.map(|addr| addr.to_string());
    config_store::save(&state.config_path, &next_config)?;
    *config = next_config;
    drop(config);
    state.gossip.set_p2p_announce_addr(parsed).await;
    Ok(())
}

pub(super) async fn set_p2p_accept_inbound(
    state: &HttpState,
    enabled: bool,
    bind_port: Option<u16>,
) -> Result<()> {
    let bind_port = bind_port.unwrap_or(config_store::DEFAULT_P2P_BIND_PORT);
    if bind_port == 0 {
        bail!("P2P bind port must be between 1 and 65535");
    }
    let previous = state.gossip.accepts_inbound().await;
    if enabled && previous {
        state.gossip.set_accept_inbound(true).await?;
    }

    let mut config = state.ui_config.lock().await;
    let mut next_config = config.clone();
    next_config.p2p_accept_inbound = enabled;
    next_config.p2p_bind_port = bind_port;
    if let Err(error) = config_store::save(&state.config_path, &next_config) {
        let _ = state.gossip.set_accept_inbound(previous).await;
        return Err(error);
    }
    *config = next_config;
    drop(config);

    if !enabled {
        state.gossip.set_accept_inbound(false).await?;
    }

    Ok(())
}

async fn replace_metrics_for_snapshot(
    store: &SqliteUiDataStore,
    snapshot: crate::domain::ChainSnapshot,
) -> Result<()> {
    let store = store.clone();
    tokio::task::spawn_blocking(move || store.replace_metrics_for_snapshot(&snapshot))
        .await
        .context("metrics worker failed")??;
    Ok(())
}

async fn clear_metrics(store: &SqliteUiDataStore) -> Result<()> {
    let store = store.clone();
    tokio::task::spawn_blocking(move || store.clear_metrics())
        .await
        .context("metrics cleanup worker failed")??;
    Ok(())
}

async fn clear_ui_data(store: &SqliteUiDataStore) -> Result<()> {
    let store = store.clone();
    tokio::task::spawn_blocking(move || store.clear_all())
        .await
        .context("UI data cleanup worker failed")??;
    Ok(())
}

async fn clear_chain(store: &SqliteChainStore) -> Result<()> {
    let store = store.clone();
    tokio::task::spawn_blocking(move || store.clear_chain())
        .await
        .context("chain reset worker failed")??;
    Ok(())
}

pub(super) async fn add_peer(state: &HttpState, peer: String) -> Result<()> {
    let peer = validate_peer_address(peer)?;
    let addresses = {
        let mut peers = state.peers.lock().await;
        peers.add_peer(peer);
        peers.addresses()
    };
    let mut config = state.ui_config.lock().await;
    config.peers = addresses;
    config_store::save(&state.config_path, &config)
}

pub(super) async fn remove_peer(state: &HttpState, peer: String) -> Result<()> {
    let peer = validate_peer_address(peer)?;
    let addresses = {
        let mut peers = state.peers.lock().await;
        if !peers.remove_peer(&peer) {
            bail!("peer is not configured as an outbound peer");
        }
        peers.addresses()
    };
    let mut config = state.ui_config.lock().await;
    config.peers = addresses;
    config_store::save(&state.config_path, &config)
}

pub(super) async fn upsert_address_book_entry(
    state: &HttpState,
    address: String,
    name: String,
    old_address: Option<String>,
) -> Result<()> {
    let address = validate_address_book_address(address)?;
    let name = validate_address_book_name(name)?;
    let old_address = old_address.map(validate_address_book_address).transpose()?;
    let mut config = state.ui_config.lock().await;
    if let Some(old_address) = old_address.as_deref() {
        if old_address != address {
            if config.address_book.contains_key(&address) {
                bail!("address is already saved");
            }
            config.address_book.remove(old_address);
        }
    } else if config.address_book.contains_key(&address) {
        bail!("address is already saved");
    }
    config.address_book.insert(address, name);
    config_store::save(&state.config_path, &config)
}

pub(super) async fn remove_address_book_entry(state: &HttpState, address: String) -> Result<()> {
    let address = validate_address_book_address(address)?;
    let mut config = state.ui_config.lock().await;
    config.address_book.remove(&address);
    config_store::save(&state.config_path, &config)
}

fn validate_peer_address(peer: String) -> Result<String> {
    let peer = peer.trim().to_string();
    if peer.is_empty() {
        bail!("peer address is required");
    }
    Ok(peer)
}

fn validate_address_book_address(address: String) -> Result<String> {
    let address = address.trim().to_string();
    if address.is_empty() {
        bail!("address is required");
    }
    validate_address(&address, "address book")?;
    Ok(address.to_ascii_lowercase())
}

fn validate_address_book_name(name: String) -> Result<String> {
    let name = name.trim().to_string();
    if name.is_empty() {
        bail!("name is required");
    }
    Ok(name)
}
