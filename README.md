# Orbit

The protocol between [Sol](https://github.com/Draysh/sol) and its worlds, and
the Rust client the worlds' apps use.

Sol is the one server: it keeps every world's data, the person's settings and
the connections between worlds. Each world is an app of its own that pairs
with Sol, the way a music player pairs with a Navidrome server.
[docs/protocol.md](docs/protocol.md) describes the whole protocol.

- **Shapes on the wire**, shared by Sol and the apps: world manifests
  (`sol-world.json`), documents and change pages, events, pairing, inbox
  deliveries and dashboard widgets.
- **`orbit::link::Link`** (feature `app`): the whole engine a world's app runs
  on. It pairs with a code approved in Sol, keeps the token in the system
  keyring (or KWallet), keeps a local copy of the world's data that works
  offline, sends local changes in order, takes in other devices' changes,
  claims and runs what other worlds ask of it, and pushes widgets.
- **`orbit::client::Sol`** (feature `client`): the protocol itself, request by
  request.
- **`orbit::keystore`** (feature `keystore`): secrets in the system keyring,
  with a KWallet fallback for KDE (from Asonica).
- **`orbit::db::Actor`**: SQLite on its own thread, for Sol's databases and an
  app's local copy alike.
- **`orbit::frames`** (feature `app`, Linux): the app's window painting at
  the screen's rate. WebKitGTK falls back to a 60 Hz timer where the driver
  has no `drmWaitVBlank` (NVIDIA); the app exports its own, and turns off
  WebKit's preference for page updates near 60 a second.

## Use it in a world's app

```toml
[dependencies]
orbit = { git = "https://github.com/Draysh/sol-orbit", tag = "v0.5.0", features = ["app"] }
```

```rust
use orbit::link::{Config, Link, Tokens};

let link = Link::open(Config {
    world: "terra".into(),
    dir: app_data_dir,
    device: "Desktop".into(),
    platform: Some("linux".into()),
    tokens: Tokens::Keyring,
})
.await?;
link.start(std::sync::Arc::new(MyWorld)); // MyWorld: orbit::link::Handler
link.connect("https://sol.example.ts.net").await?; // Status::Pairing { code, .. }
link.put("habits", "walk", serde_json::json!({ "name": "Walk outside" })).await?;
```

New worlds start from [sol-planet-template](https://github.com/Draysh/sol-planet-template),
a Tauri app already built on `Link`.

## Work on it

```sh
cargo fmt && cargo clippy --all-targets --features app -- -D warnings && cargo test --features app
```

Sol's tests drive the client and two `Link` devices against a real Sol end to end. Release by tagging
(`v0.5.0`), then move the repositories over with Sol's `scripts/worlds.sh use-tags`.

## Licence

MIT.
