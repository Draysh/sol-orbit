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
- **`orbit::client::Sol`** (feature `client`): pair, read settings, write and
  sync documents, post events, read the inbox, push widgets.
- **`orbit::db::Actor`**: SQLite on its own thread, for Sol's databases and an
  app's local cache alike.

## Use it in a world's app

```toml
[dependencies]
orbit = { git = "https://github.com/Draysh/sol-orbit", tag = "v0.2.0", features = ["client"] }
```

```rust
use std::time::Duration;
use orbit::client::Sol;

let sol = Sol::new("https://sol.example.ts.net");
let started = sol.pair("terra", "Desktop", Some("linux")).await?;
// Show started.code; the person approves it in Sol.
let paired = loop {
    if let Some(paired) = sol.claim(&started, Duration::from_secs(25)).await? {
        break paired;
    }
};
let sol = sol.with_token(paired.token);
sol.put("habits", "walk", serde_json::json!({ "name": "Walk outside" }), None).await?;
```

## Work on it

```sh
cargo fmt && cargo clippy --all-targets --features client -- -D warnings && cargo test --features client
```

Sol's tests drive the client against a real Sol end to end. Release by tagging
(`v0.3.0`), then move the repositories over with Sol's `scripts/worlds.sh use-tags`.

## Licence

MIT.
