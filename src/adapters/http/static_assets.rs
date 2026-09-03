use axum::{
    http::header,
    response::{Html, IntoResponse},
};

use super::INDEX_HTML;

pub(super) async fn index() -> Html<&'static str> {
    Html(INDEX_HTML)
}

pub(super) async fn favicon() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "image/x-icon"),
            (header::CACHE_CONTROL, "public, max-age=86400"),
        ],
        include_bytes!("../../../src-tauri/icons/icon.ico").as_slice(),
    )
}

pub(super) async fn alpine_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        include_str!("../../../www/assets/alpine.min.js"),
    )
}

pub(super) async fn app_js() -> impl IntoResponse {
    (
        [(
            header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        include_str!("../../../www/assets/iuna-ui.js"),
    )
}
