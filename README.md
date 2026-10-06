# Orbit

The SDK every world of [Sol](https://github.com/Draysh/sol) is built on. An
app hands `orbit::run` its manifest, migrations, routes, OpenAPI document and
embedded web UI; Orbit supplies the rest:

- the `serve`, `healthcheck` and `openapi` subcommands, configured by environment;
- logging, request IDs and graceful shutdown;
- SQLite behind a one-thread actor, with migrations;
- checks of the per-app tokens Sol signs (`User` and `System` extractors,
  the person's time zone and "today");
- the transactional event outbox and the `/_sol/*` endpoints Sol polls;
- the `/ui/*` file server for the app's own pages;
- `WidgetView`, the shape of a dashboard widget.

[docs/app-contract.md](docs/app-contract.md) is the full contract between Sol
and its apps.

## Use it

```toml
[dependencies]
orbit = { git = "https://github.com/Draysh/sol-orbit", tag = "v0.1.0" }
```

```rust
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    orbit::run("terra", spec(), Cli::parse().command).await
}
```

New apps start from [sol-planet-template](https://github.com/Draysh/sol-planet-template),
which is already wired up.

## Work on it

```sh
cargo fmt && cargo clippy --all-targets -- -D warnings && cargo test
```

Release by tagging (`v0.2.0`), then move the apps to the new tag with Sol's
`scripts/worlds.sh use-tags`.

## Licence

MIT.
