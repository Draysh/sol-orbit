//! Serves a web UI compiled into the binary.
//!
//! Each app embeds its own SvelteKit build (built with `paths.base` set to
//! `/<app>`) and serves it at `/ui/*`. Sol maps the browser's `/<app>/…` onto
//! it, so every app's screens live on Sol's origin next to Sol's own.

use axum::{
    Router,
    body::Body,
    http::{StatusCode, Uri, header},
    response::{IntoResponse, Response},
    routing::get,
};
use rust_embed::EmbeddedFile;

/// Looks a file up in an embedded build: `Ui::get` for a
/// `#[derive(rust_embed::Embed)] struct Ui;`.
pub type Assets = fn(&str) -> Option<EmbeddedFile>;

/// `/ui`, `/ui/` and `/ui/*` from `assets`.
pub fn routes<S: Clone + Send + Sync + 'static>(assets: Assets) -> Router<S> {
    let page = move |uri: Uri| async move {
        let path = uri.path().strip_prefix("/ui").unwrap_or_default();
        respond(assets, path)
    };
    Router::new()
        .route("/ui", get(page))
        .route("/ui/", get(page))
        .route("/ui/{*path}", get(page))
}

/// The file at `path`, or the single-page app's `index.html` for any path that
/// isn't a file. Hashed build output is cached for good; the rest revalidates.
pub fn respond(assets: Assets, path: &str) -> Response {
    let path = path.trim_start_matches('/');
    if let Some(file) = assets(path).filter(|_| !path.is_empty()) {
        return file_response(path, file);
    }
    let last = path.rsplit('/').next().unwrap_or_default();
    if last.contains('.') {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    match assets("index.html") {
        Some(index) => file_response("index.html", index),
        None => (
            [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
            "<!doctype html><title>Sol</title><p>This build has no web UI. \
             Build it with <code>npm run build --prefix web</code>, then rebuild the binary.</p>",
        )
            .into_response(),
    }
}

fn file_response(path: &str, file: EmbeddedFile) -> Response {
    let cache = if path.starts_with("_app/immutable/") {
        "public, max-age=31536000, immutable"
    } else {
        "no-cache"
    };
    (
        [
            (header::CONTENT_TYPE, file.metadata.mimetype().to_owned()),
            (header::CACHE_CONTROL, cache.to_owned()),
        ],
        Body::from(file.data.into_owned()),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use axum::body::to_bytes;
    use rust_embed::Embed;

    use super::*;

    #[derive(Embed)]
    #[folder = "tests/ui/"]
    struct TestUi;

    async fn body(res: Response) -> String {
        String::from_utf8(to_bytes(res.into_body(), 1024).await.unwrap().to_vec()).unwrap()
    }

    #[tokio::test]
    async fn files_pages_and_missing_files() {
        let js = respond(TestUi::get, "/_app/immutable/a.js");
        assert_eq!(
            js.headers()[header::CACHE_CONTROL],
            "public, max-age=31536000, immutable"
        );
        assert_eq!(body(js).await, "js");

        // Any route of the app gets the shell; a missing file does not.
        let page = respond(TestUi::get, "/habits/today");
        assert_eq!(page.headers()[header::CACHE_CONTROL], "no-cache");
        assert_eq!(body(page).await, "<!doctype html>index");
        assert_eq!(body(respond(TestUi::get, "")).await, "<!doctype html>index");
        assert_eq!(
            respond(TestUi::get, "/missing.png").status(),
            StatusCode::NOT_FOUND
        );
    }
}
