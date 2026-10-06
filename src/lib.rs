//! Orbit: the shared SDK every Sol app is built on.
//!
//! An app hands [`run`] its manifest, migrations, OpenAPI document, routes and
//! embedded web UI; Orbit supplies the CLI, logging, the SQLite actor, the
//! `/_sol/*` system endpoints Sol talks to, token checks, the `/ui/` file
//! server and graceful shutdown. `docs/app-contract.md` is the full contract.

pub mod auth;
pub mod cli;
pub mod db;
pub mod event;
pub mod manifest;
pub mod problem;
pub mod server;
pub mod telemetry;
pub mod ui;
pub mod widget;

use std::time::Instant;

use axum::Router;
use rusqlite::Connection;
use rusqlite_migration::Migrations;

pub use auth::{Claims, Scope, System, User, Verifier};
pub use db::Actor;
pub use event::Outbox;
pub use manifest::Manifest;
pub use problem::{ApiError, ApiResult};
pub use widget::WidgetView;

/// Everything an app needs to boot.
pub struct AppSpec {
    pub manifest: Manifest,
    pub migrations: Migrations<'static>,
    pub openapi: utoipa::openapi::OpenApi,
    pub routes: fn() -> Router<Ctx>,
    /// The app's web UI, embedded in the binary; served at `/ui/`.
    pub ui: Option<ui::Assets>,
}

/// Shared state handed to every handler of an app.
#[derive(Clone)]
pub struct Ctx {
    pub id: &'static str,
    pub db: Actor<Connection>,
    pub outbox: Outbox,
    pub verifier: Verifier,
    pub started: Instant,
}

impl axum::extract::FromRef<Ctx> for Verifier {
    fn from_ref(ctx: &Ctx) -> Self {
        ctx.verifier.clone()
    }
}

/// Runs one app subcommand: `serve`, `healthcheck` or `openapi`.
pub async fn run(id: &'static str, spec: AppSpec, command: cli::Command) -> anyhow::Result<()> {
    match command {
        cli::Command::Serve(args) => {
            telemetry::init();
            let path = args.data_dir.join(format!("{id}.db"));
            let migrations = spec.migrations;
            let db = Actor::spawn(id, move || db::open(&path, &migrations))?;
            let ctx = Ctx {
                id,
                db,
                outbox: Outbox::new(id),
                verifier: Verifier::remote(id, &args.sol_url),
                started: Instant::now(),
            };
            let mut manifest = spec.manifest;
            manifest.ui = spec.ui.is_some();
            let mut router = (spec.routes)().merge(server::system_routes(manifest, spec.openapi));
            if let Some(assets) = spec.ui {
                router = router.merge(ui::routes(assets));
            }
            let router = router.with_state(ctx);
            server::serve(router, args.bind).await
        }
        cli::Command::Healthcheck(args) => cli::healthcheck(&args).await,
        cli::Command::Openapi => {
            println!("{}", spec.openapi.to_pretty_json()?);
            Ok(())
        }
    }
}
