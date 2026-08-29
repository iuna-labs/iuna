use std::net::SocketAddr;

use axum::{
    Form, Json,
    body::Body,
    extract::{ConnectInfo, Extension, State},
    http::{HeaderMap, Request, StatusCode, header},
    middleware::Next,
    response::{IntoResponse, Response},
};

use super::{
    AUTH_COOKIE_NAME, ActionResponse, AuthClientKey, AuthForm, AuthStatusResponse,
    ChangePasswordForm, HttpState, SETUP_COOKIE_NAME, action_json,
    auth::session_token_hash,
    request_auth::{
        auth_client_key, auth_cookie, auth_exempt_path, change_auth_password,
        consume_setup_capability, csrf_required, local_setup_page_request, local_setup_request,
        login_auth_password, request_is_authenticated, same_origin_request, setup_auth_password,
        setup_capability_cookie, validate_setup_capability,
    },
};

pub(super) async fn require_auth_middleware(
    State(state): State<HttpState>,
    headers: HeaderMap,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    let socket_addr = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0);
    if path == "/api/auth/setup"
        && !local_setup_request(&headers, socket_addr, state.management_port)
    {
        return setup_origin_error().into_response();
    }
    if csrf_required(request.method()) && !same_origin_request(&headers, socket_addr) {
        return csrf_error().into_response();
    }
    let issue_setup_cookie = path == "/"
        && request.method() == axum::http::Method::GET
        && local_setup_page_request(&headers, socket_addr, state.management_port)
        && state.ui_config.lock().await.auth_password_hash.is_none();
    let client_key = auth_client_key(&headers, socket_addr);
    request.extensions_mut().insert(AuthClientKey(client_key));
    if auth_exempt_path(&path) {
        let mut response = next.run(request).await;
        if issue_setup_cookie {
            if let Some(cookie) = setup_capability_cookie(&state).await {
                if let Ok(value) = cookie.parse() {
                    response.headers_mut().append(header::SET_COOKIE, value);
                }
            }
        }
        return response;
    }
    let configured = state.ui_config.lock().await.auth_password_hash.is_some();
    if !configured {
        return auth_error("authentication setup is required").into_response();
    }
    if request_is_authenticated(&state, &headers).await {
        return next.run(request).await;
    }
    auth_error("authentication required").into_response()
}

pub(super) async fn api_auth_status(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Json<AuthStatusResponse> {
    let configured = state.ui_config.lock().await.auth_password_hash.is_some();
    let authenticated = configured && request_is_authenticated(&state, &headers).await;
    Json(AuthStatusResponse {
        configured,
        authenticated,
    })
}

pub(super) async fn api_auth_setup_form(
    State(state): State<HttpState>,
    Extension(client_key): Extension<AuthClientKey>,
    headers: HeaderMap,
    Form(form): Form<AuthForm>,
) -> Response {
    if let Err(error) = validate_setup_capability(&state, &headers).await {
        return (StatusCode::FORBIDDEN, action_json(Err(error))).into_response();
    }
    match setup_auth_password(&state, &form.password, &client_key.0).await {
        Ok(cookie) => {
            consume_setup_capability(&state).await;
            let mut response = action_json(Ok(())).into_response();
            if let Ok(value) = cookie.parse() {
                response.headers_mut().append(header::SET_COOKIE, value);
            }
            if let Ok(value) = format!(
                "{SETUP_COOKIE_NAME}=; Path=/api/auth/setup; HttpOnly; SameSite=Strict; Max-Age=0"
            )
            .parse()
            {
                response.headers_mut().append(header::SET_COOKIE, value);
            }
            response
        }
        Err(error) => action_json(Err(error)).into_response(),
    }
}

pub(super) async fn api_auth_login_form(
    State(state): State<HttpState>,
    Extension(client_key): Extension<AuthClientKey>,
    Form(form): Form<AuthForm>,
) -> Response {
    match login_auth_password(&state, &form.password, &client_key.0).await {
        Ok(cookie) => ([(header::SET_COOKIE, cookie)], action_json(Ok(()))).into_response(),
        Err(error) => action_json(Err(error)).into_response(),
    }
}

pub(super) async fn api_auth_logout_form(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Response {
    if let Some(token) = auth_cookie(&headers) {
        state
            .auth_sessions
            .lock()
            .await
            .remove(&session_token_hash(token));
    }
    (
        [(
            header::SET_COOKIE,
            format!("{AUTH_COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0"),
        )],
        action_json(Ok(())),
    )
        .into_response()
}

pub(super) async fn api_auth_change_password_form(
    State(state): State<HttpState>,
    Extension(client_key): Extension<AuthClientKey>,
    Form(form): Form<ChangePasswordForm>,
) -> Response {
    match change_auth_password(
        &state,
        &form.old_password,
        &form.new_password,
        &client_key.0,
    )
    .await
    {
        Ok(cookie) => ([(header::SET_COOKIE, cookie)], action_json(Ok(()))).into_response(),
        Err(error) => action_json(Err(error)).into_response(),
    }
}

fn auth_error(message: &str) -> (StatusCode, Json<ActionResponse>) {
    (
        StatusCode::UNAUTHORIZED,
        Json(ActionResponse {
            ok: false,
            error: Some(message.to_string()),
        }),
    )
}

fn csrf_error() -> (StatusCode, Json<ActionResponse>) {
    (
        StatusCode::FORBIDDEN,
        Json(ActionResponse {
            ok: false,
            error: Some("same-origin request required".to_string()),
        }),
    )
}

fn setup_origin_error() -> (StatusCode, Json<ActionResponse>) {
    (
        StatusCode::FORBIDDEN,
        Json(ActionResponse {
            ok: false,
            error: Some(
                "password setup is only available from the local management origin".to_string(),
            ),
        }),
    )
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        net::{IpAddr, Ipv4Addr, SocketAddr},
        sync::Arc,
    };

    use axum::{
        Router,
        body::{Body, to_bytes},
        extract::ConnectInfo,
        http::{Method, Request, StatusCode, header},
        middleware,
        routing::{get, post},
    };
    use tokio::sync::Mutex;
    use tower::ServiceExt;

    use crate::{
        adapters::{
            chain_store::SqliteChainStore, config_store::UiConfig, p2p::GossipNetwork,
            ui_data_store::SqliteUiDataStore, wallet_store,
        },
        app::{NodeCore, PeerBook, StratumStatus},
        domain::{GenesisBurn, Ledger, MICRO_IUNA},
    };

    use super::super::state::{AuthBackoff, AuthSession, HttpState};
    use super::{api_auth_setup_form, api_auth_status, require_auth_middleware};

    const MANAGEMENT_PORT: u16 = 18_661;
    const SETUP_CAPABILITY: &str = "test-setup-capability";
    const TEST_SEED: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon art";

    fn loopback_peer() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 51_234)
    }

    fn remote_peer() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(203, 0, 113, 10)), 51_234)
    }

    fn request(
        method: Method,
        uri: &str,
        headers: &[(&str, &str)],
        body: &str,
        peer: SocketAddr,
    ) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(uri);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let mut request = builder.body(Body::from(body.to_string())).unwrap();
        request.extensions_mut().insert(ConnectInfo(peer));
        request
    }

    async fn test_state() -> HttpState {
        let dir = tempfile::tempdir().unwrap().keep();
        let wallet_path = dir.join("wallet.json");
        let wallet =
            wallet_store::replace_with_imported_seed_phrase(&wallet_path, TEST_SEED).unwrap();
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
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
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
            wallet_path,
            stratum: StratumStatus {
                enabled: false,
                listen_addr: None,
            },
            auth_sessions: Arc::new(Mutex::new(BTreeMap::<String, AuthSession>::new())),
            auth_backoff: Arc::new(Mutex::new(BTreeMap::<String, AuthBackoff>::new())),
            setup_capability: Arc::new(Mutex::new(Some(SETUP_CAPABILITY.to_string()))),
            management_port: MANAGEMENT_PORT,
        }
    }

    fn test_app(state: HttpState) -> Router {
        Router::new()
            .route("/", get(|| async { "index" }))
            .route("/api/auth/setup", post(api_auth_setup_form))
            .route("/api/auth/status", get(api_auth_status))
            .layer(middleware::from_fn_with_state(
                state.clone(),
                require_auth_middleware,
            ))
            .with_state(state)
    }

    #[tokio::test]
    async fn setup_route_requires_local_origin_and_one_time_capability() {
        let state = test_state().await;
        let app = test_app(state.clone());
        let local_origin = format!("http://127.0.0.1:{MANAGEMENT_PORT}");
        let local_host = format!("127.0.0.1:{MANAGEMENT_PORT}");

        let page = app
            .clone()
            .oneshot(request(
                Method::GET,
                "/",
                &[(header::HOST.as_str(), &local_host)],
                "",
                loopback_peer(),
            ))
            .await
            .unwrap();
        assert_eq!(page.status(), StatusCode::OK);
        let setup_cookie = page
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|value| value.starts_with("iuna_setup="))
            .and_then(|value| value.split(';').next())
            .unwrap()
            .to_string();

        let rebound = app
            .clone()
            .oneshot(request(
                Method::POST,
                "/api/auth/setup",
                &[
                    (header::HOST.as_str(), "rebound.evil:18661"),
                    (header::ORIGIN.as_str(), "http://rebound.evil:18661"),
                    (
                        header::CONTENT_TYPE.as_str(),
                        "application/x-www-form-urlencoded",
                    ),
                    (header::COOKIE.as_str(), &setup_cookie),
                ],
                "password=correct-horse-battery-staple",
                loopback_peer(),
            ))
            .await
            .unwrap();
        assert_eq!(rebound.status(), StatusCode::FORBIDDEN);
        assert!(state.ui_config.lock().await.auth_password_hash.is_none());

        let remote = app
            .clone()
            .oneshot(request(
                Method::POST,
                "/api/auth/setup",
                &[
                    (header::HOST.as_str(), &local_host),
                    (header::ORIGIN.as_str(), &local_origin),
                    (
                        header::CONTENT_TYPE.as_str(),
                        "application/x-www-form-urlencoded",
                    ),
                    (header::COOKIE.as_str(), &setup_cookie),
                ],
                "password=correct-horse-battery-staple",
                remote_peer(),
            ))
            .await
            .unwrap();
        assert_eq!(remote.status(), StatusCode::FORBIDDEN);

        let missing_capability = app
            .clone()
            .oneshot(request(
                Method::POST,
                "/api/auth/setup",
                &[
                    (header::HOST.as_str(), &local_host),
                    (header::ORIGIN.as_str(), &local_origin),
                    (
                        header::CONTENT_TYPE.as_str(),
                        "application/x-www-form-urlencoded",
                    ),
                ],
                "password=correct-horse-battery-staple",
                loopback_peer(),
            ))
            .await
            .unwrap();
        assert_eq!(missing_capability.status(), StatusCode::FORBIDDEN);

        let success = app
            .clone()
            .oneshot(request(
                Method::POST,
                "/api/auth/setup",
                &[
                    (header::HOST.as_str(), &local_host),
                    (header::ORIGIN.as_str(), &local_origin),
                    (
                        header::CONTENT_TYPE.as_str(),
                        "application/x-www-form-urlencoded",
                    ),
                    (header::COOKIE.as_str(), &setup_cookie),
                ],
                "password=correct-horse-battery-staple",
                loopback_peer(),
            ))
            .await
            .unwrap();
        assert_eq!(success.status(), StatusCode::OK);
        let response_cookies = success
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|value| value.to_str().unwrap().to_string())
            .collect::<Vec<_>>();
        assert!(
            response_cookies
                .iter()
                .any(|cookie| cookie.starts_with("iuna_session="))
        );
        assert!(
            response_cookies.iter().any(|cookie| {
                cookie.starts_with("iuna_setup=") && cookie.contains("Max-Age=0")
            })
        );
        let success_body = to_bytes(success.into_body(), 16 * 1024).await.unwrap();
        let success_body = String::from_utf8(success_body.to_vec()).unwrap();
        assert!(success_body.contains("\"ok\":true"));
        assert!(!success_body.contains("abandon"));
        assert!(state.ui_config.lock().await.auth_password_hash.is_some());
        assert!(
            wallet_store::metadata(&state.wallet_path)
                .unwrap()
                .unwrap()
                .encrypted
        );
        assert!(state.setup_capability.lock().await.is_none());

        let session_cookie = response_cookies
            .iter()
            .find(|cookie| cookie.starts_with("iuna_session="))
            .and_then(|cookie| cookie.split(';').next())
            .unwrap();
        let status = app
            .clone()
            .oneshot(request(
                Method::GET,
                "/api/auth/status",
                &[
                    (header::HOST.as_str(), &local_host),
                    (header::COOKIE.as_str(), session_cookie),
                ],
                "",
                loopback_peer(),
            ))
            .await
            .unwrap();
        assert_eq!(status.status(), StatusCode::OK);
        let status_body = to_bytes(status.into_body(), 16 * 1024).await.unwrap();
        let status_body = String::from_utf8(status_body.to_vec()).unwrap();
        assert!(status_body.contains("\"configured\":true"));
        assert!(status_body.contains("\"authenticated\":true"));

        let replay = app
            .oneshot(request(
                Method::POST,
                "/api/auth/setup",
                &[
                    (header::HOST.as_str(), &local_host),
                    (header::ORIGIN.as_str(), &local_origin),
                    (
                        header::CONTENT_TYPE.as_str(),
                        "application/x-www-form-urlencoded",
                    ),
                    (header::COOKIE.as_str(), &setup_cookie),
                ],
                "password=correct-horse-battery-staple",
                loopback_peer(),
            ))
            .await
            .unwrap();
        assert_eq!(replay.status(), StatusCode::FORBIDDEN);
    }
}
