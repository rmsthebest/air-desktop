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

# Builds the package from HEAD's tree as a tagged tarball would be, tests
# included, checks the packaging files, and leaves the package in
# target/release-check. Takes as long as a clean build.
release-check:
    #!/usr/bin/env bash
    set -euo pipefail
    version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
    toolchain=$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)
    grep -qx "pkgver=$version" packaging/aur/PKGBUILD \
        || { echo "PKGBUILD's pkgver isn't $version"; exit 1; }
    grep -qx "_toolchain=$toolchain" packaging/aur/PKGBUILD \
        || { echo "PKGBUILD's _toolchain isn't $toolchain"; exit 1; }
    desktop-file-validate packaging/air-desktop.desktop
    dir=$PWD/target/release-check
    rm -rf "$dir"
    mkdir -p "$dir"
    cp packaging/aur/PKGBUILD "$dir"
    (cd "$dir" && makepkg --printsrcinfo) | diff - packaging/aur/.SRCINFO \
        || { echo ".SRCINFO is stale: makepkg --printsrcinfo > packaging/aur/.SRCINFO"; exit 1; }
    git archive --format=tar.gz --prefix="air-desktop-$version/" \
        -o "$dir/air-desktop-$version.tar.gz" HEAD
    # The tarball is already there, so nothing is downloaded; it has no
    # release checksum to compare with yet.
    (cd "$dir" && makepkg -f --skipchecksums)
    if command -v namcap >/dev/null; then
        namcap "$dir"/air-desktop-*.pkg.tar.*
    else
        echo "namcap isn't installed: skipped"
    fi
