# SPDX-License-Identifier: GPL-3.0-or-later
#
# Two BusyBox-only images for deploy/ci-install-test.sh, from unmodified Hummingbird packages
# (Red Hat's hardened, Fedora-derived images). @BUILDER@ is the pinned builder image.
#   busybox:        filesystem, glibc and busybox, nothing else: no bash, no curl, no
#                   coreutils. sh is BusyBox ash, as on QNAP or ASUSTOR.
#   busybox-nolibc: the package's static BusyBox alone, no C library: the release binary
#                   can't load, and the installer must stop before changing anything.
FROM @BUILDER@ AS build
USER 0
RUN dnf -y -q --installroot /rootfs --use-host-config --setopt=install_weak_deps=False \
        install filesystem glibc busybox \
 && rpm --root /rootfs -e --nodeps bash \
 && cp /rootfs/usr/bin/busybox.musl.static /tmp/busybox.static \
 && rm -f /rootfs/usr/bin/sh /rootfs/usr/bin/busybox.musl.static /rootfs/usr/bin/udhcpc \
 && chroot /rootfs /usr/bin/busybox --install -s /usr/bin \
 && mkdir -p /rootfs/var/log /rootfs/var/run /rootfs/opt /rootfs/srv /rootfs/tmp \
 && chmod 1777 /rootfs/tmp && rm -rf /rootfs/var/cache/dnf /rootfs/var/lib/dnf \
 && mkdir -p /nolibc/usr/bin /nolibc/tmp /nolibc/var/log /nolibc/var/run /nolibc/opt /nolibc/srv \
 && cp /tmp/busybox.static /nolibc/usr/bin/busybox && chmod 1777 /nolibc/tmp \
 && chroot /nolibc /usr/bin/busybox --install -s /usr/bin && ln -s usr/bin /nolibc/bin

FROM scratch AS busybox
COPY --from=build /rootfs/ /
ENV PATH=/usr/bin:/usr/sbin

FROM scratch AS busybox-nolibc
COPY --from=build /nolibc/ /
ENV PATH=/usr/bin
