use std::{
    net::{IpAddr, SocketAddr},
    str::FromStr,
    sync::Arc,
};

use anyhow::{Context, Result, bail};
use axum::http::{HeaderMap, Method, Uri, header, uri::Authority};
use secrecy::{ExposeSecret, SecretString};

use crate::{
    adapters::{config_store, wallet_store},
    domain::Wallet,
};

use super::{
    AUTH_COOKIE_NAME, AUTH_LOCKOUT_MS, AUTH_MAX_FAILED_ATTEMPTS, AUTH_SESSION_TTL_MS, AuthSession,
    HttpState, SETUP_COOKIE_NAME, SETUP_COOKIE_TTL_SECS, UNKNOWN_CLIENT_KEY,
    auth::{hash_password, random_hex, session_token_hash, validate_password, verify_password},
    now_ms,
};

pub(super) fn auth_exempt_path(path: &str) -> bool {
    path == "/"
        || path == "/favicon.ico"
        || path == "/assets/alpine.min.js"
        || path == "/assets/iuna-ui.js"
        || path == "/api/auth/status"
        || path == "/api/auth/setup"
        || path == "/api/auth/login"
}

pub(super) fn csrf_required(method: &Method) -> bool {
    !matches!(method, &Method::GET | &Method::HEAD | &Method::OPTIONS)
}

pub(super) fn same_origin_request(headers: &HeaderMap, socket_addr: Option<SocketAddr>) -> bool {
    let Some(request_host) = request_host(headers, socket_addr) else {
        return false;
    };
    let Some(origin_host) = origin_or_referer_host(headers) else {
        return false;
    };
    normalize_host(&origin_host) == normalize_host(&request_host)
}

pub(super) fn local_setup_page_request(
    headers: &HeaderMap,
    socket_addr: Option<SocketAddr>,
    management_port: u16,
) -> bool {
    socket_addr.is_some_and(|addr| addr.ip().is_loopback())
        && raw_request_local_authority(headers, management_port).is_some()
}

pub(super) fn local_setup_request(
    headers: &HeaderMap,
    socket_addr: Option<SocketAddr>,
    management_port: u16,
) -> bool {
    if !local_setup_page_request(headers, socket_addr, management_port) {
        return false;
    }
    let Some(request_authority) = raw_request_local_authority(headers, management_port) else {
        return false;
    };
    let Some(origin_authority) = local_origin_authority(headers, management_port) else {
        return false;
    };
    request_authority == origin_authority
}

fn raw_request_local_authority(headers: &HeaderMap, management_port: u16) -> Option<(String, u16)> {
    let host = header_string(headers, "host")?;
    parse_local_authority(&host, management_port)
}

fn local_origin_authority(headers: &HeaderMap, management_port: u16) -> Option<(String, u16)> {
    let value = header_string(headers, "origin").or_else(|| header_string(headers, "referer"))?;
    let uri = Uri::from_str(&value).ok()?;
    if uri.scheme_str() != Some("http") {
        return None;
    }
    parse_local_authority(uri.authority()?.as_str(), management_port)
}

fn parse_local_authority(value: &str, management_port: u16) -> Option<(String, u16)> {
    let authority = Authority::from_str(value.trim()).ok()?;
    let port = authority.port_u16()?;
    if port != management_port {
        return None;
    }
    let host = authority
        .host()
        .trim_matches(['[', ']'])
        .trim_end_matches('.')
        .to_ascii_lowercase();
    if host == "localhost" {
        return Some((host, port));
    }
    let ip = IpAddr::from_str(&host).ok()?;
    ip.is_loopback().then(|| (ip.to_string(), port))
}

fn request_host(headers: &HeaderMap, socket_addr: Option<SocketAddr>) -> Option<String> {
    let forwarded_host = socket_addr
        .filter(|addr| trusted_forwarding_peer(addr.ip()))
        .and_then(|_| header_string(headers, "x-forwarded-host"));
    forwarded_host.or_else(|| header_string(headers, "host"))
}

fn origin_or_referer_host(headers: &HeaderMap) -> Option<String> {
    header_string(headers, "origin")
        .and_then(|origin| url_host(&origin))
        .or_else(|| header_string(headers, "referer").and_then(|referer| url_host(&referer)))
}

fn header_string(headers: &HeaderMap, name: &'static str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn url_host(value: &str) -> Option<String> {
    let (_, rest) = value.split_once("://")?;
    rest.split(['/', '?', '#'])
        .next()
        .map(str::trim)
        .filter(|authority| !authority.is_empty() && *authority != "null")
        .map(|authority| {
            authority
                .rsplit('@')
                .next()
                .unwrap_or(authority)
                .to_string()
        })
}

fn normalize_host(host: &str) -> String {
    host.trim().trim_end_matches('.').to_ascii_lowercase()
}

pub(super) async fn request_is_authenticated(state: &HttpState, headers: &HeaderMap) -> bool {
    let Some(token) = auth_cookie(headers) else {
        return false;
    };
    let token_hash = session_token_hash(token);
    let now = now_ms();
    let mut sessions = state.auth_sessions.lock().await;
    sessions.retain(|_, session| session.expires_at > now);
    sessions
        .get(&token_hash)
        .is_some_and(|session| session.expires_at > now)
}

pub(super) async fn setup_capability_cookie(state: &HttpState) -> Option<String> {
    state.setup_capability.lock().await.as_ref().map(|token| {
        format!(
            "{SETUP_COOKIE_NAME}={}; Path=/api/auth/setup; HttpOnly; SameSite=Strict; Max-Age={SETUP_COOKIE_TTL_SECS}",
            token.expose_secret()
        )
    })
}

pub(super) async fn validate_setup_capability(
    state: &HttpState,
    headers: &HeaderMap,
) -> Result<()> {
    let supplied = named_cookie(headers, SETUP_COOKIE_NAME)
        .context("local password setup capability is required")?;
    let capability = state.setup_capability.lock().await;
    let expected = capability
        .as_ref()
        .context("local password setup capability is no longer available")?;
    if session_token_hash(supplied) != session_token_hash(expected.expose_secret()) {
        bail!("local password setup capability is invalid");
    }
    Ok(())
}

pub(super) async fn consume_setup_capability(state: &HttpState) {
    state.setup_capability.lock().await.take();
}

pub(super) async fn wallet_password_for_request(
    state: &HttpState,
    headers: &HeaderMap,
) -> Option<Arc<SecretString>> {
    let token = auth_cookie(headers)?;
    let token_hash = session_token_hash(token);
    let now = now_ms();
    let mut sessions = state.auth_sessions.lock().await;
    sessions.retain(|_, session| session.expires_at > now);
    sessions
        .get(&token_hash)
        .filter(|session| session.expires_at > now)
        .map(|session| session.wallet_password.clone())
}

pub(super) fn auth_client_key(headers: &HeaderMap, socket_addr: Option<SocketAddr>) -> String {
    if let Some(addr) = socket_addr {
        if !trusted_forwarding_peer(addr.ip()) {
            return addr.ip().to_string();
        }
    }
    forwarded_for_client(headers)
        .or_else(|| header_string(headers, "x-real-ip"))
        .or_else(|| forwarded_header_client(headers))
        .or_else(|| socket_addr.map(|addr| addr.ip().to_string()))
        .unwrap_or_else(|| UNKNOWN_CLIENT_KEY.to_string())
}

fn trusted_forwarding_peer(ip: std::net::IpAddr) -> bool {
    ip.is_loopback()
}

fn forwarded_for_client(headers: &HeaderMap) -> Option<String> {
    header_string(headers, "x-forwarded-for").and_then(|value| {
        value
            .split(',')
            .next()
            .map(str::trim)
            .filter(|client| !client.is_empty())
            .map(ToOwned::to_owned)
    })
}

fn forwarded_header_client(headers: &HeaderMap) -> Option<String> {
    let value = header_string(headers, "forwarded")?;
    for item in value.split(';') {
        let Some((name, value)) = item.split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("for") {
            return Some(
                value
                    .trim()
                    .trim_matches('"')
                    .trim_matches('[')
                    .trim_matches(']')
                    .to_string(),
            )
            .filter(|client| !client.is_empty());
        }
    }
    None
}

pub(super) async fn setup_auth_password(
    state: &HttpState,
    password: SecretString,
    client_key: &str,
) -> Result<String> {
    let exposed_password = password.expose_secret();
    check_auth_backoff(state, client_key).await?;
    if let Err(error) = validate_password(exposed_password) {
        record_auth_failure(state, client_key).await;
        return Err(error);
    }
    let mut config = state.ui_config.lock().await;
    if config.auth_password_hash.is_some() {
        record_auth_failure(state, client_key).await;
        bail!("authentication is already configured");
    }
    config.auth_password_hash = Some(hash_password(exposed_password)?);
    config_store::save(&state.config_path, &config)?;
    drop(config);
    wallet_store::encrypt_existing_with_password(&state.wallet_path, exposed_password)?;
    let wallet = wallet_store::load_with_password(&state.wallet_path, exposed_password)?;
    restore_node_wallet_from_store(state, wallet, Some(exposed_password)).await?;
    clear_auth_backoff(state, client_key).await;
    create_session_cookie(state, password).await
}

pub(super) async fn login_auth_password(
    state: &HttpState,
    password: SecretString,
    client_key: &str,
) -> Result<String> {
    let exposed_password = password.expose_secret();
    check_auth_backoff(state, client_key).await?;
    let hash = state
        .ui_config
        .lock()
        .await
        .auth_password_hash
        .clone()
        .context("authentication setup is required")?;
    if !verify_password(exposed_password, &hash)? {
        record_auth_failure(state, client_key).await;
        bail!("invalid password");
    }
    wallet_store::encrypt_existing_with_password(&state.wallet_path, exposed_password)?;
    let wallet = wallet_store::load_with_password(&state.wallet_path, exposed_password)?;
    restore_node_wallet_from_store(state, wallet, Some(exposed_password)).await?;
    clear_auth_backoff(state, client_key).await;
    create_session_cookie(state, password).await
}

pub(super) async fn change_auth_password(
    state: &HttpState,
    old_password: SecretString,
    new_password: SecretString,
    client_key: &str,
) -> Result<String> {
    let exposed_old_password = old_password.expose_secret();
    let exposed_new_password = new_password.expose_secret();
    check_auth_backoff(state, client_key).await?;
    validate_password(exposed_new_password)?;
    let current_hash = state
        .ui_config
        .lock()
        .await
        .auth_password_hash
        .clone()
        .context("authentication setup is required")?;
    if !verify_password(exposed_old_password, &current_hash)? {
        record_auth_failure(state, client_key).await;
        bail!("invalid current password");
    }
    let wallet = wallet_store::reencrypt_with_password(
        &state.wallet_path,
        exposed_old_password,
        exposed_new_password,
    )?;
    {
        let mut config = state.ui_config.lock().await;
        config.auth_password_hash = Some(hash_password(exposed_new_password)?);
        config_store::save(&state.config_path, &config)?;
    }
    restore_node_wallet_from_store(state, wallet, Some(exposed_new_password)).await?;
    state.auth_sessions.lock().await.clear();
    clear_auth_backoff(state, client_key).await;
    create_session_cookie(state, new_password).await
}

pub(super) async fn restore_node_wallet_from_store(
    state: &HttpState,
    wallet: Wallet,
    _password: Option<&str>,
) -> Result<()> {
    let mut node = state.node.lock().await;
    node.replace_wallet(wallet);
    Ok(())
}

async fn check_auth_backoff(state: &HttpState, client_key: &str) -> Result<()> {
    let now = now_ms();
    let mut backoffs = state.auth_backoff.lock().await;
    let backoff = backoffs.entry(client_key.to_string()).or_default();
    if backoff
        .locked_until_ms
        .is_some_and(|locked_until| locked_until > now)
    {
        bail!("too many failed login attempts; try again later");
    }
    if backoff.locked_until_ms.is_some() {
        backoff.locked_until_ms = None;
        backoff.failed_attempts = 0;
    }
    Ok(())
}

async fn record_auth_failure(state: &HttpState, client_key: &str) {
    let mut backoffs = state.auth_backoff.lock().await;
    let backoff = backoffs.entry(client_key.to_string()).or_default();
    backoff.failed_attempts = backoff.failed_attempts.saturating_add(1);
    if backoff.failed_attempts >= AUTH_MAX_FAILED_ATTEMPTS {
        backoff.locked_until_ms = Some(now_ms().saturating_add(AUTH_LOCKOUT_MS));
    }
}

async fn clear_auth_backoff(state: &HttpState, client_key: &str) {
    state.auth_backoff.lock().await.remove(client_key);
}

async fn create_session_cookie(state: &HttpState, password: SecretString) -> Result<String> {
    let token = random_hex(32)?;
    let token_hash = session_token_hash(token.expose_secret());
    let expires_at = now_ms().saturating_add(AUTH_SESSION_TTL_MS);
    state.auth_sessions.lock().await.insert(
        token_hash,
        AuthSession {
            expires_at,
            wallet_password: Arc::new(password),
        },
    );
    Ok(format!(
        "{AUTH_COOKIE_NAME}={}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
        token.expose_secret(),
        AUTH_SESSION_TTL_MS / 1000
    ))
}

pub(super) fn auth_cookie(headers: &HeaderMap) -> Option<&str> {
    named_cookie(headers, AUTH_COOKIE_NAME)
}

fn named_cookie<'a>(headers: &'a HeaderMap, expected_name: &str) -> Option<&'a str> {
    let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    cookie.split(';').find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        (name == expected_name).then_some(value)
    })
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
        sync::Arc,
    };

    use axum::http::{HeaderMap, HeaderValue, header};
    use tokio::sync::Mutex;

    use crate::{
        adapters::{
            chain_store::SqliteChainStore, config_store::UiConfig, p2p::GossipNetwork,
            ui_data_store::SqliteUiDataStore,
        },
        app::{NodeCore, PeerBook, StratumStatus},
        domain::{GenesisBurn, Ledger, MICRO_IUNA, Wallet},
    };

    use super::super::state::{AuthSession, HttpState};
    use super::{AUTH_COOKIE_NAME, now_ms};
    use super::{
        auth_client_key, check_auth_backoff, consume_setup_capability, local_setup_page_request,
        local_setup_request, record_auth_failure, request_is_authenticated, same_origin_request,
        session_token_hash, setup_capability_cookie, validate_setup_capability,
    };

    fn headers(values: &[(&'static str, &'static str)]) -> HeaderMap {
        let mut headers = HeaderMap::new();
        for (name, value) in values {
            headers.insert(*name, HeaderValue::from_static(value));
        }
        headers
    }

    fn socket(ip: [u8; 4]) -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::from(ip)), 9444)
    }

    fn ipv6_loopback_socket() -> SocketAddr {
        SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 9444)
    }

    async fn test_state() -> HttpState {
        let dir = tempfile::tempdir().unwrap().keep();
        let wallet = Wallet::from_seed("http-auth-abuse-tests");
        let mut allocations = BTreeMap::new();
        allocations.insert(wallet.address().to_string(), 10 * MICRO_IUNA);
        let ledger = Ledger::new_with_genesis_burns(
            allocations,
            vec![GenesisBurn::new(wallet.address(), 1)],
            1,
        )
        .unwrap();
        let node = Arc::new(Mutex::new(NodeCore::from_ledger(wallet, ledger, 0)));
        let peers = Arc::new(Mutex::new(PeerBook::default()));
        let gossip = GossipNetwork::start(
            node.clone(),
            peers.clone(),
            socket([127, 0, 0, 1]),
            None,
            false,
        )
        .await
        .unwrap();

        HttpState {
            node,
            peers,
            gossip,
            ui_config: Arc::new(Mutex::new(UiConfig::default())),
            config_path: dir.join("config.json"),
            chain_store: SqliteChainStore::open(dir.join("chain.sqlite")).unwrap(),
            ui_data_store: SqliteUiDataStore::open(dir.join("ui.sqlite")).unwrap(),
            wallet_path: dir.join("wallet.json"),
            stratum: StratumStatus {
                enabled: false,
                listen_addr: None,
            },
            auth_sessions: Arc::new(Mutex::new(BTreeMap::new())),
            auth_backoff: Arc::new(Mutex::new(BTreeMap::new())),
            setup_capability: Arc::new(Mutex::new(Some("test-setup-capability".into()))),
            management_port: 9444,
            wallet_endpoint_addr: None,
            ui_data_ready: Arc::new(std::sync::atomic::AtomicBool::new(true)),
        }
    }

    #[test]
    fn csrf_same_origin_requires_matching_origin_or_referer_host() {
        assert!(same_origin_request(
            &headers(&[
                ("host", "127.0.0.1:9444"),
                ("origin", "http://127.0.0.1:9444")
            ]),
            None
        ));
        assert!(same_origin_request(
            &headers(&[
                ("host", "iuna.local:9444"),
                ("referer", "http://iuna.local:9444/settings")
            ]),
            None
        ));
        assert!(!same_origin_request(
            &headers(&[
                ("host", "127.0.0.1:9444"),
                ("origin", "https://evil.example")
            ]),
            None
        ));
        assert!(!same_origin_request(
            &headers(&[("host", "127.0.0.1:9444")]),
            None
        ));
    }

    #[test]
    fn local_setup_accepts_only_exact_loopback_management_origins() {
        assert!(local_setup_page_request(
            &headers(&[("host", "127.0.0.1:9444")]),
            Some(socket([127, 0, 0, 1])),
            9444
        ));
        assert!(local_setup_request(
            &headers(&[
                ("host", "127.0.0.1:9444"),
                ("origin", "http://127.0.0.1:9444")
            ]),
            Some(socket([127, 0, 0, 1])),
            9444
        ));
        assert!(local_setup_request(
            &headers(&[
                ("host", "localhost:9444"),
                ("referer", "http://localhost:9444/setup")
            ]),
            Some(socket([127, 0, 0, 1])),
            9444
        ));
        assert!(local_setup_request(
            &headers(&[("host", "[::1]:9444"), ("origin", "http://[::1]:9444")]),
            Some(ipv6_loopback_socket()),
            9444
        ));
    }

    #[test]
    fn local_setup_rejects_dns_rebinding_and_remote_requests() {
        assert!(!local_setup_request(
            &headers(&[
                ("host", "rebound.evil:9444"),
                ("origin", "http://rebound.evil:9444")
            ]),
            Some(socket([127, 0, 0, 1])),
            9444
        ));
        assert!(!local_setup_request(
            &headers(&[
                ("host", "127.0.0.1:9444"),
                ("origin", "http://localhost:9444")
            ]),
            Some(socket([127, 0, 0, 1])),
            9444
        ));
        assert!(!local_setup_request(
            &headers(&[
                ("host", "127.0.0.1:9444"),
                ("origin", "https://127.0.0.1:9444")
            ]),
            Some(socket([127, 0, 0, 1])),
            9444
        ));
        assert!(!local_setup_request(
            &headers(&[
                ("host", "127.0.0.1:9444"),
                ("origin", "http://127.0.0.1:9444")
            ]),
            Some(socket([203, 0, 113, 10])),
            9444
        ));
        assert!(!local_setup_request(
            &headers(&[
                ("host", "127.0.0.1:9555"),
                ("origin", "http://127.0.0.1:9555")
            ]),
            Some(socket([127, 0, 0, 1])),
            9444
        ));
        assert!(!local_setup_request(
            &headers(&[
                ("host", "rebound.evil:9444"),
                ("x-forwarded-host", "127.0.0.1:9444"),
                ("origin", "http://127.0.0.1:9444")
            ]),
            Some(socket([127, 0, 0, 1])),
            9444
        ));
    }

    #[tokio::test]
    async fn setup_capability_cookie_is_required_and_one_time() {
        let state = test_state().await;
        let cookie = setup_capability_cookie(&state).await.unwrap();
        assert!(cookie.starts_with("iuna_setup=test-setup-capability;"));
        assert!(cookie.contains("Path=/api/auth/setup"));
        assert!(cookie.contains("HttpOnly"));
        assert!(cookie.contains("SameSite=Strict"));

        assert!(
            validate_setup_capability(&state, &HeaderMap::new())
                .await
                .is_err()
        );
        assert!(
            validate_setup_capability(
                &state,
                &headers(&[("cookie", "iuna_setup=wrong-capability")])
            )
            .await
            .is_err()
        );
        assert!(
            validate_setup_capability(
                &state,
                &headers(&[("cookie", "other=value; iuna_setup=test-setup-capability")])
            )
            .await
            .is_ok()
        );

        consume_setup_capability(&state).await;
        assert!(
            validate_setup_capability(
                &state,
                &headers(&[("cookie", "iuna_setup=test-setup-capability")])
            )
            .await
            .is_err()
        );
        assert!(setup_capability_cookie(&state).await.is_none());
    }

    #[test]
    fn csrf_ignores_forwarded_host_from_untrusted_peer() {
        let headers = headers(&[
            ("host", "127.0.0.1:9444"),
            ("x-forwarded-host", "evil.example"),
            ("origin", "https://evil.example"),
        ]);

        assert!(!same_origin_request(
            &headers,
            Some(socket([203, 0, 113, 10]))
        ));
        assert!(!same_origin_request(
            &headers,
            Some(socket([192, 168, 1, 10]))
        ));
    }

    #[test]
    fn csrf_accepts_forwarded_host_from_trusted_proxy() {
        let headers = headers(&[
            ("host", "127.0.0.1:9444"),
            ("x-forwarded-host", "iuna.example"),
            ("origin", "https://iuna.example"),
        ]);

        assert!(same_origin_request(&headers, Some(socket([127, 0, 0, 1]))));
    }

    #[test]
    fn auth_client_key_ignores_forwarded_client_from_untrusted_peer() {
        let headers = headers(&[
            ("x-forwarded-for", "198.51.100.50"),
            ("x-real-ip", "198.51.100.51"),
            ("forwarded", "for=198.51.100.52"),
        ]);

        assert_eq!(
            auth_client_key(&headers, Some(socket([203, 0, 113, 10]))),
            "203.0.113.10"
        );
        assert_eq!(
            auth_client_key(&headers, Some(socket([192, 168, 1, 10]))),
            "192.168.1.10"
        );
        assert_eq!(
            auth_client_key(&headers, Some(socket([127, 0, 0, 1]))),
            "198.51.100.50"
        );
    }

    #[tokio::test]
    async fn auth_backoff_locks_out_and_resets_after_expiry() {
        let state = test_state().await;
        let client_key = "client-a";
        for _ in 0..super::AUTH_MAX_FAILED_ATTEMPTS {
            record_auth_failure(&state, client_key).await;
        }

        assert!(check_auth_backoff(&state, client_key).await.is_err());

        {
            let mut backoffs = state.auth_backoff.lock().await;
            let backoff = backoffs.get_mut(client_key).unwrap();
            backoff.locked_until_ms = Some(now_ms().saturating_sub(1));
        }

        assert!(check_auth_backoff(&state, client_key).await.is_ok());
        let backoffs = state.auth_backoff.lock().await;
        let backoff = backoffs.get(client_key).unwrap();
        assert_eq!(backoff.failed_attempts, 0);
        assert_eq!(backoff.locked_until_ms, None);
    }

    #[tokio::test]
    async fn expired_sessions_are_rejected_and_pruned() {
        let state = test_state().await;
        let expired_token = "expired";
        let live_token = "live";
        state.auth_sessions.lock().await.insert(
            session_token_hash(expired_token),
            AuthSession {
                expires_at: now_ms().saturating_sub(1),
                wallet_password: Arc::new("expired-password".into()),
            },
        );
        state.auth_sessions.lock().await.insert(
            session_token_hash(live_token),
            AuthSession {
                expires_at: now_ms().saturating_add(60_000),
                wallet_password: Arc::new("live-password".into()),
            },
        );

        let expired_headers = headers(&[(header::COOKIE.as_str(), "iuna_session=expired")]);
        assert!(!request_is_authenticated(&state, &expired_headers).await);
        assert!(
            !state
                .auth_sessions
                .lock()
                .await
                .contains_key(&session_token_hash(expired_token))
        );

        let live_cookie = format!("{AUTH_COOKIE_NAME}={live_token}");
        let mut live_headers = HeaderMap::new();
        live_headers.insert(header::COOKIE, HeaderValue::from_str(&live_cookie).unwrap());
        assert!(request_is_authenticated(&state, &live_headers).await);
    }
}
