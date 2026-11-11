#!/bin/bash
# Set up a CI job that runs in a UBI container (`container:` in the workflow): Red Hat's
# userland around our build and tests, while GitHub's hosted runner (Ubuntu, the only Linux it
# offers) only hosts the container. Run as root, after the checkout:
#
#     .github/scripts/ci-container-setup.sh TOOLCHAIN [COMPONENTS]
#
# - Rust from rustup, the toolchain the rest of CI and the release use: rustup-init pinned to
#   one version and checked against its SHA-256, then TOOLCHAIN (and the comma-separated
#   COMPONENTS) installed under $HOME.
# - A user `ci` with the uid and gid of the checkout's owner (the runner's user), so that the
#   tests run as a normal user, as they did on the runner itself: FUSE mounts through the
#   setuid fusermount3, and the root-only tests skip as tests/expected-skips.json says. The
#   workflow's later steps run as `ci` through `defaults.run.shell` (runuser); the
#   actions run as root, so the job ends its setup with
#   `chown -R ci "$GITHUB_WORKSPACE" "$HOME"` once the caches are restored.
#
# The packages come from the job's first step (dnf), before the checkout, which needs git.
set -euo pipefail

toolchain=$1
components=${2:-}

rustup_version=1.29.3
case "$(uname -m)" in
    x86_64) rustup_sha256=b1c8cb14e30889e7e2999e6b69e62d9e9b9ffc0f4df8180564d306d9680e6c55 ;;
    aarch64) rustup_sha256=9f608c142e0e38a78957d0661ba1dfaf0b6fb37184cfaa4b9a0aa57af6184282 ;;
    *) echo "no pinned rustup-init for $(uname -m)" >&2; exit 1 ;;
esac

uid=$(stat -c %u "$GITHUB_WORKSPACE")
gid=$(stat -c %g "$GITHUB_WORKSPACE")
if [[ "$uid" == 0 ]]; then
    uid=1001 gid=1001
fi
getent group "$gid" >/dev/null || groupadd -g "$gid" ci
useradd -u "$uid" -g "$gid" -M -d "$HOME" -s /bin/bash ci
mkdir -p "$HOME"
chown -R ci: "$GITHUB_WORKSPACE" "$HOME"
git config --system --add safe.directory '*'

dir=$(mktemp -d)
chmod 0755 "$dir"
init=$dir/rustup-init  # rustup-init acts on the name it is run as
curl -sSfL --proto =https --tlsv1.2 -o "$init" \
    "https://static.rust-lang.org/rustup/archive/$rustup_version/$(uname -m)-unknown-linux-gnu/rustup-init"
echo "$rustup_sha256  $init" | sha256sum -c
chmod 0755 "$init"
args=(-y --no-modify-path --profile minimal --default-toolchain "$toolchain")
[[ -n "$components" ]] && args+=(--component "$components")
runuser -u ci -- "$init" "${args[@]}"
rm -rf "$dir"
echo "$HOME/.cargo/bin" >> "$GITHUB_PATH"
runuser -u ci -- env PATH="$HOME/.cargo/bin:$PATH" rustc -vV
