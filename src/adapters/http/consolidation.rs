use axum::{Form, Json, extract::State};
use serde::Deserialize;
use serde_json::{Value, json};

use super::HttpState;
use crate::domain::{Amount, OutPoint};

#[derive(Deserialize)]
pub(super) struct PreviewForm {
    fee_per_byte: Amount,
    #[serde(default)]
    merge_roots: bool,
}

#[derive(Deserialize)]
pub(super) struct SubmitForm {
    fee_per_byte: Amount,
    max_fee: Amount,
    merge_roots: bool,
    address: String,
    utxos: String,
}

pub(super) async fn preview(
    State(state): State<HttpState>,
    Form(form): Form<PreviewForm>,
) -> Json<Value> {
    let node = state.node.lock().await;
    match node.consolidation_plan(form.fee_per_byte, form.merge_roots) {
        Ok(plan) => Json(json!({ "ok": true, "plan": plan })),
        Err(error) => Json(json!({ "ok": false, "error": error.to_string() })),
    }
}

pub(super) async fn submit(
    State(state): State<HttpState>,
    Form(form): Form<SubmitForm>,
) -> Json<Value> {
    let points: Vec<OutPoint> = match serde_json::from_str(&form.utxos) {
        Ok(points) => points,
        Err(_) => return Json(json!({ "ok": false, "error": "Invalid output selection" })),
    };
    let (result, outbox) = {
        let mut node = state.node.lock().await;
        let result = node.consolidate(
            &points,
            form.fee_per_byte,
            form.max_fee,
            form.merge_roots,
            &form.address,
        );
        (result, node.drain_outbox())
    };
    match result {
        Ok(transaction_id) => {
            // Submission has already happened. Return its identifier even if broadcasting fails;
            // the UI must never blindly retry a possibly accepted financial action. Keep the
            // legacy `signature` field name for API compatibility with existing clients.
            let broadcast = state.gossip.broadcast(outbox).await;
            Json(json!({ "ok": true, "signature": transaction_id,
                "broadcast_error": broadcast.err().map(|error| error.to_string()) }))
        }
        Err(error) => Json(json!({ "ok": false, "error": error.to_string() })),
    }
}
