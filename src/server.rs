use std::{net::SocketAddr, sync::Arc, time::Duration};

use axum::{
    Json, Router,
    extract::{Query, State},
    routing::get,
};
use serde::Deserialize;
use tokio::net::TcpListener;
use tower_http::{
    catch_panic::CatchPanicLayer,
    request_id::{MakeRequestUuid, PropagateRequestIdLayer, SetRequestIdLayer},
    trace::TraceLayer,
};

use crate::{
    ApiResult, Ctx, System,
    event::{self, MAX_WAIT, OutboxPage},
    manifest::{Health, Manifest},
};

/// Adds the standard layers and serves until SIGTERM or Ctrl-C.
pub async fn serve(router: Router, bind: SocketAddr) -> anyhow::Result<()> {
    let app = with_layers(router);
    let listener = TcpListener::bind(bind).await?;
    tracing::info!(%bind, "listening");
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    Ok(())
}

pub fn with_layers(router: Router) -> Router {
    router
        .layer(CatchPanicLayer::new())
        .layer(TraceLayer::new_for_http())
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(MakeRequestUuid))
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        if let Ok(mut sig) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            sig.recv().await;
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutting down");
}

/// The `/_sol/*` endpoints every app exposes to Sol.
pub fn system_routes(manifest: Manifest, openapi: utoipa::openapi::OpenApi) -> Router<Ctx> {
    let manifest = Arc::new(manifest);
    let openapi = Arc::new(openapi);
    Router::new()
        .route(
            "/_sol/manifest",
            get(move || async move { Json(manifest.as_ref().clone()) }),
        )
        .route(
            "/_sol/openapi.json",
            get(move || async move { Json(openapi.as_ref().clone()) }),
        )
        .route("/_sol/health", get(health))
        .route("/_sol/outbox", get(outbox))
}

async fn health(State(ctx): State<Ctx>) -> Json<Health> {
    let db = ctx
        .db
        .call(|c| c.query_row("SELECT 1", [], |r| r.get::<_, i64>(0)))
        .await;
    Json(Health {
        status: "ok".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        arch: std::env::consts::ARCH.into(),
        uptime_s: ctx.started.elapsed().as_secs(),
        db: if matches!(db, Ok(Ok(1))) {
            "ok"
        } else {
            "error"
        }
        .into(),
    })
}

#[derive(Debug, Deserialize)]
struct OutboxQuery {
    #[serde(default)]
    after: i64,
    /// Seconds to wait for new events when there are none yet.
    #[serde(default)]
    wait: u64,
    #[serde(default = "default_limit")]
    limit: u32,
}

fn default_limit() -> u32 {
    100
}

async fn outbox(
    State(ctx): State<Ctx>,
    _sol: System,
    Query(q): Query<OutboxQuery>,
) -> ApiResult<Json<OutboxPage>> {
    let wait = Duration::from_secs(q.wait).min(MAX_WAIT);
    let limit = q.limit.clamp(1, 500);
    // Register for wake-ups before reading, so an event committed in between
    // still ends the wait.
    let notified = ctx.outbox.notify.notified();
    tokio::pin!(notified);
    notified.as_mut().enable();

    let source = ctx.outbox.source;
    let read = |ctx: &Ctx| {
        let db = ctx.db.clone();
        async move {
            db.call(move |c| event::read_after(c, source, q.after, limit))
                .await
        }
    };
    let page = read(&ctx).await??;
    if !page.events.is_empty() || wait.is_zero() {
        return Ok(Json(page));
    }
    let _ = tokio::time::timeout(wait, notified).await;
    Ok(Json(read(&ctx).await??))
}
