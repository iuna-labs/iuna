use std::net::SocketAddr;

use anyhow::{Context, Result, bail};
use axum::http::{HeaderMap, Method, header};

use crate::{
    adapters::{config_store, wallet_store},
    domain::Wallet,
};

use super::{
    AUTH_COOKIE_NAME, AUTH_LOCKOUT_MS, AUTH_MAX_FAILED_ATTEMPTS, AUTH_SESSION_TTL_MS, AuthSession,
    HttpState, UNKNOWN_CLIENT_KEY,
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

pub(super) fn same_origin_request(headers: &HeaderMap) -> bool {
    let Some(request_host) = request_host(headers) else {
        return false;
    };
    let Some(origin_host) = origin_or_referer_host(headers) else {
        return false;
    };
    normalize_host(&origin_host) == normalize_host(&request_host)
}

fn request_host(headers: &HeaderMap) -> Option<String> {
    header_string(headers, "x-forwarded-host").or_else(|| header_string(headers, "host"))
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

pub(super) async fn wallet_password_for_request(
    state: &HttpState,
    headers: &HeaderMap,
) -> Option<String> {
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
    match ip {
        std::net::IpAddr::V4(ip) => ip.is_loopback() || ip.is_private() || ip.is_link_local(),
        std::net::IpAddr::V6(ip) => {
            ip.is_loopback() || ipv6_is_unique_local(ip) || ipv6_is_unicast_link_local(ip)
        }
    }
}

fn ipv6_is_unique_local(ip: std::net::Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xfe00) == 0xfc00
}

fn ipv6_is_unicast_link_local(ip: std::net::Ipv6Addr) -> bool {
    (ip.segments()[0] & 0xffc0) == 0xfe80
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
    password: &str,
    client_key: &str,
) -> Result<String> {
    check_auth_backoff(state, client_key).await?;
    if let Err(error) = validate_password(password) {
        record_auth_failure(state, client_key).await;
        return Err(error);
    }
    let mut config = state.ui_config.lock().await;
    if config.auth_password_hash.is_some() {
        record_auth_failure(state, client_key).await;
        bail!("authentication is already configured");
    }
    config.auth_password_hash = Some(hash_password(password)?);
    config_store::save(&state.config_path, &config)?;
    drop(config);
    wallet_store::encrypt_existing_with_password(&state.wallet_path, password)?;
    let wallet = wallet_store::load_with_password(&state.wallet_path, password)?;
    restore_node_wallet_from_store(state, wallet, Some(password)).await?;
    clear_auth_backoff(state, client_key).await;
    create_session_cookie(state, password).await
}

pub(super) async fn login_auth_password(
    state: &HttpState,
    password: &str,
    client_key: &str,
) -> Result<String> {
    check_auth_backoff(state, client_key).await?;
    let hash = state
        .ui_config
        .lock()
        .await
        .auth_password_hash
        .clone()
        .context("authentication setup is required")?;
    if !verify_password(password, &hash)? {
        record_auth_failure(state, client_key).await;
        bail!("invalid password");
    }
    wallet_store::encrypt_existing_with_password(&state.wallet_path, password)?;
    let wallet = wallet_store::load_with_password(&state.wallet_path, password)?;
    restore_node_wallet_from_store(state, wallet, Some(password)).await?;
    clear_auth_backoff(state, client_key).await;
    create_session_cookie(state, password).await
}

pub(super) async fn change_auth_password(
    state: &HttpState,
    old_password: &str,
    new_password: &str,
    client_key: &str,
) -> Result<String> {
    check_auth_backoff(state, client_key).await?;
    validate_password(new_password)?;
    let current_hash = state
        .ui_config
        .lock()
        .await
        .auth_password_hash
        .clone()
        .context("authentication setup is required")?;
    if !verify_password(old_password, &current_hash)? {
        record_auth_failure(state, client_key).await;
        bail!("invalid current password");
    }
    let wallet =
        wallet_store::reencrypt_with_password(&state.wallet_path, old_password, new_password)?;
    {
        let mut config = state.ui_config.lock().await;
        config.auth_password_hash = Some(hash_password(new_password)?);
        config_store::save(&state.config_path, &config)?;
    }
    restore_node_wallet_from_store(state, wallet, Some(new_password)).await?;
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

async fn create_session_cookie(state: &HttpState, password: &str) -> Result<String> {
    let token = random_hex(32)?;
    let token_hash = session_token_hash(&token);
    let expires_at = now_ms().saturating_add(AUTH_SESSION_TTL_MS);
    state.auth_sessions.lock().await.insert(
        token_hash,
        AuthSession {
            expires_at,
            wallet_password: password.to_string(),
        },
    );
    Ok(format!(
        "{AUTH_COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={}",
        AUTH_SESSION_TTL_MS / 1000
    ))
}

pub(super) fn auth_cookie(headers: &HeaderMap) -> Option<&str> {
    let cookie = headers.get(header::COOKIE)?.to_str().ok()?;
    cookie.split(';').find_map(|part| {
        let (name, value) = part.trim().split_once('=')?;
        (name == AUTH_COOKIE_NAME).then_some(value)
    })
}
