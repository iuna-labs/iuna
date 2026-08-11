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
    ChangePasswordForm, HttpState, action_json,
    auth::session_token_hash,
    request_auth::{
        auth_client_key, auth_cookie, auth_exempt_path, change_auth_password, csrf_required,
        login_auth_password, request_is_authenticated, same_origin_request, setup_auth_password,
    },
};

pub(super) async fn require_auth_middleware(
    State(state): State<HttpState>,
    headers: HeaderMap,
    mut request: Request<Body>,
    next: Next,
) -> Response {
    let path = request.uri().path().to_string();
    if csrf_required(request.method()) && !same_origin_request(&headers) {
        return csrf_error().into_response();
    }
    let client_key = auth_client_key(
        &headers,
        request
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|info| info.0),
    );
    request.extensions_mut().insert(AuthClientKey(client_key));
    if auth_exempt_path(&path) {
        return next.run(request).await;
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
    Form(form): Form<AuthForm>,
) -> Response {
    match setup_auth_password(&state, &form.password, &client_key.0).await {
        Ok(cookie) => ([(header::SET_COOKIE, cookie)], action_json(Ok(()))).into_response(),
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
