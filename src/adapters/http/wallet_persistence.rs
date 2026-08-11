use std::time::Duration;

use tokio::time::sleep;

use crate::adapters::wallet_store;

use super::{HttpState, now_ms};

pub(super) async fn run_owned_blinded_outbox_persistence(state: HttpState) {
    loop {
        sleep(Duration::from_millis(500)).await;
        let (version, entries) = {
            let node = state.node.lock().await;
            let version = node.owned_blinded_outbox_version();
            if version == 0 {
                continue;
            }
            (version, node.owned_blinded_transactions())
        };
        let password = wallet_persistence_password(&state).await;
        match wallet_store::replace_owned_blinded_transactions(
            &state.wallet_path,
            password.as_deref(),
            entries,
        ) {
            Ok(()) => {
                state
                    .node
                    .lock()
                    .await
                    .mark_owned_blinded_outbox_persisted(version);
            }
            Err(error) if format!("{error:#}").contains("wallet is encrypted") => {}
            Err(error) => eprintln!("failed to persist owned blinded transactions: {error:#}"),
        }
    }
}

async fn wallet_persistence_password(state: &HttpState) -> Option<String> {
    let metadata = wallet_store::metadata(&state.wallet_path).ok().flatten()?;
    if !metadata.encrypted {
        return None;
    }
    let now = now_ms();
    state
        .auth_sessions
        .lock()
        .await
        .values()
        .find(|session| session.expires_at > now)
        .map(|session| session.wallet_password.clone())
}
