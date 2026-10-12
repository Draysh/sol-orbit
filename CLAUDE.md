# Orbit

The protocol between Sol (the one server) and its worlds (apps that pair with
it), plus the Rust client those apps use. `docs/protocol.md` is the contract;
change it together with the types in `src/` and Sol's handlers.

## Who builds on it

- Sol (`hub/crates/sol`) uses the shapes, `db` and `telemetry`, without
  features (its tests use `app`). `kit/quick`, the template and every world
  app use feature `app`; sol-quick-core (`kit/quick/core`) turns `testing`
  on too, always (its test helpers are always built, so orbit is one build
  for an app and its tests): what `testing` adds must stay additive.
- They build against a **git tag** of this repository (`orbit = { git, tag }`),
  so an edit here reaches nothing until they point at this checkout:
  `hub/scripts/worlds.sh use-local` does it for `kit/quick`, the template
  and the worlds (it is on now; leave it). The hub stays pinned to its tag.
- Sol's tests (`hub/crates/sol/src/tests/`) drive the client and two `Link`
  devices against a real Sol end to end. After changing either side, run
  them against this checkout without touching the hub's manifest: in `hub/`,
  `cargo test --workspace --config
  'patch."https://github.com/Draysh/sol-orbit".orbit.path="../kit/orbit"'`,
  then `git checkout Cargo.lock` if the lock changed.
- Releasing: bump `version` in `Cargo.toml`, tag `v<version>`, push, then
  `hub/scripts/worlds.sh use-tags <orbit> <design> <quick>` with the new tag
  and release the consumers. Tags and pushes are the person's.

## Modules

Without features (the shapes, shared with Sol):

- `lib.rs`: the module list, `PROTOCOL` and the two request headers.
- `world.rs`: `WorldManifest` (`sol-world.json`) and its checks.
- `doc.rs`: documents, change pages, name rules, `value_of` (JSON that keeps `f32`s as written), `first_shape` (the `v` of rows from before a stored type had one).
- `event.rs`: `Emit` (what an app posts) and `Envelope` (what Sol stores).
- `device.rs`: pairing, `Person`, inbox `Delivery`s, widget pushes.
- `widget.rs`: `WidgetView`, a widget on Sol's dashboard.
- `update.rs`: the update check, release file names and install kinds (`portable` alone).
- `error.rs`: `ErrorBody`, every error's JSON.
- `db.rs`: `Actor`, SQLite on its own thread (Sol's databases and an app's copy).
- `telemetry.rs`: logging set-up; `init_app` adds a log file that starts again at 2 MB, and logs panics.

`client` (HTTP) and `keystore` (keyring):

- `client.rs`: `client::Sol`, the protocol request by request.
- `keystore.rs`: secrets in the Secret Service or KWallet.

`app` (both plus everything a world app runs on):

- `link/mod.rs`: `Link`, its `Config`, `Status`, `Update`, `Handler`; events, widgets, the time zone.
- `link/mod.rs` also: `LocalCopyError` (a copy that won't open: newer or damaged) and `set_aside`;
  waiting for a locked keyring at login.
- `link/docs.rs`: documents in the local copy (`list`, `list_prefix` and `count` by the start of
  their ids on the `(collection, id)` key, `get`, `put` with the version it was made
  on, `overwrite` without, `delete`, the typed `*_as`, and `list_counted`: rows with how many of
  another shape were left out, logged once when that count changes).
- `link/kept.rs`: writes kept aside (`held` table): conflicts (a `409`, both copies), writes Sol
  rejected (`400`/`413`), writes from before an unpairing (sent again for the same person); `Kept`,
  `KeptWrite`, `keep_mine`, `drop_kept`, `export_kept`.
- `link/settings.rs`: the world's settings and `on_settings` / `Configure`.
- `link/pairing.rs`: connect, approval, unpairing, starting over (waiting writes are set aside,
  never deleted, and replayed when the same person pairs again).
- `link/sync.rs`: sending local writes (`if_version`, conflicts, rejected writes), taking in
  changes, online/offline, waiting out a `429`, reading everything again when Sol's database id
  changes (a restore), and starting the sync and inbox loops again after a panic.
- `link/inbox.rs`: claiming deliveries and handing them to the `Handler`.
- `link/cache.rs` + `cache.sql`, `cache-2.sql`: the local SQLite tables (opened with
  `db::open_with_backup`, copies in `backups/`) and their helpers.
- `link/token.rs`: the token in the keyring or a private file.
- `clock.rs`: `Clock`, the person's day: the time zone and `day_starts`
  (unset, `DEFAULT_DAY_STARTS`, 04:00, which the manifests that declare it
  default to: Terra, Luna, Mercury and the template). A world without the
  setting uses `Clock::calendar`, midnight in the person's zone. Every
  "today" goes through it.
- `install.rs`, `updates.rs`: the app installing and updating itself: `verify_offer` (keys, the
  trusted comment's file, versions only up), a test start (`--version`, Qt major.minor), the switch
  keeping `<bin>.old` (fsynced), the trial (`.<bin>.trial`) and rollback (`.<bin>.rolled-back`).
- `moons.rs`: a planet's "running" lock and starting a moon still written down for it but
  not carried (`start_except`), kept for moons with an app of their own;
  `doors.rs`: opening other worlds' apps.

`testing` (`app` plus test helpers, for tests here and in the worlds):

- `testing.rs`: `offline` / `open` (a `Link` with a local copy and no Sol),
  `FakeHttp` (a canned HTTP server on 127.0.0.1: `routes` by method and
  path, or `start` with a function; every `Request` kept, with its
  headers, body and time), `Reply`, `StandInSol` (settings answered, long
  polls held, the rest to a function; `paired` / `paired_with` for a link
  paired with it as `person(id)`), `puts`.

## Tests

Inline `mod tests` in each file, except `link/tests.rs` (`Link` offline,
and against `testing::StandInSol`s that answer `429`, `401`, `409`, `413`:
every rule about keeping writes has a test there). `tests/fixtures/` holds two
signed files and their keys for `updates.rs` (`update.txt`, and a release
`.tar.gz` whose trusted comment names it, from Sol's tests). Sol's
`nothing_written_is_lost` drives a conflict and a re-pairing end to end.
The online paths are tested from Sol's side (above).

## Shapes that must stay stable

Protocol 2 is frozen for Sol 1.0: `docs/protocol.md`'s "Compatibility"
section says what may change. In short, new fields are optional
(`#[serde(default)]`), and every enum read from the wire ends in a
`#[serde(other)] Unknown` variant, so a new value is not breaking:

- On the wire: everything in `world`, `doc`, `event`, `device`, `widget`,
  `update` and `error`, plus release file names (`sol-<world>-<version>-<target>.tar.gz`
  and `<file>.sig`, a minisign signature).
- On disk, in the folder every Sol app on the computer shares
  (`sol-worlds` in the data folder): `moons/<planet>/<moon>.json`
  (`{ path, on }`), `apps/<world>.json` (`{ path }`) and the
  `running/<world>.lock` files. Other apps' builds read them.
- An app's local copy (`<world>.db`): `cache.sql` and `cache-2.sql` are
  migrations; add a new one rather than editing them. `pref:` keys in
  `meta` survive pairing again; `held` rows survive unpairing.
- Beside an installed binary: `<bin>.old`, `.<bin>.trial` and
  `.<bin>.rolled-back` (JSON, `install::Trial` / `RolledBack`).

## Rules

- `Link` is shared by every world: world-specific behaviour belongs in the
  world's `Handler` or its own code, never here.

Checks: `cargo fmt && cargo clippy --all-targets --features app -- -D warnings &&
cargo test --features app`, then `cargo clippy --all-targets -- -D warnings && cargo test`
(no features), and `cargo clippy --all-targets --features testing -- -D warnings`.
