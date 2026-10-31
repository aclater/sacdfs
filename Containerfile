# SPDX-License-Identifier: GPL-3.0-or-later
#
# sacdfs for a NAS: the FUSE mount and the servers that share it (Samba for SMB,
# NFS-Ganesha for NFS) in one container. See docs/deployment.md.
#
#   podman build -t sacdfs .
#
# Why two bases (docs/deployment.md, "Image"): sacdfs is built on UBI 10 with RHEL's Rust
# toolchain. UBI's repositories carry neither Samba nor an NFS server, RHEL's full
# repositories need a subscription (so neither this public image nor CI can use them), and
# NFS-Ganesha is not in RHEL at all; the runtime stage is therefore Fedora, which has all
# three. sacdfs links only glibc and libgcc_s, which Fedora's newer glibc runs.

# Build stage: UBI 10 and RHEL's Rust (AppStream).
FROM registry.access.redhat.com/ubi10/ubi:latest@sha256:7495bfa53ae7a81a58aeface0bb4858d12741832c542c29dc690d67b183b73b0 AS build
RUN dnf -y --setopt=install_weak_deps=False install cargo gcc && dnf clean all
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src src
COPY benches benches
COPY examples examples
# Generic x86-64 by default, set explicitly: RHEL 10's Rust targets x86-64-v3 (AVX2) unless
# told otherwise, and many NAS CPUs (Atom, Celeron, Pentium Silver) have no AVX2. The PCM
# converter still uses AVX2 where the CPU has it (runtime dispatch, #23). Pass
# --build-arg RUSTFLAGS="-C target-cpu=x86-64-v3" for an AVX2 NAS (about 20 % less CPU for PCM).
ARG RUSTFLAGS="-C target-cpu=x86-64"
RUN RUSTFLAGS="$RUSTFLAGS" cargo build --release --locked --bin sacdfs \
 && install -D -m 0755 target/release/sacdfs /out/sacdfs

# Runtime stage: Fedora, for Samba, NFS-Ganesha and fusermount3.
FROM registry.fedoraproject.org/fedora:44@sha256:b414c09be79dd1f32529b55b096b6bb30435d82ee16fbdbbabf8985eb5f86db0
RUN dnf -y --setopt=install_weak_deps=False install \
        samba nfs-ganesha nfs-ganesha-vfs fuse3 procps-ng \
 && dnf clean all \
 && rm -rf /var/cache/dnf /var/log/dnf* \
 && mkdir -p /images /srv/sacd /cache /var/lib/nfs/ganesha /run/ganesha
COPY --from=build /out/sacdfs /usr/local/bin/sacdfs
COPY deploy/entrypoint.sh deploy/healthcheck.sh /usr/local/bin/
COPY COPYING /usr/share/licenses/sacdfs/COPYING

# All settings are environment variables; see docs/deployment.md.
ENV SACDFS_SOURCE=/images \
    SACDFS_FORMATS=dsf,wav \
    SACDFS_CACHE_MB=256 \
    SACDFS_INDEX_CACHE=/cache \
    SMB=1 \
    NFS=1 \
    SHARE_NAME=sacd

LABEL org.opencontainers.image.title="sacdfs" \
      org.opencontainers.image.description="SACD images as DSF and WAV files, shared over SMB and NFS" \
      org.opencontainers.image.source="https://github.com/aclater/sacdfs" \
      org.opencontainers.image.licenses="GPL-3.0-or-later"

EXPOSE 445/tcp 2049/tcp
VOLUME ["/cache"]
HEALTHCHECK --interval=30s --timeout=10s --start-period=60s --retries=3 \
    CMD ["/usr/local/bin/healthcheck.sh"]
STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
