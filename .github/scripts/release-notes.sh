#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# The text of a release's notes on GitHub that is not from CHANGELOG.md (release.yml):
#
#     release-notes.sh full NOTES_MD     the whole notes: NOTES_MD (the CHANGELOG.md section, from
#                                        changelog-notes.py), then the downloads, with the macOS
#                                        lines if MACOS_RELEASE is "true"
#     release-notes.sh packages-later    the package lines, for a release that gets its packages
#                                        from a later run (job "packages-publish")
#     release-notes.sh macos-later RUN   the macOS lines, for a release that gets its macOS
#                                        archives from its release run RUN (job "macos-publish")
#
# To standard output. Reads VERSION (0.7.0, 0.7.0-rc.1) and GitHub's GITHUB_REPOSITORY; `full`
# also IMAGE and GITHUB_SHA; the other two GITHUB_SERVER_URL and GITHUB_RUN_ID. Each line of
# text is here once, so the notes of a release and of assets added to it later say the same.
set -euo pipefail

: "${VERSION:?}" "${GITHUB_REPOSITORY:?}"

packages() {
    local v=${VERSION/-/\~}
    echo "Linux packages of the binaries above, x86_64 and aarch64 (\`/usr/bin/sacdfs\` and a systemd unit, not enabled; they need fuse3; not signed with a package key: check the \`.sha256\` and the attestation; [packages guide](https://github.com/$GITHUB_REPOSITORY/blob/main/docs/install/packages.md)):"
    echo
    echo "- \`sacdfs-$v-1.x86_64.rpm\`, \`sacdfs-$v-1.aarch64.rpm\`: Fedora, RHEL 9 and 10."
    echo "- \`sacdfs_$v-1_amd64.deb\`, \`sacdfs_$v-1_arm64.deb\`: Debian 12 and 13, Ubuntu 22.04 and 24.04."
    echo "- \`sacdfs-$v-1.src.rpm\`: the source RPM, which rebuilds offline with the distribution's own Rust."
    echo
}

macos() {
    echo "macOS 11 or later, mounted by the Mac's own NFS client with nothing else installed (\`sacdfs-nfs\`; [macOS guide](https://github.com/$GITHUB_REPOSITORY/blob/main/docs/install/macos.md)). Not signed or notarised: download with \`curl\`, as the guide shows."
    echo
    echo "- \`sacdfs-$VERSION-aarch64-macos.tar.gz\`: Apple Silicon."
    echo "- \`sacdfs-$VERSION-x86_64-macos.tar.gz\`: Intel."
    echo
}

case ${1:-} in
    full)
        : "${IMAGE:?}" "${GITHUB_SHA:?}"
        cat "${2:?NOTES_MD}"
        echo
        echo "## Downloads"
        echo
        echo "Linux binaries, glibc 2.17 or later (RHEL 7 era and later), needing FUSE and, as a user other than root, FUSE 3's \`fusermount3\` (package fuse3) or FUSE 2's \`fusermount\`; root needs neither:"
        echo
        echo "- \`sacdfs-$VERSION-x86_64-linux.tar.gz\`: generic x86-64, for any x86_64 CPU."
        echo "- \`sacdfs-$VERSION-x86_64-v3-linux.tar.gz\`: x86-64-v3 (AVX2, most Intel and AMD CPUs since about 2015): about 20 % less CPU for WAV files. It stops with an illegal-instruction error on older CPUs."
        echo "- \`sacdfs-$VERSION-aarch64-linux.tar.gz\`: aarch64 (64-bit ARM)."
        echo "- \`sacdfs-$VERSION-i686-linux.tar.gz\`: 32-bit x86 (Pentium 4, Atom and later), static: it needs no C library. Install it by hand ([binary guide](https://github.com/$GITHUB_REPOSITORY/blob/main/docs/install/binary.md#32-bit-x86)); the installer and the image are 64-bit only."
        echo
        packages
        echo "Windows (x86_64, Windows 10 or 11): \`sacdfs-$VERSION-x86_64-windows.zip\`, \`sacdfs-win.exe\`, which needs [WinFsp](https://winfsp.dev) 2.1 or later ([Windows guide](https://github.com/$GITHUB_REPOSITORY/blob/main/docs/install/windows.md)). It uses WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos, https://github.com/winfsp/winfsp. Not code-signed."
        echo
        if [[ "${MACOS_RELEASE:-}" == true ]]; then macos; fi
        echo "Container image for a NAS (SMB and NFS): \`$IMAGE:$VERSION\` (generic CPU; amd64 and arm64)."
        echo
        echo "Installer: \`install.sh\` finds the system (a NAS or Linux computer) and sets sacdfs up ([install guide](https://github.com/$GITHUB_REPOSITORY/blob/main/docs/install/README.md)). Over SSH:"
        echo
        echo '```sh'
        echo "curl -fsSL https://github.com/$GITHUB_REPOSITORY/releases/latest/download/install.sh \\"
        echo "  | sudo sh -s -- --iso-dir /path/to/images"
        echo '```'
        echo
        echo "Verify a download: \`sha256sum -c <asset>.sha256\` and \`gh attestation verify <asset> -R $GITHUB_REPOSITORY\`; the image: \`gh attestation verify oci://$IMAGE:$VERSION -R $GITHUB_REPOSITORY\`. See the README, \"Install\"."
        echo
        echo "Built from $GITHUB_SHA."
        ;;
    packages-later)
        : "${GITHUB_SERVER_URL:?}" "${GITHUB_RUN_ID:?}"
        packages
        echo "The packages were added after the release, from its published archives and its tag's source, by a later run on \`main\` ($GITHUB_SERVER_URL/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID), which their attestation names."
        echo
        ;;
    macos-later)
        : "${GITHUB_SERVER_URL:?}" "${GITHUB_RUN_ID:?}"
        RUN=${2:?RUN}
        macos
        echo "The macOS archives were added after the release, from this tag's release run ($GITHUB_SERVER_URL/$GITHUB_REPOSITORY/actions/runs/$RUN); their attestation names the later run on \`main\` that published them ($GITHUB_SERVER_URL/$GITHUB_REPOSITORY/actions/runs/$GITHUB_RUN_ID)."
        echo
        ;;
    *)
        echo "usage: release-notes.sh full NOTES_MD | packages-later | macos-later RUN" >&2
        exit 2
        ;;
esac
