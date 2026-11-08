#!/usr/bin/env bash
# Builds the package in a clean Arch container, as root at first: makepkg itself
# refuses root. `.github/workflows/package.yml` runs this; to try it locally:
#   podman run --rm -v "$PWD:/src:Z" -w /src archlinux:base-devel packaging/ci-build.sh
set -euo pipefail

# Providers for the PKGBUILD's virtual dependencies, then the PKGBUILD's own
# lists, so a dependency missing from them fails here too.
source packaging/aur/PKGBUILD
pacman -Syu --noconfirm --needed vulkan-swrast ttf-dejavu
pacman -S --noconfirm --needed "${depends[@]}" "${makedepends[@]}" \
    git just desktop-file-utils namcap

useradd --create-home builder
chown -R builder: .
# Less to keep: a runner's disk is small, and nothing here debugs these builds.
su builder -c 'CARGO_INCREMENTAL=0 CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 just release-check'
