use std::{net::SocketAddr, time::Duration};

use anyhow::{Context, Result, bail};
use tokio::{
    net::{TcpListener, TcpStream},
    sync::{mpsc, watch},
    time::{Instant, interval, interval_at, sleep, timeout},
};

use crate::app::{GossipEnvelope, debug_logging_enabled};

use super::{
    CATCHUP_REQUEST_TIMEOUT, CONNECT_TIMEOUT, GossipNetwork, GossipSession, HANDSHAKE_TIMEOUT,
    INBOUND_PEER_QUEUE_BYTES, INBOUND_PEER_QUEUE_SIZE, INBOUND_SESSION_PREFIX,
    INITIAL_RECONNECT_DELAY, MAX_RECONNECT_DELAY, OutboundBatch, PEER_EXCHANGE_INTERVAL,
    PEER_QUEUE_BYTES, PEER_QUEUE_SIZE, PeerStatus, SESSION_SYNC_INTERVAL, is_self_peer_address_for,
    next_reconnect_delay_with_max, process_envelope, process_hello_with_verification,
    read_session_envelope, record_peer_status, respond_to_peer_verification_challenge,
    write_envelope, write_payload, write_peer_exchange,
};

struct InboundRegistration {
    key: String,
    sender: mpsc::Sender<OutboundBatch>,
    shutdown: watch::Sender<bool>,
    queue_bytes: std::sync::Arc<tokio::sync::Semaphore>,
}

pub(super) async fn accept_loop(network: GossipNetwork, listener: TcpListener) {
    loop {
        match listener.accept().await {
            Ok((stream, remote_addr)) => {
                let network = network.clone();
                let permit = match network.try_acquire_inbound_session(remote_addr.ip()) {
                    Ok(permit) => permit,
                    Err(rejection) => {
                        super::P2pMetricsCounters::inc(
                            &network.inner.metrics.inbound_sessions_rejected,
                        );
                        super::P2pMetricsCounters::set_last(
                            &network.inner.metrics.last_session_failure,
                            format!("{remote_addr}: {}", rejection.label()),
                        );
                        if debug_logging_enabled() {
                            eprintln!(
                                "p2p inbound connection from {remote_addr} rejected: {}",
                                rejection.label()
                            );
                        }
                        drop(stream);
                        continue;
                    }
                };
                super::P2pMetricsCounters::inc(&network.inner.metrics.inbound_sessions_started);
                let session_key = format!("{INBOUND_SESSION_PREFIX}{remote_addr}");
                let (sender, receiver) = mpsc::channel(INBOUND_PEER_QUEUE_SIZE);
                let (shutdown, mut shutdown_receiver) = watch::channel(false);
                let queue_bytes =
                    std::sync::Arc::new(tokio::sync::Semaphore::new(INBOUND_PEER_QUEUE_BYTES));
                let registration = InboundRegistration {
                    key: session_key.clone(),
                    sender,
                    shutdown,
                    queue_bytes,
                };
                tokio::spawn(async move {
                    let _permit = permit;
                    let result = session_loop(
                        network.clone(),
                        stream,
                        remote_addr,
                        None,
                        receiver,
                        &mut shutdown_receiver,
                        Some(registration),
                    )
                    .await;
                    network.inner.sessions.lock().await.remove(&session_key);
                    match result {
                        Ok(()) => {
                            super::P2pMetricsCounters::inc(&network.inner.metrics.sessions_closed);
                        }
                        Err(error) if super::is_quiet_disconnect(&error) => {
                            super::P2pMetricsCounters::inc(
                                &network.inner.metrics.quiet_disconnects,
                            );
                        }
                        Err(error) => {
                            super::P2pMetricsCounters::inc(&network.inner.metrics.session_failures);
                            super::P2pMetricsCounters::set_last(
                                &network.inner.metrics.last_session_failure,
                                format!("{remote_addr}: {error:#}"),
                            );
                            if debug_logging_enabled() {
                                eprintln!(
                                    "p2p inbound connection from {remote_addr} failed: {error:#}"
                                );
                            }
                        }
                    }
                });
            }
            Err(error) if debug_logging_enabled() => eprintln!("p2p accept failed: {error:#}"),
            Err(_) => {}
        }
    }
}

pub(super) async fn outbound_supervisor(network: GossipNetwork) {
    let mut tick = interval(Duration::from_secs(2));
    loop {
        tick.tick().await;
        network.ensure_outbound_sessions().await;
    }
}

pub(super) async fn outbound_session(
    network: GossipNetwork,
    peer: String,
    mut receiver: mpsc::Receiver<OutboundBatch>,
    mut shutdown: watch::Receiver<bool>,
) {
    let mut reconnect_delay = INITIAL_RECONNECT_DELAY;
    loop {
        let self_filter_addr = network.self_filter_addr().await;
        if !peer_is_connectable(&network, &peer).await
            || is_self_peer_address_for(&peer, network.inner.listen_addr, self_filter_addr)
        {
            network.inner.sessions.lock().await.remove(&peer);
            return;
        }
        if network.inner.peers.lock().await.is_banned(&peer) {
            sleep(MAX_RECONNECT_DELAY).await;
            continue;
        }
        super::P2pMetricsCounters::inc(&network.inner.metrics.outbound_connect_attempts);
        let stream = match timeout(CONNECT_TIMEOUT, TcpStream::connect(&peer)).await {
            Ok(Ok(stream)) => {
                super::P2pMetricsCounters::inc(&network.inner.metrics.outbound_connect_successes);
                stream
            }
            Ok(Err(error)) => {
                super::P2pMetricsCounters::inc(&network.inner.metrics.outbound_connect_failures);
                network
                    .inner
                    .peers
                    .lock()
                    .await
                    .record_error(&peer, format!("connecting to peer {peer}: {error}"));
                sleep(reconnect_delay).await;
                reconnect_delay = next_reconnect_delay(reconnect_delay);
                continue;
            }
            Err(_) => {
                super::P2pMetricsCounters::inc(&network.inner.metrics.outbound_connect_failures);
                network
                    .inner
                    .peers
                    .lock()
                    .await
                    .record_error(&peer, format!("connecting to peer {peer}: timeout"));
                sleep(reconnect_delay).await;
                reconnect_delay = next_reconnect_delay(reconnect_delay);
                continue;
            }
        };

        reconnect_delay = INITIAL_RECONNECT_DELAY;
        let remote_addr = stream.peer_addr().unwrap_or_else(|_| {
            peer.parse()
                .unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0)))
        });
        super::P2pMetricsCounters::inc(&network.inner.metrics.outbound_sessions_started);
        let result = session_loop(
            network.clone(),
            stream,
            remote_addr,
            Some(peer.clone()),
            receiver,
            &mut shutdown,
            None,
        )
        .await;
        if *shutdown.borrow() {
            network.inner.sessions.lock().await.remove(&peer);
            return;
        }
        match result {
            Ok(()) => {
                super::P2pMetricsCounters::inc(&network.inner.metrics.sessions_closed);
            }
            Err(error) if super::is_quiet_disconnect(&error) => {
                super::P2pMetricsCounters::inc(&network.inner.metrics.quiet_disconnects);
            }
            Err(error) => {
                super::P2pMetricsCounters::inc(&network.inner.metrics.session_failures);
                let message = format!("{error:#}");
                super::P2pMetricsCounters::set_last(
                    &network.inner.metrics.last_session_failure,
                    format!("{peer}: {message}"),
                );
                network
                    .inner
                    .peers
                    .lock()
                    .await
                    .record_error(&peer, message.clone());
                if debug_logging_enabled() {
                    eprintln!("p2p session with {peer} failed: {message}");
                }
            }
        }

        let (sender, next_receiver) = mpsc::channel(PEER_QUEUE_SIZE);
        let (next_shutdown, next_shutdown_receiver) = watch::channel(false);
        let queue_bytes = std::sync::Arc::new(tokio::sync::Semaphore::new(PEER_QUEUE_BYTES));
        receiver = next_receiver;
        shutdown = next_shutdown_receiver;
        if !peer_is_connectable(&network, &peer).await {
            network.inner.sessions.lock().await.remove(&peer);
            return;
        }
        network.inner.sessions.lock().await.insert(
            peer.clone(),
            GossipSession {
                peer: peer.clone(),
                sender,
                shutdown: next_shutdown,
                queue_bytes,
            },
        );
        sleep(reconnect_delay).await;
        reconnect_delay = next_reconnect_delay(reconnect_delay);
    }
}

async fn session_loop(
    network: GossipNetwork,
    stream: TcpStream,
    remote_addr: SocketAddr,
    stable_peer: Option<String>,
    mut outbound: mpsc::Receiver<OutboundBatch>,
    shutdown: &mut watch::Receiver<bool>,
    inbound_registration: Option<InboundRegistration>,
) -> Result<()> {
    let (reader, mut writer) = stream.into_split();
    let connection_label = stable_peer
        .as_ref()
        .map(|peer| format!("outbound {peer}"))
        .unwrap_or_else(|| format!("inbound {remote_addr}"));
    let advertised_addr = network.advertised_addr().await;
    let hello = network.inner.node.lock().await.hello(
        advertised_addr.map(|addr| addr.to_string()),
        Some(network.inner.node_id.clone()),
    );
    write_envelope(&mut writer, &hello).await?;
    let mut reader = super::LimitedLineReader::new(reader);
    let mut sync_tick = interval_at(
        Instant::now() + SESSION_SYNC_INTERVAL,
        SESSION_SYNC_INTERVAL,
    );
    let mut peer_exchange_tick = interval_at(
        Instant::now() + PEER_EXCHANGE_INTERVAL,
        PEER_EXCHANGE_INTERVAL,
    );
    let mut outbound_closed = false;
    let mut peer_status: Option<PeerStatus> = None;
    let mut catchup_requested_at: Option<Instant> = None;
    let is_outbound_session = stable_peer.is_some();
    let mut known_peer = stable_peer;
    let mut handshake_complete = false;
    let mut shutdown_closed = false;

    if !is_outbound_session {
        let envelope = timeout(
            HANDSHAKE_TIMEOUT,
            read_session_envelope(&network, &connection_label, &mut reader),
        )
        .await
        .context("inbound p2p handshake timed out")??
        .context("inbound peer closed before sending Hello")?;
        let hello = match envelope {
            GossipEnvelope::Hello(hello) => hello,
            challenge @ GossipEnvelope::PeerVerificationChallenge { .. } => {
                respond_to_peer_verification_challenge(&network, &mut writer, &challenge).await?;
                return Ok(());
            }
            _ => bail!("inbound peer sent data before Hello"),
        };
        let status = process_hello_with_verification(
            &network,
            &mut writer,
            &mut reader,
            &connection_label,
            remote_addr,
            &mut known_peer,
            hello,
        )
        .await?;
        if status.reject_session {
            return Ok(());
        }
        peer_status = Some(status);
        handshake_complete = true;
        if let Some(registration) = inbound_registration {
            let peer = known_peer
                .clone()
                .unwrap_or_else(|| remote_addr.to_string());
            network.inner.sessions.lock().await.insert(
                registration.key,
                GossipSession {
                    peer,
                    sender: registration.sender,
                    shutdown: registration.shutdown,
                    queue_bytes: registration.queue_bytes,
                },
            );
        }
        maybe_start_catchup(
            &network,
            &mut writer,
            peer_status.as_ref().unwrap(),
            &mut catchup_requested_at,
        )
        .await?;
        write_peer_exchange(&network, &mut writer, &known_peer).await?;
    }

    if is_outbound_session {
        if let Ok(Ok(Some(envelope))) = timeout(
            HANDSHAKE_TIMEOUT,
            read_session_envelope(&network, &connection_label, &mut reader),
        )
        .await
        {
            if let GossipEnvelope::Hello(hello) = envelope {
                let status = process_hello_with_verification(
                    &network,
                    &mut writer,
                    &mut reader,
                    &connection_label,
                    remote_addr,
                    &mut known_peer,
                    hello,
                )
                .await?;
                if status.reject_session {
                    return Ok(());
                }
                peer_status = Some(status);
                handshake_complete = true;
                if is_outbound_session && known_peer.is_none() {
                    return Ok(());
                }
                maybe_start_catchup(
                    &network,
                    &mut writer,
                    peer_status.as_ref().unwrap(),
                    &mut catchup_requested_at,
                )
                .await?;
                write_peer_exchange(&network, &mut writer, &known_peer).await?;
            } else if let GossipEnvelope::PeerStatus {
                height,
                tip_hash,
                time_ms,
            } = envelope
            {
                let status = PeerStatus::from_envelope(height, tip_hash, time_ms);
                record_peer_status(&network, &known_peer, remote_addr, &status).await;
                peer_status = Some(status);
                handshake_complete = true;
                maybe_start_catchup(
                    &network,
                    &mut writer,
                    peer_status.as_ref().unwrap(),
                    &mut catchup_requested_at,
                )
                .await?;
                write_peer_exchange(&network, &mut writer, &known_peer).await?;
            } else if respond_to_peer_verification_challenge(&network, &mut writer, &envelope)
                .await?
            {
                if known_peer.is_none() {
                    return Ok(());
                }
            } else if !maybe_request_inventory(
                &network,
                &mut writer,
                &envelope,
                &mut catchup_requested_at,
            )
            .await?
            {
                let requested_chain_data = process_envelope(
                    &network,
                    &mut writer,
                    remote_addr,
                    &mut known_peer,
                    envelope,
                )
                .await?;
                if requested_chain_data {
                    catchup_requested_at = Some(Instant::now());
                }
                if is_outbound_session && known_peer.is_none() {
                    return Ok(());
                }
            }
        }
    }

    loop {
        tokio::select! {
            result = shutdown.changed(), if !shutdown_closed => {
                match result {
                    Ok(()) if *shutdown.borrow() => return Ok(()),
                    Ok(()) => {}
                    Err(_) if !is_outbound_session => return Ok(()),
                    Err(_) => shutdown_closed = true,
                }
            }
            maybe_batch = outbound.recv(), if !outbound_closed => {
                match maybe_batch {
                    Some(batch) => {
                        let payload = super::envelopes_for_peer(
                            Some(&network.inner.node),
                            peer_status.clone(),
                            &batch.envelopes,
                        ).await;
                        write_payload(&mut writer, &payload).await?;
                        if let Some(peer) = &known_peer {
                            network.inner.peers.lock().await.record_sent(peer, payload.len() as u64);
                        }
                    }
                    None => outbound_closed = true,
                }
            }
            _ = sync_tick.tick() => {
                let status = network.inner.node.lock().await.peer_status();
                write_envelope(&mut writer, &status).await?;
                if catchup_requested_at.is_some_and(|started| started.elapsed() >= CATCHUP_REQUEST_TIMEOUT) {
                    catchup_requested_at = None;
                }
                if let Some(status) = peer_status.as_ref() {
                    maybe_start_catchup(&network, &mut writer, status, &mut catchup_requested_at).await?;
                }
            }
            _ = peer_exchange_tick.tick() => {
                write_peer_exchange(&network, &mut writer, &known_peer).await?;
            }
            envelope = read_session_envelope(&network, &connection_label, &mut reader) => {
                let Some(envelope) = envelope? else {
                    return Ok(());
                };
                if let GossipEnvelope::Hello(hello) = envelope {
                    if handshake_complete {
                        bail!("peer sent duplicate Hello");
                    }
                    let status = process_hello_with_verification(
                            &network,
                            &mut writer,
                            &mut reader,
                            &connection_label,
                            remote_addr,
                            &mut known_peer,
                            hello,
                        )
                        .await?;
                    if status.reject_session {
                        return Ok(());
                    }
                    peer_status = Some(status);
                    handshake_complete = true;
                    if is_outbound_session && known_peer.is_none() {
                        return Ok(());
                    }
                    maybe_start_catchup(
                        &network,
                        &mut writer,
                        peer_status.as_ref().unwrap(),
                        &mut catchup_requested_at,
                    ).await?;
                    write_peer_exchange(&network, &mut writer, &known_peer).await?;
                    continue;
                }
                if let GossipEnvelope::PeerStatus {
                    height,
                    tip_hash,
                    time_ms,
                } = &envelope
                {
                    let status = PeerStatus::from_envelope(*height, tip_hash.clone(), *time_ms);
                    record_peer_status(&network, &known_peer, remote_addr, &status).await;
                    peer_status = Some(status);
                    maybe_start_catchup(
                        &network,
                        &mut writer,
                        peer_status.as_ref().unwrap(),
                        &mut catchup_requested_at,
                    ).await?;
                    write_peer_exchange(&network, &mut writer, &known_peer).await?;
                    continue;
                }

                if respond_to_peer_verification_challenge(&network, &mut writer, &envelope).await? {
                    if known_peer.is_none() {
                        return Ok(());
                    }
                    continue;
                }
                if maybe_request_inventory(
                    &network,
                    &mut writer,
                    &envelope,
                    &mut catchup_requested_at,
                )
                .await?
                {
                    continue;
                }
                let continue_catchup = matches!(
                    &envelope,
                    GossipEnvelope::Block(_) | GossipEnvelope::Blocks { .. } | GossipEnvelope::ChainBootstrap(_)
                );
                let completes_catchup_request = matches!(
                    &envelope,
                    GossipEnvelope::Blocks { .. } | GossipEnvelope::ChainBootstrap(_)
                );
                let (height_before, response_already_applied) = if continue_catchup {
                    let node = network.inner.node.lock().await;
                    let status = node.ledger().status();
                    let already_applied = match &envelope {
                        GossipEnvelope::Blocks { blocks } => {
                            !blocks.is_empty() && node.ledger().contains_block_sequence(blocks)
                        }
                        _ => false,
                    };
                    (Some((status.height, status.tip_hash)), already_applied)
                } else {
                    (None, false)
                };
                let requested_chain_data = process_envelope(
                    &network,
                    &mut writer,
                    remote_addr,
                    &mut known_peer,
                    envelope,
                ).await?;
                let chain_changed = if let Some(height_before) = height_before {
                    let status = network.inner.node.lock().await.ledger().status();
                    status.height != height_before.0 || status.tip_hash != height_before.1
                } else {
                    false
                };
                let response_satisfied = chain_changed || response_already_applied;
                update_catchup_request_state(
                    &mut catchup_requested_at,
                    completes_catchup_request,
                    response_satisfied,
                    requested_chain_data,
                );
                if continue_catchup && response_satisfied && !requested_chain_data {
                    if let Some(status) = peer_status.as_ref() {
                        maybe_start_catchup(
                            &network,
                            &mut writer,
                            status,
                            &mut catchup_requested_at,
                        ).await?;
                    }
                }
                if session_peer_is_banned(&network, &known_peer, remote_addr).await {
                    return Ok(());
                }
                if is_outbound_session && known_peer.is_none() {
                    return Ok(());
                }
            }
        }
    }
}

async fn maybe_start_catchup(
    network: &GossipNetwork,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    peer_status: &PeerStatus,
    requested_at: &mut Option<Instant>,
) -> Result<()> {
    if requested_at.is_none() && super::maybe_request_catchup(network, writer, peer_status).await? {
        *requested_at = Some(Instant::now());
    }
    Ok(())
}

fn update_catchup_request_state(
    requested_at: &mut Option<Instant>,
    completes_request: bool,
    response_satisfied: bool,
    requested_chain_data: bool,
) {
    if requested_chain_data || (completes_request && !response_satisfied) {
        *requested_at = Some(Instant::now());
    } else if completes_request {
        *requested_at = None;
    }
}

async fn maybe_request_inventory(
    network: &GossipNetwork,
    writer: &mut tokio::net::tcp::OwnedWriteHalf,
    envelope: &GossipEnvelope,
    requested_at: &mut Option<Instant>,
) -> Result<bool> {
    let GossipEnvelope::Inventory { blocks } = envelope else {
        return Ok(false);
    };
    if requested_at.is_some() {
        return Ok(true);
    }
    let request = network
        .inner
        .node
        .lock()
        .await
        .missing_inventory_request(blocks);
    if let Some(request) = request {
        write_envelope(writer, &request).await?;
        *requested_at = Some(Instant::now());
    }
    Ok(true)
}

async fn peer_is_connectable(network: &GossipNetwork, peer: &str) -> bool {
    network.inner.peers.lock().await.is_connectable_peer(peer)
}

async fn session_peer_is_banned(
    network: &GossipNetwork,
    known_peer: &Option<String>,
    remote_addr: SocketAddr,
) -> bool {
    let peer = known_peer
        .as_deref()
        .map(str::to_owned)
        .unwrap_or_else(|| remote_addr.to_string());
    network.inner.peers.lock().await.is_banned(&peer)
}

pub(super) fn next_reconnect_delay(current: Duration) -> Duration {
    next_reconnect_delay_with_max(current, MAX_RECONNECT_DELAY)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use tokio::time::Instant;

    use super::super::{INITIAL_RECONNECT_DELAY, MAX_RECONNECT_DELAY};
    use super::{next_reconnect_delay, update_catchup_request_state};

    #[test]
    fn reconnect_backoff_is_capped() {
        assert_eq!(
            next_reconnect_delay(INITIAL_RECONNECT_DELAY),
            Duration::from_secs(2)
        );
        assert_eq!(
            next_reconnect_delay(MAX_RECONNECT_DELAY),
            MAX_RECONNECT_DELAY
        );
    }

    #[test]
    fn already_applied_response_clears_in_flight_request() {
        let mut requested_at = Some(Instant::now());

        update_catchup_request_state(&mut requested_at, true, true, false);

        assert!(requested_at.is_none());
    }

    #[test]
    fn fork_recovery_request_is_marked_in_flight() {
        let mut requested_at = None;

        update_catchup_request_state(&mut requested_at, false, false, true);

        assert!(requested_at.is_some());
    }
}
