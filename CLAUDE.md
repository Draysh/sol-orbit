# Orbit

The protocol between Sol (the one server) and its worlds (apps that pair with
it), plus the Rust client those apps use. `docs/protocol.md` is the contract;
change it together with the types in `src/` and Sol's handlers.

## Who builds on it

- Sol (`hub/crates/sol`) uses the shapes, `db` and `telemetry`, without
  features (its tests use `app`). `kit/quick`, the template and every world
  app use feature `app`.
- They build against a **git tag** of this repository (`orbit = { git, tag }`),
  so an edit here reaches nothing until `hub/scripts/worlds.sh use-local`
  points them at this checkout (it is on during the 2026-10 refactor; leave it).
- Sol's tests (`hub/crates/sol/src/tests.rs`) drive the client and two `Link`
  devices against a real Sol end to end: run `cargo test --workspace` in
  `hub/` after changing either side.
- Releasing: bump `version` in `Cargo.toml`, tag `v<version>`, push, then
  `hub/scripts/worlds.sh use-tags <orbit> <design> <quick>` with the new tag
  and release the consumers. Tags and pushes are the person's.

## Modules

Without features (the shapes, shared with Sol):

- `lib.rs`: the module list, `PROTOCOL` and the two request headers.
- `world.rs`: `WorldManifest` (`sol-world.json`) and its checks.
- `doc.rs`: documents, change pages, name rules, `value_of` (JSON that keeps `f32`s as written).
- `event.rs`: `Emit` (what an app posts) and `Envelope` (what Sol stores).
- `device.rs`: pairing, `Person`, inbox `Delivery`s, widget pushes.
- `widget.rs`: `WidgetView`, a widget on Sol's dashboard.
- `update.rs`: the update check, release file names and install kinds.
- `error.rs`: `ErrorBody`, every error's JSON.
- `db.rs`: `Actor`, SQLite on its own thread (Sol's databases and an app's copy).
- `telemetry.rs`: logging set-up.

`client` (HTTP) and `keystore` (keyring):

- `client.rs`: `client::Sol`, the protocol request by request.
- `keystore.rs`: secrets in the Secret Service or KWallet.

`app` (both plus everything a world app runs on):

- `link/mod.rs`: `Link`, its `Config`, `Status`, `Update`, `Handler`; events, widgets, the time zone.
- `link/docs.rs`: documents in the local copy (`list`, `get`, `put`, `delete` and the typed `*_as`).
- `link/settings.rs`: the world's settings and `on_settings` / `Configure`.
- `link/pairing.rs`: connect, approval, unpairing, starting over.
- `link/sync.rs`: sending local writes, taking in changes, online/offline.
- `link/inbox.rs`: claiming deliveries and handing them to the `Handler`.
- `link/cache.rs` + `cache.sql`: the local SQLite tables and their helpers.
- `link/token.rs`: the token in the keyring or a private file.
- `clock.rs`: `Clock`, the person's day (time zone and `day_starts`).
- `install.rs`, `updates.rs`: the app installing and updating itself.
- `moons.rs`: moons started by their planet; `doors.rs`: opening other worlds' apps.

## Tests

Inline `mod tests` in each file, except `link/tests.rs` (offline `Link`
tests). `tests/fixtures/` holds a signed file and its key for `updates.rs`.
The online paths are tested from Sol's side (above).

## Shapes that must stay stable

Keep them backwards compatible within a minor version (new fields optional,
`#[serde(default)]`):

- On the wire: everything in `world`, `doc`, `event`, `device`, `widget`,
  `update` and `error`, plus release file names (`sol-<world>-<version>-<target><suffix>`
  and `<file>.sig`).
- On disk, in the folder every Sol app on the computer shares
  (`sol-worlds` in the data folder): `moons/<planet>/<moon>.json`
  (`{ path, on }`), `apps/<world>.json` (`{ path }`) and the
  `running/<world>.lock` files. Other apps' builds read them.
- An app's local copy (`<world>.db`): `cache.sql` is a migration; add a new
  one rather than editing it. `pref:` keys in `meta` survive pairing again.

## Rules

- `Link` is shared by every world: world-specific behaviour belongs in the
  world's `Handler` or its own code, never here.

Checks: `cargo fmt && cargo clippy --all-targets --features app -- -D warnings && cargo test --features app`,
then `cargo clippy --all-targets -- -D warnings && cargo test` (no features).
