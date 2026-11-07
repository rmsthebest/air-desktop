# Format check, lints and the fast test layers.
check:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test

run:
    cargo run

# Account seam tests: a real core against Air's in-process test server
# (Postgres and MinIO from Air's compose file must be running).
seam:
    cargo test seam -- --ignored
