use anyhow::{Context, Result, bail};
use axum::{Json, extract::State, http::HeaderMap};

use crate::{
    adapters::wallet_store,
    app::FeeEstimate,
    domain::{Amount, MINE_FINALIZER_FEE, OutPoint},
};

use super::{
    HttpState,
    types::{BurnSettingsForm, FeeEstimateResponse, TransferForm, WalletSetupResponse},
    wallet_password_for_request,
};

pub(super) async fn api_wallet_setup(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Json<WalletSetupResponse> {
    wallet_setup_json(wallet_setup_response(&state, &headers).await)
}

pub(super) async fn wallet_setup_response(
    state: &HttpState,
    headers: &HeaderMap,
) -> Result<WalletSetupResponse> {
    let setup_complete = state.ui_config.lock().await.setup_complete;
    let password = wallet_password_for_request(state, headers).await;
    let migration_required = state.node.lock().await.network_migration_from().is_some();
    let seed_phrase = if setup_complete && !migration_required {
        None
    } else {
        wallet_store::setup_seed_phrase_with_password(&state.wallet_path, password.as_deref())?
    };
    let address = state.node.lock().await.wallet_receive_address()?;
    Ok(WalletSetupResponse {
        ok: true,
        error: None,
        address: Some(address),
        seed_phrase,
        dev_verify_bypass: dev_seed_verify_bypass_enabled(),
        requires_peer: setup_requires_peer(state).await,
    })
}

pub(super) async fn setup_requires_peer(state: &HttpState) -> bool {
    !state.node.lock().await.has_real_chain()
}

pub(super) async fn replace_setup_wallet_with_generated_seed(
    state: &HttpState,
    headers: &HeaderMap,
) -> Result<WalletSetupResponse> {
    ensure_wallet_setup_open(state).await?;
    let password = wallet_password_for_request(state, headers)
        .await
        .context("wallet password session is required")?;
    let (wallet, seed_phrase) =
        wallet_store::replace_with_generated_seed_phrase_encrypted(&state.wallet_path, &password)?;
    state.node.lock().await.replace_wallet(wallet);
    let address = state.node.lock().await.wallet_receive_address()?;
    Ok(WalletSetupResponse {
        ok: true,
        error: None,
        address: Some(address),
        seed_phrase: Some(seed_phrase),
        dev_verify_bypass: dev_seed_verify_bypass_enabled(),
        requires_peer: setup_requires_peer(state).await,
    })
}

pub(super) async fn import_setup_wallet_seed(
    state: &HttpState,
    headers: &HeaderMap,
    seed_phrase: &str,
) -> Result<WalletSetupResponse> {
    ensure_wallet_setup_open(state).await?;
    let password = wallet_password_for_request(state, headers)
        .await
        .context("wallet password session is required")?;
    let wallet = wallet_store::replace_with_imported_seed_phrase_encrypted(
        &state.wallet_path,
        seed_phrase,
        &password,
    )?;
    state.node.lock().await.replace_wallet(wallet);
    let address = state.node.lock().await.wallet_receive_address()?;
    Ok(WalletSetupResponse {
        ok: true,
        error: None,
        address: Some(address),
        seed_phrase: None,
        dev_verify_bypass: dev_seed_verify_bypass_enabled(),
        requires_peer: setup_requires_peer(state).await,
    })
}

async fn ensure_wallet_setup_open(state: &HttpState) -> Result<()> {
    let setup_complete = state.ui_config.lock().await.setup_complete;
    let migration_required = state.node.lock().await.network_migration_from().is_some();
    if setup_complete && !migration_required {
        bail!("wallet setup is already complete");
    }
    Ok(())
}

pub(super) fn wallet_setup_json(result: Result<WalletSetupResponse>) -> Json<WalletSetupResponse> {
    match result {
        Ok(response) => Json(response),
        Err(error) => Json(WalletSetupResponse {
            ok: false,
            error: Some(format!("{error:#}")),
            address: None,
            seed_phrase: None,
            dev_verify_bypass: dev_seed_verify_bypass_enabled(),
            requires_peer: false,
        }),
    }
}

fn dev_seed_verify_bypass_enabled() -> bool {
    dev_seed_verify_bypass_allowed(std::env::var_os("IUNA_DEV_SKIP_SEED_VERIFY").is_some())
}

pub(super) fn dev_seed_verify_bypass_allowed(env_present: bool) -> bool {
    env_present
}

pub(super) async fn transfer(state: &HttpState, form: TransferForm) -> Result<()> {
    let (to, amount, fee_per_byte, selected_utxos) = validate_transfer_form(form)?;

    let result = {
        let mut node = state.node.lock().await;
        let to = node.normalize_user_address(&to)?;
        let result = node.transfer_with_fee_rate(to, amount, fee_per_byte, &selected_utxos);
        let outbox = node.drain_outbox();
        (result, outbox)
    };

    match result.0 {
        Ok(_) => state.gossip.broadcast(result.1).await,
        Err(error) => Err(error),
    }
}

pub(super) fn validate_transfer_form(
    form: TransferForm,
) -> Result<(String, Amount, Amount, Vec<OutPoint>)> {
    let to = form.to.trim();
    if to.is_empty() {
        bail!("recipient is required");
    }
    if form.amount == 0 {
        bail!("amount must be greater than zero");
    }
    let fee = required_fee_per_byte_transfer(&form)?;
    let selected_utxos = form
        .utxos
        .lines()
        .flat_map(|line| line.split(','))
        .map(str::trim)
        .filter(|value| !value.trim().is_empty())
        .map(parse_outpoint)
        .collect::<Result<Vec<_>>>()?;
    Ok((to.to_string(), form.amount, fee, selected_utxos))
}

pub(super) async fn estimate_transfer_fee(
    state: &HttpState,
    form: TransferForm,
) -> Result<FeeEstimate> {
    let (to, amount, fee_per_byte, selected_utxos) = validate_transfer_form(form)?;
    let node = state.node.lock().await;
    let to = node.normalize_user_address(&to)?;
    node.estimate_transfer_fee(to, amount, fee_per_byte, &selected_utxos)
}

pub(super) async fn estimate_burn_fee(
    state: &HttpState,
    form: BurnSettingsForm,
) -> Result<FeeEstimate> {
    let fee_per_byte = required_fee_per_byte_burn(&form)?;
    if form.amount == 0 {
        bail!("amount must be greater than zero");
    }
    state
        .node
        .lock()
        .await
        .estimate_burn_fee(form.amount, fee_per_byte)
}

pub(super) async fn estimate_mine_fee(state: &HttpState) -> Result<FeeEstimate> {
    state
        .node
        .lock()
        .await
        .estimate_mine_fee(MINE_FINALIZER_FEE)
}

fn required_fee_per_byte_transfer(form: &TransferForm) -> Result<Amount> {
    form.fee_per_byte.context("fee per byte is required")
}

pub(super) fn required_fee_per_byte_burn(form: &BurnSettingsForm) -> Result<Amount> {
    form.fee_per_byte.context("fee per byte is required")
}

pub(super) fn fee_estimate_json(result: Result<FeeEstimate>) -> Json<FeeEstimateResponse> {
    match result {
        Ok(estimate) => Json(FeeEstimateResponse {
            ok: true,
            error: None,
            bytes: Some(estimate.bytes),
            fee: Some(estimate.fee),
        }),
        Err(error) => Json(FeeEstimateResponse {
            ok: false,
            error: Some(format!("{error:#}")),
            bytes: None,
            fee: None,
        }),
    }
}

fn parse_outpoint(value: &str) -> Result<OutPoint> {
    let (txid, index) = value
        .rsplit_once(':')
        .with_context(|| format!("invalid UTXO reference {value}"))?;
    if txid.is_empty() {
        bail!("invalid UTXO reference {value}");
    }
    Ok(OutPoint {
        txid: txid.to_string(),
        index: index
            .parse::<u32>()
            .with_context(|| format!("invalid UTXO reference {value}"))?,
    })
}
