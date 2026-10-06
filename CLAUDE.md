# Orbit

The protocol between Sol (the one server) and its worlds (apps that pair with
it), plus the Rust client those apps use. `docs/protocol.md` is the contract;
change it together with the types in `src/` and Sol's handlers.

- Types here are shared on the wire: keep them backwards compatible within a
  minor version (new fields optional with `#[serde(default)]`).
- The client is behind the `client` feature, so Sol doesn't pull in reqwest
  twice and apps that only need the types stay small.
- Sol's tests (`hub/crates/sol/src/tests.rs`) exercise the client end to end;
  run them after changing either side.

Checks: `cargo fmt && cargo clippy --all-targets --features client -- -D warnings && cargo test --features client`.
