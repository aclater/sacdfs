#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# zig and cargo-zigbuild for the jobs that link through zig (the release binaries and the
# i686 tests, #244, #488): one zig version, its archive checked against its SHA-256, and one
# cargo-zigbuild version, the same in every job. Run on a GitHub runner, after Rust is set up:
#
#     .github/scripts/zig-setup.sh
#
# Unpacks zig under $RUNNER_TEMP, puts it on the PATH of the job's later steps ($GITHUB_PATH)
# and installs cargo-zigbuild with cargo.
set -euo pipefail

zig_version=0.16.0
cargo_zigbuild_version=0.24.0
arch=$(uname -m)
case $arch in
    x86_64) zig_sha256=70e49664a74374b48b51e6f3fdfbf437f6395d42509050588bd49abe52ba3d00 ;;
    aarch64) zig_sha256=ea4b09bfb22ec6f6c6ceac57ab63efb6b46e17ab08d21f69f3a48b38e1534f17 ;;
    *) echo "no pinned zig for $arch" >&2; exit 1 ;;
esac

zig="zig-$arch-linux-$zig_version"
curl -sSfL --proto =https --tlsv1.2 -o "$RUNNER_TEMP/$zig.tar.xz" \
    "https://ziglang.org/download/$zig_version/$zig.tar.xz"
echo "$zig_sha256  $RUNNER_TEMP/$zig.tar.xz" | sha256sum -c
tar -C "$RUNNER_TEMP" -xJf "$RUNNER_TEMP/$zig.tar.xz"
echo "$RUNNER_TEMP/$zig" >> "$GITHUB_PATH"
cargo install --locked cargo-zigbuild --version "$cargo_zigbuild_version"
