use std::{net::SocketAddr, time::Duration};

use anyhow::Result;
use tokio::{
    net::{TcpListener, TcpStream},
    sync::mpsc,
    time::{Instant, interval, interval_at, sleep, timeout},
};

use crate::app::{GossipEnvelope, debug_logging_enabled};

use super::{
    CONNECT_TIMEOUT, GossipNetwork, HANDSHAKE_TIMEOUT, INITIAL_RECONNECT_DELAY,
    MAX_RECONNECT_DELAY, PEER_EXCHANGE_INTERVAL, PEER_QUEUE_SIZE, PeerStatus,
    SESSION_SYNC_INTERVAL, is_self_peer_address_for, next_reconnect_delay_with_max,
    process_envelope, process_hello_with_verification, push_catchup_to_peer, read_session_envelope,
    record_peer_status, respond_to_peer_verification_challenge, write_envelope, write_payload,
    write_peer_exchange,
};

type OutboundBatch = Vec<GossipEnvelope>;

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
                tokio::spawn(async move {
                    let _permit = permit;
                    let result = session_loop(
                        network.clone(),
                        stream,
                        remote_addr,
                        None,
                        mpsc::channel(1).1,
                    )
                    .await;
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
        )
        .await;
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
        receiver = next_receiver;
        if !peer_is_connectable(&network, &peer).await {
            network.inner.sessions.lock().await.remove(&peer);
            return;
        }
        network
            .inner
            .sessions
            .lock()
            .await
            .insert(peer.clone(), sender);
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
    let is_outbound_session = stable_peer.is_some();
    let mut known_peer = stable_peer;

    if known_peer.is_some() {
        if let Ok(Ok(Some(envelope))) = timeout(
            HANDSHAKE_TIMEOUT,
            read_session_envelope(&network, &connection_label, &mut reader),
        )
        .await
        {
            if let GossipEnvelope::Hello(hello) = envelope {
                peer_status = Some(
                    process_hello_with_verification(
                        &network,
                        &mut writer,
                        &mut reader,
                        &connection_label,
                        remote_addr,
                        &mut known_peer,
                        hello,
                    )
                    .await?,
                );
                if is_outbound_session && known_peer.is_none() {
                    return Ok(());
                }
                super::maybe_request_catchup(&network, &mut writer, peer_status.as_ref().unwrap())
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
                super::maybe_request_catchup(&network, &mut writer, peer_status.as_ref().unwrap())
                    .await?;
                write_peer_exchange(&network, &mut writer, &known_peer).await?;
            } else if respond_to_peer_verification_challenge(&network, &mut writer, &envelope)
                .await?
            {
                if known_peer.is_none() {
                    return Ok(());
                }
            } else {
                process_envelope(
                    &network,
                    &mut writer,
                    remote_addr,
                    &mut known_peer,
                    envelope,
                )
                .await?;
                if is_outbound_session && known_peer.is_none() {
                    return Ok(());
                }
            }
        }
    }

    loop {
        tokio::select! {
            maybe_batch = outbound.recv(), if !outbound_closed => {
                match maybe_batch {
                    Some(batch) => {
                        let payload = super::envelopes_for_peer(
                            Some(&network.inner.node),
                            peer_status.clone(),
                            &batch,
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
                if let Some(status) = peer_status.as_mut() {
                    if let Some(updated_status) = push_catchup_to_peer(&network, &mut writer, status).await? {
                        *status = updated_status;
                    }
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
                    peer_status = Some(
                        process_hello_with_verification(
                            &network,
                            &mut writer,
                            &mut reader,
                            &connection_label,
                            remote_addr,
                            &mut known_peer,
                            hello,
                        )
                        .await?,
                    );
                    if is_outbound_session && known_peer.is_none() {
                        return Ok(());
                    }
                    super::maybe_request_catchup(&network, &mut writer, peer_status.as_ref().unwrap()).await?;
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
                    super::maybe_request_catchup(&network, &mut writer, peer_status.as_ref().unwrap()).await?;
                    write_peer_exchange(&network, &mut writer, &known_peer).await?;
                    continue;
                }

                if respond_to_peer_verification_challenge(&network, &mut writer, &envelope).await? {
                    if known_peer.is_none() {
                        return Ok(());
                    }
                    continue;
                }
                process_envelope(
                    &network,
                    &mut writer,
                    remote_addr,
                    &mut known_peer,
                    envelope,
                ).await?;
                if is_outbound_session && known_peer.is_none() {
                    return Ok(());
                }
            }
        }
    }
}

async fn peer_is_connectable(network: &GossipNetwork, peer: &str) -> bool {
    network.inner.peers.lock().await.is_connectable_peer(peer)
}

pub(super) fn next_reconnect_delay(current: Duration) -> Duration {
    next_reconnect_delay_with_max(current, MAX_RECONNECT_DELAY)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::super::{INITIAL_RECONNECT_DELAY, MAX_RECONNECT_DELAY};
    use super::next_reconnect_delay;

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
}
