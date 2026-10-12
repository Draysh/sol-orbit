# Orbit

The protocol between [Sol](https://github.com/Draysh/sol) and its worlds, and
the Rust client the worlds' apps use.

Sol is the one server: it keeps every world's data, the person's settings and
the connections between worlds. Each world is an app of its own that pairs
with Sol, the way a music player pairs with a Navidrome server.
[docs/protocol.md](docs/protocol.md) describes the whole protocol.

- **Shapes on the wire**, shared by Sol and the apps: world manifests
  (`sol-world.json`), documents and change pages, events, pairing, inbox
  deliveries, dashboard widgets and updates.
- **`orbit::link::Link`** (feature `app`): the whole engine a world's app runs
  on. It pairs with a code approved in Sol, keeps the token in the system
  keyring (or KWallet), keeps a local copy of the world's data that works
  offline, sends local changes in order, takes in other devices' changes,
  claims and runs what other worlds ask of it, and pushes widgets. Nothing
  written is lost: a document changed on another device first comes back
  as a conflict with both copies kept (`if_version`), a write Sol turns
  down is kept with Sol's reason, and writes waiting when the device is
  unpaired are kept and sent once it pairs again as the same person
  (`Link::kept`). A local copy that won't open says why (`LocalCopyError`).
- **`orbit::telemetry`**: logs to stdout, and for an app to a file of its own
  that starts again at 2 MB, with panics logged.
- **`orbit::clock::Clock`** (feature `app`): the person's day, in their time
  zone and starting at the world's `day_starts` setting (04:00 until it is set,
  `DEFAULT_DAY_STARTS`).
- **`orbit::client::Sol`** (feature `client`): the protocol itself, request by
  request.
- **`orbit::keystore`** (feature `keystore`): secrets in the system keyring,
  with a KWallet fallback for KDE (from Asonica).
- **`orbit::db::Actor`**: SQLite on its own thread, for Sol's databases and an
  app's local copy alike.
- **`orbit::install`** and **`orbit::updates`** (feature `app`): the app
  installing itself for the person and updating through Sol. Every file's
  signature is checked against the keys built into it (one or several), its
  trusted comment must name the file, and the version only goes up; the new
  binary is started (`--version`, its Qt too) before it goes in, the old one
  stays as `<bin>.old`, and a new version that never comes up is rolled back
  on the next start.
- **`orbit::moons`** (feature `app`): moons beside their planet. Every moon
  is carried in its planet's app now; what a moon with an app of its own
  may still use stays: the planet holds a "running" lock while it runs
  (such a moon left when it let go), starts any moon still written down
  for it but those it carries, with `--background` (no window), and
  uninstalling takes a moon off the list.
- **`orbit::doors`** (feature `app`): which Sol apps are installed on the
  computer, and opening one from another.

## Use it in a world's app

World apps usually reach `Link` through
[sol-quick](https://github.com/Draysh/sol-quick), which opens it for them.

```toml
[dependencies]
orbit = { git = "https://github.com/Draysh/sol-orbit", tag = "v0.7.0", features = ["app"] }
```

```rust
use orbit::{clock::Clock, link::{Config, Link, Tokens}};

let link = Link::open(Config {
    world: "terra".into(),
    dir: app_data_dir,
    device: "Desktop".into(),
    platform: Some("linux".into()),
    version: env!("CARGO_PKG_VERSION").into(),
    tokens: Tokens::Keyring,
})
.await?;
link.start(std::sync::Arc::new(MyWorld)); // MyWorld: orbit::link::Handler
link.connect("https://sol.example.ts.net").await?; // Status::Pairing { code, .. }
link.put_as("habits", "walk", &Habit { name: "Walk outside".into() }).await?;
let habits: Vec<Habit> = link.list_as("habits").await?;
let today = Clock::of(&link).await.today();
```

New worlds start from [sol-planet-template](https://github.com/Draysh/sol-planet-template).

## Work on it

```sh
cargo fmt && cargo clippy --all-targets --features app -- -D warnings && cargo test --features app
cargo clippy --all-targets -- -D warnings && cargo test
```

Sol's tests drive the client and two `Link` devices against a real Sol end to
end. Release by bumping the version in `Cargo.toml` and tagging it
(`v0.7.0`), then move the repositories over with Sol's
`scripts/worlds.sh use-tags`.

## Licence

MIT.
