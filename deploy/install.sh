#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# sacdfs installer (#340): finds out where it runs and sets sacdfs up there, the way the
# guide in docs/install/ for that system does:
#   - with a container engine (Docker or Podman, and Compose): deploy/nas-compose.yaml;
#   - otherwise the release binary, with the systemd unit (deploy/systemd/sacdfs.service)
#     where systemd runs, or with deploy/sacdfs-ctl and the NAS's own start-at-boot hook
#     (Synology Task Scheduler, QNAP autorun.sh, ASUSTOR init.d);
#   - TrueNAS and Unraid manage their own apps: it only points to their app screens.
#
#   sh install.sh --iso-dir PATH [options]      (as root; see --help)
#
# Rules: plain POSIX sh (BusyBox ash is enough); a second run changes nothing; --dry-run
# shows every change and makes none; nothing is ever written under --iso-dir.
# docs/install/README.md says what was checked where.

# A download cut short (a pipe into sh that ends early) must not look like success: until
# main, on the last line, clears it, this makes any end of the script an error (#352).
trap 'printf "install.sh: the script ended before its last line: the download was cut short, and nothing was changed. Download it again.\n" >&2; exit 1' EXIT

DEFAULT_VERSION=0.6.0 # set to the release's version when the release packs this file
REPO=aclater/sacdfs
# Where releases are downloaded from: GitHub, or a mirror with the same layout
# (<url>/download/v<version>/<file>), as the CI test uses.
RELEASES=${SACDFS_RELEASES_URL:-https://github.com/$REPO/releases}

PATH=$PATH:/usr/local/sbin:/usr/local/bin:/usr/sbin:/sbin:/usr/bin:/bin:/usr/syno/sbin:/usr/syno/bin
export PATH

usage() {
    cat <<'EOF'
Usage: sh install.sh --iso-dir PATH [options]

Sets sacdfs up on this system: a container (Compose) where Docker or Podman runs, otherwise
the release binary with systemd or the NAS's own start-at-boot hook. Run it as root.

  --iso-dir PATH       the folder that holds your SACD images (.iso); never written to
  --share smb|nfs|both|none
                       container route: which servers to run (default both). Binary
                       routes: share the folder with your NAS's own file sharing
  --share-name NAME    SMB share and NFS export name (container), or the name of the
                       folder the DSF and WAV files appear in (binary); default sacd
  --mount DIR          binary routes: the folder to mount on (default: VOLUME/NAME)
  --prefix DIR         where sacdfs's own files go on a NAS (default: VOLUME/sacdfs;
                       /opt/sacdfs for a container on plain Linux)
  --method auto|compose|systemd|ctl
                       how to run sacdfs (default auto)
  --platform NAME      skip detection: synology qnap asustor terramaster ugreen readynas
                       openmediavault truenas unraid linux
  --version VER        the sacdfs release to install (default DEFAULT)
  --archive FILE       a downloaded release archive to use instead of downloading
  --dry-run            show every change, make none
  --uninstall          stop sacdfs and remove what this script installs
  -h, --help           this text
EOF
}

say() { printf '%s\n' "$*"; }
warn() { printf 'install.sh: warning: %s\n' "$*" >&2; }
die() {
    printf 'install.sh: %s\n' "$*" >&2
    exit 1
}

need_arg() { [ "$#" -ge 2 ] && [ -n "$2" ] || die "$1 needs a value (see --help)"; }

# ---------------------------------------------------------------------------------------
# Where are we?

detect_platform() {
    if [ -f /etc/unraid-version ]; then
        echo unraid
    elif [ -x /usr/bin/midclt ] || grep -qis truenas /etc/version 2>/dev/null; then
        echo truenas
    elif [ -f /etc.defaults/VERSION ] && [ -d /usr/syno ]; then
        echo synology
    elif [ -f /etc/config/uLinux.conf ]; then
        echo qnap
    elif [ -f /etc/nas.conf ] && [ -d /usr/builtin ]; then
        echo asustor
    elif [ -f /etc/tos/conf/tos.conf ] || [ -d /etc/tos ] || [ -x /usr/bin/tos ]; then
        echo terramaster
    elif [ -d /ugreen ] || [ -d /etc/ugreen ]; then
        echo ugreen
    elif [ -d /frontview ] || [ -x /usr/sbin/readynasd ]; then
        echo readynas
    elif [ -d /etc/openmediavault ]; then
        echo openmediavault
    else
        echo linux
    fi
}

# The data volume the images are on, for sacdfs's own files and the folder to mount on, as
# the guides use it: /volume1 (Synology, ASUSTOR, UGREEN), /Volume1 (TerraMaster), /data
# (ReadyNAS), a QNAP shared folder (/share/CACHEDEV1_DATA/Multimedia).
volume_of() {
    case $PLATFORM in
        qnap) printf '%s\n' "$1" | cut -d/ -f1-4 ;;
        linux | openmediavault) echo ;;
        *) printf '%s\n' "$1" | cut -d/ -f1-2 ;;
    esac
}

glibc_version() {
    v=$(getconf GNU_LIBC_VERSION 2>/dev/null | sed -n 's/^glibc \([0-9][0-9.]*\).*/\1/p')
    [ -n "$v" ] || v=$(ldd --version 2>&1 | sed -n '1s/.* \([0-9][0-9]*\.[0-9][0-9]*\)$/\1/p')
    echo "$v"
}
find_engine() {
    for d in /usr/local/bin /var/packages/ContainerManager/target/usr/bin \
        /share/CACHEDEV1_DATA/.qpkg/container-station/bin /Volume1/@apps/DockerEngine/dockerd/bin \
        /usr/local/AppCentral/docker-ce/bin; do
        [ -x "$d/docker" ] && case :$PATH: in *":$d:"*) ;; *) PATH=$PATH:$d ;; esac
    done
    if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
        if docker compose version >/dev/null 2>&1; then
            ENGINE=docker COMPOSE="docker compose"
        elif command -v docker-compose >/dev/null 2>&1; then
            ENGINE=docker COMPOSE=docker-compose
        fi
    fi
    if [ -z "$COMPOSE" ] && command -v podman >/dev/null 2>&1 && podman info >/dev/null 2>&1; then
        if command -v podman-compose >/dev/null 2>&1; then
            ENGINE=podman COMPOSE=podman-compose
        elif podman compose version >/dev/null 2>&1; then
            ENGINE=podman COMPOSE="podman compose"
        fi
    fi
}

# Never under the images folder: sacdfs would list its own files, and the images stay
# untouched. Nor the images under the mount, which would hide them.
inside() { case $1/ in "$2"/*) return 0 ;; esac; return 1; }
cleanup() {
    [ -n "$QNAP_CFG_MOUNTED" ] && umount "$QNAP_CFG_MOUNTED" 2>/dev/null
    [ -n "$TMP" ] && rm -rf "$TMP"
}

# run CMD...: run it, or in a dry run print it. Counts as a change.
run() {
    CHANGED=1
    if [ "$DRY" = 1 ]; then
        say "  would run: $*"
        return 0
    fi
    say "  running: $*"
    "$@"
}

# same A B: the two files have the same bytes (cmp isn't on every system).
same() {
    if command -v cmp >/dev/null 2>&1; then
        cmp -s "$1" "$2"
    else
        [ "$(sha256sum <"$1")" = "$(sha256sum <"$2")" ]
    fi
}

# put SRC DEST MODE: install a file if it differs. Returns 0 if it was (or would be) changed.
put() {
    if [ -f "$2" ] && same "$1" "$2"; then
        say "  unchanged: $2"
        return 1
    fi
    CHANGED=1
    if [ "$DRY" = 1 ]; then
        if [ -e "$2" ]; then say "  would replace: $2"; else say "  would write: $2"; fi
        return 0
    fi
    mkdir -p "$(dirname "$2")" || die "cannot make $(dirname "$2")"
    if ! { cp "$1" "$2.install.$$" && chmod "$3" "$2.install.$$" && mv -f "$2.install.$$" "$2"; }; then
        rm -f "$2.install.$$"
        die "cannot write $2"
    fi
    say "  wrote: $2"
    return 0
}

# remove FILE...: delete files this script installs.
remove() {
    for f in "$@"; do
        if [ -e "$f" ] || [ -L "$f" ]; then
            CHANGED=1
            if [ "$DRY" = 1 ]; then say "  would remove: $f"; else rm -f "$f" && say "  removed: $f"; fi
        fi
    done
}

# rmdir_empty DIR: remove a folder this script made, only if empty and not mounted.
mounted() { awk -v d="$1" '$2 == d { f = 1 } END { exit !f }' /proc/mounts; }
rmdir_empty() {
    [ -d "$1" ] || return 0
    mounted "$1" && return 0
    if [ -z "$(ls -A "$1" 2>/dev/null)" ]; then
        CHANGED=1
        if [ "$DRY" = 1 ]; then say "  would remove the empty folder: $1"; else rmdir "$1" && say "  removed the empty folder: $1"; fi
    fi
}

fetch() {
    if command -v curl >/dev/null 2>&1; then
        curl -fsSL --retry 3 -o "$2" "$1"
    elif command -v wget >/dev/null 2>&1; then
        # BusyBox's wget may be built without HTTPS: then it says so, and this fails.
        wget -q -O "$2" "$1"
    else
        die "neither curl nor wget is here to download $1: install one, or download the release archive yourself and give it with --archive"
    fi
}
get_archive() {
    [ -n "$PKGDIR" ] && return 0
    v=${VERSION:-$DEFAULT_VERSION}
    pkg=sacdfs-$v-$ARCH-linux
    if [ -n "$ARCHIVE" ]; then
        [ -f "$ARCHIVE" ] || die "--archive $ARCHIVE: no such file"
        if [ -f "$ARCHIVE.sha256" ]; then
            (cd "$(dirname "$ARCHIVE")" && sha256sum -c "$(basename "$ARCHIVE").sha256" >/dev/null) ||
                die "$ARCHIVE doesn't match $ARCHIVE.sha256"
            say "  checksum OK: $ARCHIVE"
        else
            warn "no $ARCHIVE.sha256 next to it: the archive isn't checked"
        fi
        tar -C "$TMP" -xzf "$ARCHIVE" || die "cannot unpack $ARCHIVE"
        pkg=''
        for d in "$TMP"/sacdfs-*-linux; do [ -d "$d" ] && pkg=${d##*/}; done
        [ -n "$pkg" ] || die "$ARCHIVE is not a sacdfs release archive"
    else
        url=$RELEASES/download/v$v/$pkg.tar.gz
        say "  downloading $url (to a temporary folder)"
        fetch "$url" "$TMP/$pkg.tar.gz" || die "download failed: $url (without curl, BusyBox's wget needs HTTPS support; or use --archive)"
        fetch "$url.sha256" "$TMP/$pkg.tar.gz.sha256" || die "download failed: $url.sha256"
        (cd "$TMP" && sha256sum -c "$pkg.tar.gz.sha256" >/dev/null) ||
            die "the download doesn't match its checksum; try again later"
        say "  checksum OK: $pkg.tar.gz"
        tar -C "$TMP" -xzf "$TMP/$pkg.tar.gz" || die "cannot unpack $pkg.tar.gz"
    fi
    PKGDIR=$TMP/$pkg
}

# local_file NAME: a release file next to this script, if it is in an unpacked archive or a
# clone's deploy/ folder (and no other release was asked for).
local_file() {
    [ -n "$SRC" ] && [ -z "$ARCHIVE" ] || return 1
    for f in "$SRC/$1" "$SRC/systemd/$1"; do
        [ -f "$f" ] && { echo "$f"; return 0; }
    done
    return 1
}

# file_of NAME: where a release file is. The binary comes from next to this script only
# when no --version was given.
file_of() {
    if [ "$1" != sacdfs ] || [ -z "$VERSION" ]; then local_file "$1" && return 0; fi
    [ -n "$PKGDIR" ] && [ -f "$PKGDIR/$1" ] && echo "$PKGDIR/$1"
}

# fetch_files NAME...: download and check the release archive if a file isn't at hand.
fetch_files() {
    for n in "$@"; do
        file_of "$n" >/dev/null || get_archive
        file_of "$n" >/dev/null ||
            die "$n isn't in the release archive (sacdfs ${VERSION:-$DEFAULT_VERSION} is older than this script)"
    done
}

# check_binary: the release binary runs here (glibc, CPU) before anything is installed.
check_binary() {
    if ! { cp "$(file_of sacdfs)" "$TMP/sacdfs-check" && chmod 0755 "$TMP/sacdfs-check"; }; then
        die "cannot copy the binary to $TMP"
    fi
    out=$("$TMP/sacdfs-check" --version 2>&1) || die "the sacdfs binary doesn't run here: $out (glibc 2.17 or later is needed; see $DOCS/binary.md#binaries)"
    say "  $out runs here"
}

ensure_fuse() {
    [ -e /dev/fuse ] && return 0
    run modprobe fuse || die "/dev/fuse is missing and 'modprobe fuse' failed"
}

host_address() {
    a=$(ip -4 route get 192.0.2.1 2>/dev/null | sed -n 's/.* src \([0-9.]*\).*/\1/p')
    [ -n "$a" ] || a=$(hostname 2>/dev/null)
    echo "${a:-this-nas}"
}

# port_open PORT: true when something answers on the port here (if a tool can tell).
port_open() {
    if command -v nc >/dev/null 2>&1; then
        nc -z -w 3 127.0.0.1 "$1" 2>/dev/null
    elif [ -r /proc/net/tcp ]; then
        hex=$(printf '%04X' "$1")
        cat /proc/net/tcp /proc/net/tcp6 2>/dev/null | awk -v p=":$hex" '$2 ~ p"$" && $4 == "0A" { f = 1 } END { exit !f }'
    else
        return 1
    fi
}

# ---------------------------------------------------------------------------------------
# Container route: deploy/nas-compose.yaml with the images folder and the shares filled in.

install_compose() {
    say "Route: container ($COMPOSE), $COMPOSE_FILE"
    case $ISO_DIR in *[:\\]*) die "--iso-dir can't contain ':' or '\\' for a container" ;; esac
    ensure_fuse
    fetch_files nas-compose.yaml
    src=$(file_of nas-compose.yaml)
    v=${VERSION:-}
    smb=1 nfs=1
    case $SHARE in
        smb) nfs=0 ;;
        nfs) smb=0 ;;
        none) die "--share none: the container only serves the shares; use --method systemd or ctl for a local mount" ;;
    esac
    selinux=0
    if [ -r /sys/fs/selinux/enforce ] && [ "$(cat /sys/fs/selinux/enforce)" = 1 ]; then selinux=1; fi
    # The images line and three settings, in deploy/nas-compose.yaml's own layout.
    awk -v iso="$ISO_DIR" -v name="$SHARE_NAME" -v smb="$smb" -v nfs="$nfs" '
        /^ *- \/volume1\/music:\/images:ro/ {
            ind = $0; sub(/-.*/, "", ind)
            print ind "- " iso ":/images:ro"; n++; next
        }
        { print }
        /^ *SACDFS_FLATTEN:/ {
            ind = $0; sub(/[^ ].*/, "", ind)
            print ind "SHARE_NAME: \"" name "\""
            print ind "SMB: \"" smb "\""
            print ind "NFS: \"" nfs "\""
            m++
        }
        END { if (n != 1 || m != 1) exit 1 }' "$src" >"$TMP/compose.yaml" ||
        die "$src doesn't have the images line and settings this script fills in"
    if [ -n "$v" ]; then
        sed "s|^\( *image: ghcr.io/aclater/sacdfs:\).*|\1$v|" "$TMP/compose.yaml" >"$TMP/c2" && mv "$TMP/c2" "$TMP/compose.yaml"
    fi
    if [ "$selinux" = 1 ]; then
        sed 's|^\( *\)# - label=disable.*|\1- label=disable|' "$TMP/compose.yaml" >"$TMP/c2" && mv "$TMP/c2" "$TMP/compose.yaml"
    fi
    changed=0
    put "$TMP/compose.yaml" "$COMPOSE_FILE" 0644 && changed=1
    running=$($ENGINE inspect -f '{{.State.Running}}' sacdfs 2>/dev/null)
    if [ "$changed" = 1 ] || [ "$running" != true ]; then
        run $COMPOSE -p sacdfs -f "$COMPOSE_FILE" up -d || die "'$COMPOSE up' failed; see '$ENGINE logs sacdfs'"
    else
        say "  the container runs with this file already"
    fi
    [ "$DRY" = 1 ] && return 0
    say "  waiting for the container to be healthy (up to 5 minutes for a large library)"
    i=0
    while [ "$i" -lt 60 ]; do
        h=$($ENGINE inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{end}}' sacdfs 2>/dev/null)
        [ "$h" = healthy ] && break
        [ "$h" = unhealthy ] && die "the container is unhealthy; see '$ENGINE logs sacdfs'"
        sleep 5
        i=$((i + 1))
    done
    [ "$h" = healthy ] || die "the container isn't healthy after 5 minutes; see '$ENGINE logs sacdfs'"
    say "  healthy"
    a=$(host_address)
    if [ "$smb" = 1 ]; then
        if port_open 10445; then say "  SMB answers on port 10445"; else warn "nothing answers on port 10445 here"; fi
    fi
    if [ "$nfs" = 1 ]; then
        if port_open 12049; then say "  NFS answers on port 12049"; else warn "nothing answers on port 12049 here"; fi
    fi
    say ""
    say "Done. Give a player:"
    [ "$smb" = 1 ] && say "  SMB: //$a/$SHARE_NAME on port 10445 (smb://$a:10445/$SHARE_NAME)"
    [ "$nfs" = 1 ] && say "  NFS: $a:/$SHARE_NAME on port 12049 (Linux: mount -t nfs -o port=12049,mountport=12049,vers=3 ...)"
    say "Stop it with: $COMPOSE -p sacdfs -f $COMPOSE_FILE stop (never kill -9)."
    say "Guides: $DOCS/linux-client.md and $DOCS/README.md (players)."
}

uninstall_compose() {
    [ -f "$COMPOSE_FILE" ] || return 0
    say "Removing the container route: $COMPOSE_FILE"
    if [ -n "$COMPOSE" ]; then
        run $COMPOSE -p sacdfs -f "$COMPOSE_FILE" down || warn "'$COMPOSE down' failed"
    else
        warn "no Compose answers here: stop the container 'sacdfs' yourself"
    fi
    remove "$COMPOSE_FILE"
    say "  (the index cache volume sacdfs_sacdfs-cache is kept; '$ENGINE volume rm sacdfs_sacdfs-cache' removes it)"
}

# ---------------------------------------------------------------------------------------
# Binary routes: the settings file both read, as in docs/install/nas-binary.md.

write_conf() {
    {
        say "# Written by install.sh (sacdfs). Plain NAME=value lines: systemd reads this file too."
        say "SACDFS_IMAGES=$ISO_DIR"
        say "SACDFS_MOUNT=$MOUNT"
        if [ "$METHOD" = ctl ]; then
            say "SACDFS_BIN=$BIN"
            say "SACDFS_OPTIONS=\"--index-cache $PREFIX/cache\""
        fi
    } >"$TMP/sacdfs.conf"
    case $ISO_DIR$MOUNT in *[\ \"\$\`\\]*) die "paths with spaces, quotes, \$ or \\ can't go in the settings file" ;; esac
    put "$TMP/sacdfs.conf" "$CONF" 0644
}

check_mount() {
    [ "$DRY" = 1 ] && return 0
    n=0
    for e in "$MOUNT"/*; do [ -e "$e" ] && n=$((n + 1)); done
    if mounted "$MOUNT"; then
        say "  mounted on $MOUNT: $n folders"
    else
        die "$MOUNT isn't mounted"
    fi
    say ""
    say "Done. The DSF and WAV files are in $MOUNT."
    if [ "$SHARE" != none ]; then
        say "To play them from other machines, share $MOUNT with $NAME's own file sharing"
        say "(SMB or NFS). Guide: $DOCS/$GUIDE"
    fi
}

install_systemd() {
    say "Route: release binary under systemd ($UNIT)"
    fetch_files sacdfs sacdfs.service
    check_binary
    ensure_fuse
    changed=0
    put "$(file_of sacdfs)" "$BIN" 0755 && changed=1
    put "$(file_of sacdfs.service)" "$UNIT" 0644 && { changed=1; run systemctl daemon-reload; }
    write_conf && changed=1
    if ! systemctl is-enabled sacdfs >/dev/null 2>&1; then run systemctl enable sacdfs; fi
    if systemctl is-active sacdfs >/dev/null 2>&1; then
        if [ "$changed" = 1 ]; then run systemctl restart sacdfs; else say "  sacdfs is running with these files already"; fi
    else
        run systemctl start sacdfs || die "sacdfs didn't start: journalctl -u sacdfs -n 20"
    fi
    if [ "$DRY" = 0 ]; then
        i=0
        while [ "$i" -lt 60 ] && ! mounted "$MOUNT"; do sleep 1; i=$((i + 1)); done
        systemctl is-active sacdfs >/dev/null 2>&1 || die "sacdfs isn't active: journalctl -u sacdfs -n 20"
        say "  $("$BIN" --version), systemctl is-active: active"
    fi
    check_mount
    say "Stop it with: systemctl stop sacdfs (never kill -9). Log: journalctl -u sacdfs"
}

uninstall_systemd() {
    UNIT=/etc/systemd/system/sacdfs.service
    [ -f "$UNIT" ] || return 0
    say "Removing the systemd route: $UNIT"
    systemctl is-active sacdfs >/dev/null 2>&1 && run systemctl stop sacdfs
    systemctl is-enabled sacdfs >/dev/null 2>&1 && run systemctl disable sacdfs
    remove "$UNIT" /etc/default/sacdfs /usr/local/bin/sacdfs
    run systemctl daemon-reload
}

# ---------------------------------------------------------------------------------------
# sacdfs-ctl and the NAS's start-at-boot hook.

ctl() { env SACDFS_CONF="$CONF" "$CTL" "$@"; }
runctl() { run env SACDFS_CONF="$CONF" "$CTL" "$@"; }
hook_install() {
    case $PLATFORM in
        synology)
            for t in $SYNO_TASKS; do
                if esynoscheduler --list 2>/dev/null | grep -q "\"$t\""; then
                    say "  Task Scheduler task $t: already there"
                    continue
                fi
                case $t in *start) ev=bootup op=start ;; *) ev=shutdown op=stop ;; esac
                [ -d /usr/syno/etc/esynoscheduler ] || run mkdir -p /usr/syno/etc/esynoscheduler
                run esynoscheduler --create task_name="$t" event="$ev" enable=true owner='{"0":"root"}' \
                    operation_type=script operation="SACDFS_CONF=$CONF $CTL $op" || die "esynoscheduler failed"
            done
            ;;
        asustor)
            d=/usr/local/etc/init.d
            {
                say "#!/bin/sh"
                say "# sacdfs (install.sh): ADM runs S* with start at boot and K* with stop at shut-down."
                say "export SACDFS_CONF=$CONF"
                say "case \$1 in"
                say "    start | stop | restart | status) exec $CTL \"\$1\" ;;"
                say "esac"
            } >"$TMP/S99sacdfs"
            put "$TMP/S99sacdfs" "$d/S99sacdfs" 0755
            if [ "$(readlink "$d/K01sacdfs" 2>/dev/null)" != S99sacdfs ]; then
                run ln -sf S99sacdfs "$d/K01sacdfs"
            else
                say "  unchanged: $d/K01sacdfs"
            fi
            ;;
        qnap)
            if [ "$(getcfg Misc Autorun -f /etc/config/uLinux.conf 2>/dev/null)" != TRUE ]; then
                run setcfg Misc Autorun TRUE -f /etc/config/uLinux.conf
                # Remembered, so that --uninstall switches it off again, and only then (#353).
                say "# Written by install.sh (sacdfs): it switched on QNAP's \"Run user defined processes\"; --uninstall switches it off." >"$TMP/autorun-mark"
                put "$TMP/autorun-mark" "$AUTORUN_MARK" 0644
            else
                say "  run user defined processes: already on"
            fi
            qnap_autorun install
            ;;
        *)
            warn "$NAME has no start-at-boot hook this script knows: sacdfs runs now, but won't come back after a restart."
            warn "Start it at boot with: SACDFS_CONF=$CONF $CTL start"
            ;;
    esac
}

hook_remove() {
    case $PLATFORM in
        synology)
            for t in $SYNO_TASKS; do
                if esynoscheduler --list 2>/dev/null | grep -q "\"$t\""; then
                    run esynoscheduler --delete task_name="$t" || warn "remove the Task Scheduler task $t in DSM"
                fi
            done
            ;;
        asustor) remove /usr/local/etc/init.d/K01sacdfs /usr/local/etc/init.d/S99sacdfs ;;
        qnap)
            qnap_autorun remove
            # "Run user defined processes": off again only if this script switched it on and
            # autorun.sh holds nothing else to run (#353).
            if [ "$(getcfg Misc Autorun -f /etc/config/uLinux.conf 2>/dev/null)" = TRUE ]; then
                if [ ! -f "$AUTORUN_MARK" ]; then
                    say "  run user defined processes: left on (this installer didn't switch it on)"
                elif grep -v '^[[:space:]]*#' "$TMP/autorun.sh" | grep -q '[^[:space:]]'; then
                    say "  run user defined processes: left on (autorun.sh runs other things too)"
                else
                    run setcfg Misc Autorun FALSE -f /etc/config/uLinux.conf
                fi
            fi
            remove "$AUTORUN_MARK"
            ;;
    esac
}
qnap_autorun() {
    cfg=/tmp/config
    if ! mounted "$cfg"; then
        dev=$(/sbin/hal_app --get_boot_pd port_id=0 2>/dev/null)
        [ -n "$dev" ] || die "can't find QNAP's boot flash (hal_app); see $DOCS/qnap.md step 10"
        mkdir -p "$cfg"
        mount "${dev}6" -t ext2 "$cfg" || die "can't mount ${dev}6 on $cfg"
        QNAP_CFG_MOUNTED=$cfg
    fi
    f=$cfg/autorun.sh
    if [ -f "$f" ]; then
        awk -v b="$BEGIN_MARK" -v e="$END_MARK" '$0 == b { s = 1 } !s { print } $0 == e { s = 0 }' "$f" >"$TMP/autorun.sh"
    else
        echo "#!/bin/sh" >"$TMP/autorun.sh"
    fi
    if [ "$1" = install ]; then
        cat >>"$TMP/autorun.sh" <<EOF
$BEGIN_MARK
# autorun.sh runs before the data volume is mounted: wait for it, then start sacdfs.
(
  i=0
  while [ ! -x $CTL ] && [ \$i -lt 120 ]; do sleep 5; i=\$((i + 1)); done
  SACDFS_CONF=$CONF $CTL start
) > /tmp/sacdfs-autorun.log 2>&1 &
$END_MARK
EOF
    fi
    if [ "$1" = remove ] && [ ! -f "$f" ]; then :; else put "$TMP/autorun.sh" "$f" 0755; fi
    if [ -n "$QNAP_CFG_MOUNTED" ]; then
        umount "$QNAP_CFG_MOUNTED" 2>/dev/null
        QNAP_CFG_MOUNTED=
    fi
}

install_ctl() {
    say "Route: release binary with sacdfs-ctl, files in $PREFIX"
    fetch_files sacdfs sacdfs-ctl
    check_binary
    ensure_fuse
    changed=0
    put "$(file_of sacdfs)" "$BIN" 0755 && changed=1
    put "$(file_of sacdfs-ctl)" "$CTL" 0755 && changed=1
    write_conf && changed=1
    [ -d "$PREFIX/cache" ] || run mkdir -p "$PREFIX/cache"
    [ -d "$MOUNT" ] || run mkdir -p "$MOUNT"
    hook_install
    if [ -x "$CTL" ] && [ -f "$CONF" ] && ctl status >/dev/null 2>&1; then
        if [ "$changed" = 1 ]; then runctl restart || die "sacdfs-ctl restart failed"; else say "  sacdfs is running with these files already"; fi
    else
        runctl start || die "sacdfs didn't start; see ${SACDFS_LOG:-/var/log/sacdfs.log}"
    fi
    [ "$DRY" = 1 ] || say "  $("$BIN" --version), $(ctl status)"
    check_mount
    say "Stop it with: SACDFS_CONF=$CONF $CTL stop (never kill -9)."
}

uninstall_ctl() {
    CTL=$PREFIX/sacdfs-ctl CONF=$PREFIX/sacdfs.conf
    [ -f "$CONF" ] || [ -f "$CTL" ] || return 0
    say "Removing the sacdfs-ctl route: $PREFIX"
    if [ -x "$CTL" ] && [ -f "$CONF" ]; then
        st=0
        ctl status >/dev/null 2>&1 || st=$?
        [ "$st" != 3 ] && runctl stop
    fi
    hook_remove
    remove "$PREFIX/sacdfs" "$CTL" "$CONF"
    # sacdfs's own index cache (rebuilt at the next start), made by this script.
    if [ -d "$PREFIX/cache" ]; then
        CHANGED=1
        if [ "$DRY" = 1 ]; then say "  would remove the index cache: $PREFIX/cache"; else rm -rf "$PREFIX/cache" && say "  removed the index cache: $PREFIX/cache"; fi
    fi
}

# ---------------------------------------------------------------------------------------
# Everything runs from here, on the last line, so a download cut short runs nothing.

# quote WORD: WORD in single quotes, safe to paste into a shell.
quote() { printf "'%s'" "$(printf '%s' "$1" | sed "s/'/'\\\\''/g")"; }

# parse_args "$@": the options, checked. No prompts ever (the one-liner has no terminal):
# what is missing is an error with the usage.
parse_args() {
    ISO_DIR='' SHARE=both SHARE_NAME=sacd MOUNT='' PREFIX='' METHOD=auto PLATFORM='' VERSION=''
    ARCHIVE='' DRY=0 UNINSTALL=0
    # The arguments again, quoted for the shell, for the line to re-run with sudo.
    ARGS_Q=''
    for a in "$@"; do ARGS_Q="$ARGS_Q $(quote "$a")"; done
    while [ "$#" -gt 0 ]; do
        case $1 in
            --iso-dir=*) ISO_DIR=${1#*=} ;;
            --iso-dir) need_arg "$@"; ISO_DIR=$2; shift ;;
            --share=*) SHARE=${1#*=} ;;
            --share) need_arg "$@"; SHARE=$2; shift ;;
            --share-name=*) SHARE_NAME=${1#*=} ;;
            --share-name) need_arg "$@"; SHARE_NAME=$2; shift ;;
            --mount=*) MOUNT=${1#*=} ;;
            --mount) need_arg "$@"; MOUNT=$2; shift ;;
            --prefix=*) PREFIX=${1#*=} ;;
            --prefix) need_arg "$@"; PREFIX=$2; shift ;;
            --method=*) METHOD=${1#*=} ;;
            --method) need_arg "$@"; METHOD=$2; shift ;;
            --platform=*) PLATFORM=${1#*=} ;;
            --platform) need_arg "$@"; PLATFORM=$2; shift ;;
            --version=*) VERSION=${1#*=} ;;
            --version) need_arg "$@"; VERSION=$2; shift ;;
            --archive=*) ARCHIVE=${1#*=} ;;
            --archive) need_arg "$@"; ARCHIVE=$2; shift ;;
            --dry-run) DRY=1 ;;
            --uninstall) UNINSTALL=1 ;;
            -h | --help)
                usage | sed "s/DEFAULT)/$DEFAULT_VERSION)/"
                exit 0
                ;;
            *) die "unknown option: $1 (see --help)" ;;
        esac
        shift
    done

    case $SHARE in smb | nfs | both | none) ;; *) die "--share must be smb, nfs, both or none" ;; esac
    case $METHOD in auto | compose | systemd | ctl) ;; *) die "--method must be auto, compose, systemd or ctl" ;; esac
    case $SHARE_NAME in
        '' | *[!A-Za-z0-9._-]*) die "--share-name: use letters, digits, '.', '_' and '-' only" ;;
    esac
    case $VERSION in *[!0-9A-Za-z.+-]*) die "--version: a version such as $DEFAULT_VERSION" ;; esac
    if [ -z "$ISO_DIR" ]; then
        usage | sed "s/DEFAULT)/$DEFAULT_VERSION)/" >&2
        die "--iso-dir PATH is required: the folder that holds your SACD images"
    fi
}

main() {
    # The whole script arrived (see the trap at the top).
    trap - EXIT
    # SELF: this script's file, when it runs from one; empty when it is piped into sh.
    SELF='' SRC=''
    case $0 in */install.sh | install.sh) [ -f "$0" ] && SELF=$0 ;; esac
    # Release files next to it (an unpacked release archive, or deploy/ in a clone) are used
    # instead of downloading them; never when piped.
    if [ -n "$SELF" ]; then
        here=$(cd "$(dirname "$SELF")" && pwd)
        [ -f "$here/sacdfs-ctl" ] && SRC=$here
    fi
    parse_args "$@"
    # sacdfs is Linux only. Stop on anything else before downloading or changing a thing (on
    # macOS the Linux binary was downloaded, then failed its check with a glibc hint).
    os=$(uname -s)
    if [ "$os" != Linux ]; then
        die "sacdfs runs on Linux only, and this system is $os: nothing was changed.
Run the installer on your NAS or Linux computer (over SSH), then play the discs from this
computer over the network: https://github.com/$REPO/blob/main/docs/deployment.md#mounting-the-shares-on-clients"
    fi
    [ -n "$PLATFORM" ] || PLATFORM=$(detect_platform)
    case $PLATFORM in
        synology) NAME="Synology DSM" GUIDE=synology.md ;;
        qnap) NAME="QNAP" GUIDE=qnap.md ;;
        asustor) NAME="ASUSTOR ADM" GUIDE=asustor.md ;;
        terramaster) NAME="TerraMaster TOS" GUIDE=terramaster.md ;;
        ugreen) NAME="UGREEN" GUIDE=ugreen-ugos.md ;;
        readynas) NAME="ReadyNAS OS" GUIDE=readynas.md ;;
        openmediavault) NAME="OpenMediaVault" GUIDE=openmediavault.md ;;
        truenas) NAME="TrueNAS" GUIDE=truenas-scale.md ;;
        unraid) NAME="Unraid" GUIDE=unraid.md ;;
        linux) NAME="Linux" GUIDE=nas-binary.md ;;
        *) die "--platform: unknown system '$PLATFORM' (see --help)" ;;
    esac
    DOCS=https://github.com/$REPO/blob/main/docs/install

    say "sacdfs installer: $NAME detected$([ "$DRY" = 1 ] && echo ' (dry run: nothing is changed)')"

    # TrueNAS and Unraid: their own app screens own the containers. Nothing to install here.
    case $PLATFORM in
        truenas)
            say "TrueNAS runs sacdfs as a custom app from its own Apps screen; this script doesn't"
            say "install behind it. Apps, Discover Apps, Custom App, Install via YAML, then follow:"
            say "  $DOCS/truenas-scale.md"
            say "(The app's YAML is deploy/truenas/sacdfs in the repository.)"
            exit 0
            ;;
        unraid)
            say "Unraid runs sacdfs from a Docker template in its own Docker screen; this script"
            say "doesn't install behind it. Follow:"
            say "  $DOCS/unraid.md"
            say "(The template is deploy/unraid/sacdfs.xml in the repository.)"
            exit 0
            ;;
    esac

    if [ "$(id -u)" != 0 ]; then
        if [ "$DRY" = 1 ]; then
            warn "not root: the dry run can't see everything a real run would"
        else
            # Nothing has been changed yet. Say exactly what to type; never call sudo here.
            if [ -n "$SELF" ]; then
                again="sudo sh $(quote "$SELF")$ARGS_Q"
            else
                again="curl -fsSL $RELEASES/download/v$DEFAULT_VERSION/install.sh | sudo sh -s --$ARGS_Q"
            fi
            die "sacdfs is installed as root, and you aren't (nothing was changed). Run it again with sudo:
  $again
(On a NAS without sudo, log in as root or admin first.)"
        fi
    fi

    # Resolve the images folder: symbolic links (QNAP's /share/Multimedia) to the real path.
    [ -d "$ISO_DIR" ] || die "--iso-dir $ISO_DIR is not a folder"
    ISO_DIR=$(cd -P "$ISO_DIR" && pwd -P) || die "cannot enter $ISO_DIR"
    [ "$ISO_DIR" != / ] || die "--iso-dir can't be /"
    VOLUME=$(volume_of "$ISO_DIR")

    # ---------------------------------------------------------------------------------------
    # The machine: architecture, glibc, FUSE, systemd, container engine.

    ARCH=$(uname -m)
    case $ARCH in
        x86_64 | amd64) ARCH=x86_64 ;;
        aarch64 | arm64) ARCH=aarch64 ;;
        *) ARCH_BAD=1 ;;
    esac
    GLIBC=$(glibc_version)

    if [ -d /run/systemd/system ]; then HAS_SYSTEMD=1; else HAS_SYSTEMD=0; fi

    # A container engine that answers, and a Compose for it. NAS packages install Docker
    # outside the login's PATH.
    COMPOSE=
    ENGINE=
    find_engine

    say "  CPU $(uname -m), glibc ${GLIBC:-unknown}, FUSE $([ -e /dev/fuse ] && echo present || echo 'not loaded'), systemd $([ "$HAS_SYSTEMD" = 1 ] && echo yes || echo no), containers ${COMPOSE:-none}"

    # How to run sacdfs here.
    if [ "$METHOD" = auto ]; then
        if [ -n "$COMPOSE" ]; then
            METHOD=compose
        elif [ "$PLATFORM" = synology ] || [ "$PLATFORM" = qnap ] || [ "$PLATFORM" = asustor ]; then
            # Their guides: sacdfs-ctl and the NAS's own hook (DSM has systemd, but its units
            # aren't where a user's service belongs).
            METHOD=ctl
        elif [ "$HAS_SYSTEMD" = 1 ]; then
            METHOD=systemd
        else
            METHOD=ctl
        fi
    fi
    case $METHOD in
        compose) [ -n "$COMPOSE" ] || [ "$UNINSTALL" = 1 ] || die "--method compose: no Docker or Podman with Compose answers here" ;;
        systemd) [ "$HAS_SYSTEMD" = 1 ] || die "--method systemd: systemd isn't running here" ;;
    esac
    if [ "$METHOD" != compose ] && [ "$UNINSTALL" = 0 ]; then
        [ -z "${ARCH_BAD:-}" ] || die "no release binary for $(uname -m); see $DOCS/binary.md#from-source"
    fi

    # The paths.
    if [ -z "$PREFIX" ]; then
        if [ -n "$VOLUME" ]; then
            PREFIX=$VOLUME/sacdfs
        elif [ "$METHOD" = compose ]; then
            PREFIX=/opt/sacdfs
        else
            PREFIX=/opt/sacdfs
        fi
    fi
    if [ -z "$MOUNT" ]; then
        if [ -n "$VOLUME" ]; then MOUNT=$VOLUME/$SHARE_NAME; else MOUNT=/srv/$SHARE_NAME; fi
    fi
    case $PREFIX in /*) PREFIX=${PREFIX%/} ;; *) die "--prefix must be an absolute path" ;; esac
    case $MOUNT in /*) MOUNT=${MOUNT%/} ;; *) die "--mount must be an absolute path" ;; esac
    if [ "$METHOD" = compose ]; then OWN="$PREFIX"; else OWN="$PREFIX $MOUNT"; fi
    for p in $OWN; do
        if inside "$p" "$ISO_DIR"; then
            die "$p is inside --iso-dir $ISO_DIR, and nothing may be written there: give --prefix and --mount outside it"
        fi
    done
    if [ "$METHOD" != compose ] && inside "$ISO_DIR" "$MOUNT"; then
        die "--iso-dir $ISO_DIR is inside the mount folder $MOUNT"
    fi
    if [ "$METHOD" = systemd ]; then
        BIN=/usr/local/bin/sacdfs CONF=/etc/default/sacdfs UNIT=/etc/systemd/system/sacdfs.service
    else
        BIN=$PREFIX/sacdfs CONF=$PREFIX/sacdfs.conf CTL=$PREFIX/sacdfs-ctl
    fi
    COMPOSE_FILE=$PREFIX/compose.yaml

    # ---------------------------------------------------------------------------------------
    # Doing things, or saying what would be done.

    CHANGED=0
    TMP=
    QNAP_CFG_MOUNTED=
    trap cleanup EXIT
    trap 'exit 130' INT TERM HUP
    TMP=$(mktemp -d 2>/dev/null || mktemp -d -t sacdfs-install.XXXXXX) || die "mktemp failed"
    # Some NAS systems mount /tmp noexec (Synology DSM): the binary is tried out before it's
    # installed, so then the temporary folder goes on the data volume (never under --iso-dir).
    printf '#!/bin/sh\nexit 0\n' >"$TMP/x" && chmod 0755 "$TMP/x"
    if ! "$TMP/x" 2>/dev/null; then
        rm -rf "$TMP"
        TMP=
        base=${VOLUME:-$(dirname "$PREFIX")}
        [ -d "$base" ] || die "/tmp doesn't allow running programs, and $base doesn't exist"
        TMP=$(mktemp -d "$base/.sacdfs-install.XXXXXX") || die "mktemp in $base failed"
    fi
    rm -f "$TMP/x"

    # The release files: sacdfs, sacdfs-ctl, sacdfs.service, nas-compose.yaml. From the folder
    # this script is in when it is an unpacked release archive or deploy/ of a clone (the files
    # that match this script), and the binary from the archive.

    PKGDIR=

    SYNO_TASKS="sacdfs-start sacdfs-stop"

    # QNAP's autorun.sh lives on the boot flash's config partition. Only the block between the
    # two marker lines is ours; whatever else the file holds stays.
    BEGIN_MARK="# sacdfs install.sh: begin"
    END_MARK="# sacdfs install.sh: end"
    # There when this script switched autorun on (see hook_install).
    AUTORUN_MARK=$PREFIX/qnap-autorun-switched-on

    # ---------------------------------------------------------------------------------------

    if [ "$UNINSTALL" = 1 ]; then
        uninstall_compose
        uninstall_systemd
        uninstall_ctl
        rmdir_empty "$MOUNT"
        rmdir_empty "$PREFIX"
        if [ "$CHANGED" = 0 ]; then say "Nothing of sacdfs's to remove here."; fi
        say "Your images in $ISO_DIR were not touched."
        exit 0
    fi

    case $METHOD in
        compose) install_compose ;;
        systemd) install_systemd ;;
        ctl) install_ctl ;;
    esac
    if [ "$CHANGED" = 0 ]; then say "Nothing changed: sacdfs was already set up like this."; fi
}

main "$@"
