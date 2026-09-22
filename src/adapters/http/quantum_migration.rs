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
    let (result, outbox) = {
        let mut node = state.node.lock().await;
        let result =
            node.submit_quantum_migration(form.fee_per_byte, form.max_fee, &form.transaction_id);
        (result, node.drain_outbox())
    };
    match result {
        Ok(preview) => {
            let broadcast = state.gossip.broadcast(outbox).await;
            Json(json!({
                "ok": true,
                "transaction_id": preview.transaction_id,
                "remaining_legacy_utxos": preview.remaining_legacy_utxos,
                "broadcast_error": broadcast.err().map(|error| error.to_string()),
            }))
        }
        Err(error) => Json(json!({ "ok": false, "error": error.to_string() })),
    }
}
