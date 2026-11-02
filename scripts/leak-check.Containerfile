# SPDX-License-Identifier: GPL-3.0-or-later
#
# The environment of scripts/leak-check.sh (#151): UBI 10 with RHEL 10's Rust, valgrind
# (memcheck and massif), fuse3 and Python. Red Hat's packages, as the release image's
# build stage (Containerfile); heaptrack is not in UBI, so massif is the heap profiler.
#
#   podman build -t sacdfs-leak -f scripts/leak-check.Containerfile scripts
FROM registry.access.redhat.com/ubi10/ubi:latest
RUN dnf -y install --setopt=install_weak_deps=False rust cargo valgrind fuse3 python3 procps-ng \
    && dnf clean all
