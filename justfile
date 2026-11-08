# Format check, lints and the fast test layers.
check:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo clippy --all-targets --features seam -- -D warnings
    cargo test

run:
    cargo run

# Account seam tests: a real core against Air's in-process test server
# (Postgres and MinIO from Air's compose file must be running). Behind the
# `seam` feature, so other test builds keep the production core.
seam:
    cargo test --features seam seam -- --ignored

# Regenerate the dependencies' license list the about screen shows, after
# Cargo.lock changes (a test fails until then). Needs
# `cargo install cargo-about --features cli --locked`.
regenerate-licenses:
    cargo about generate --offline -c about.toml about.hbs -o assets/licenses.json
    sha256sum Cargo.lock | cut -d' ' -f1 > assets/licenses.lock
