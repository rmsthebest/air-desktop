# Format check, lints and the fast test layers.
check:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    cargo test

run:
    cargo run
