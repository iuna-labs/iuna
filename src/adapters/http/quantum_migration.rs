use axum::{Form, Json, extract::State};
use serde::Deserialize;
use serde_json::{Value, json};

use super::HttpState;
use crate::domain::Amount;

#[derive(Deserialize)]
pub(super) struct PreviewForm {
    fee_per_byte: Amount,
}

#[derive(Deserialize)]
pub(super) struct SubmitForm {
    fee_per_byte: Amount,
    max_fee: Amount,
    transaction_id: String,
}

pub(super) async fn preview(
    State(state): State<HttpState>,
    Form(form): Form<PreviewForm>,
) -> Json<Value> {
    let node = state.node.lock().await;
    match node.preview_quantum_migration(form.fee_per_byte) {
        Ok(preview) => Json(json!({ "ok": true, "preview": preview })),
        Err(error) => Json(json!({ "ok": false, "error": error.to_string() })),
    }
}

pub(super) async fn submit(
    State(state): State<HttpState>,
    Form(form): Form<SubmitForm>,
) -> Json<Value> {
    let (result, outbox, pending_envelope) = {
        let mut node = state.node.lock().await;
        let result =
            node.submit_quantum_migration(form.fee_per_byte, form.max_fee, &form.transaction_id);
        let pending_envelope = result.as_ref().ok().and_then(|preview| {
            node.pending_transaction_v2_envelopes()
                .ok()?
                .into_iter()
                .find(|(transaction_id, _)| transaction_id == &preview.transaction_id)
        });
        (result, node.drain_outbox(), pending_envelope)
    };
    match result {
        Ok(preview) => {
            let persistence_error = if let Some((transaction_id, envelope)) = pending_envelope {
                let store = state.chain_store.clone();
                tokio::task::spawn_blocking(move || {
                    store.save_pending_transaction_v2(&transaction_id, &envelope)
                })
                .await
                .map_err(|error| anyhow::anyhow!("migration persistence worker failed: {error}"))
                .and_then(|result| result)
                .err()
                .map(|error| error.to_string())
            } else {
                Some(
                    "queued migration is missing from the local transaction-v2 mempool".to_string(),
                )
            };
            let broadcast = state.gossip.broadcast(outbox).await;
            Json(json!({
                "ok": true,
                "transaction_id": preview.transaction_id,
                "remaining_legacy_utxos": preview.remaining_legacy_utxos,
                "persistence_error": persistence_error,
                "broadcast_error": broadcast.err().map(|error| error.to_string()),
            }))
        }
        Err(error) => Json(json!({ "ok": false, "error": error.to_string() })),
    }
}
