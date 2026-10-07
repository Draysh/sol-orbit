# Orbit

The protocol between Sol (the one server) and its worlds (apps that pair with
it), plus the Rust client those apps use. `docs/protocol.md` is the contract;
change it together with the types in `src/` and Sol's handlers.

- Types here are shared on the wire: keep them backwards compatible within a
  minor version (new fields optional with `#[serde(default)]`).
- Features keep Sol lean: `client` (HTTP), `keystore` (keyring), `app`
  (both plus `link::Link`, the engine every world's app runs on, and
  `frames`, which keeps the window at the screen's rate on Linux: the app's
  `build.rs` must export `drmWaitVBlank`, see `src/frames.rs`).
- `Link` is shared by every world: world-specific behaviour belongs in the
  world's `Handler`, never here.
- Sol's tests (`hub/crates/sol/src/tests.rs`) exercise the client and two
  `Link` devices end to end; run them after changing either side.

Checks: `cargo fmt && cargo clippy --all-targets --features app -- -D warnings && cargo test --features app`
(and once without features).
