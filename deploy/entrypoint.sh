#!/bin/bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Container entry point: mounts sacdfs at /srv/sacd, then shares it read-only over SMB
# (smbd) and NFS (NFS-Ganesha). Everything logs to standard output. If any of the three
# stops, the others are stopped too and the container exits, so the runtime restarts it.
# Settings: environment variables, documented in docs/deployment.md.
set -euo pipefail

MNT=/srv/sacd
log() { printf '%s entrypoint: %s\n' "$(date -u +%FT%TZ)" "$*"; }
die() { log "error: $*"; exit 1; }
on() { case "${1,,}" in 1 | yes | true | on) return 0 ;; *) return 1 ;; esac; }

# --- sacdfs options ---------------------------------------------------------------------
SOURCE=${SACDFS_SOURCE:-/images}
[ -e "$SOURCE" ] || die "SACDFS_SOURCE $SOURCE does not exist (mount the images there, read-only)"
args=(-f --allow-other --format "${SACDFS_FORMATS:-dsf,wav}")
# Unset: sacdfs sizes the cache from the container's memory limit.
[ -n "${SACDFS_CACHE_MB:-}" ] && args+=(--cache-mb "$SACDFS_CACHE_MB")
[ -n "${SACDFS_LISTING_MB:-}" ] && args+=(--listing-mb "$SACDFS_LISTING_MB")
[ -n "${SACDFS_THREADS:-}" ] && args+=(--threads "$SACDFS_THREADS")
[ -n "${SACDFS_MAX_OPEN_IMAGES:-}" ] && args+=(--max-open-images "$SACDFS_MAX_OPEN_IMAGES")
[ -n "${SACDFS_REQUEST_TIMEOUT:-}" ] && args+=(--request-timeout "$SACDFS_REQUEST_TIMEOUT")
# Seconds sacdfs keeps serving files that clients hold open after the stop signal, before it
# exits anyway (#276). Keep the container's stop timeout above this plus the servers' 10 s.
if [ -n "${SACDFS_STOP_GRACE:-}" ]; then
    [[ $SACDFS_STOP_GRACE =~ ^(0|[1-9][0-9]{0,3})$ ]] ||
        die "SACDFS_STOP_GRACE must be a whole number of seconds from 0 to 9999 (got '$SACDFS_STOP_GRACE')"
    args+=(--stop-grace "$SACDFS_STOP_GRACE")
fi
[ -n "${SACDFS_PAUSES:-}" ] && args+=(--pauses "$SACDFS_PAUSES")
[ -n "${SACDFS_PCM_RATE:-}" ] && args+=(--pcm-rate "$SACDFS_PCM_RATE")
[ -n "${SACDFS_PCM_CUTOFF:-}" ] && args+=(--pcm-cutoff "$SACDFS_PCM_CUTOFF")
[ -n "${SACDFS_PCM_GAIN:-}" ] && args+=(--pcm-gain "$SACDFS_PCM_GAIN")
on "${SACDFS_FLATTEN:-0}" && args+=(--flatten)
on "${SACDFS_MC_ALBUM_SUFFIX:-1}" || args+=(--no-multichannel-album-suffix)
[ -n "${SACDFS_SIDECARS:-}" ] && args+=(--sidecars "$SACDFS_SIDECARS")
[ -n "${SACDFS_TAGS:-}" ] && args+=(--tags "$SACDFS_TAGS")
# Background lookups on MusicBrainz (#185): off unless asked; they need SACDFS_TAGS, writable.
if on "${SACDFS_MUSICBRAINZ:-0}"; then
    [ -n "${SACDFS_TAGS:-}" ] || die "SACDFS_MUSICBRAINZ=1 needs SACDFS_TAGS (a writable tags directory)"
    args+=(--musicbrainz)
    on "${SACDFS_LOOKUP_NAME:-0}" && args+=(--lookup-name)
    on "${SACDFS_LOOKUP_DURATIONS:-0}" && args+=(--lookup-durations)
fi
case "${SACDFS_INDEX_CACHE:-/cache}" in
    "" | off | none | 0) args+=(--no-index-cache) ;;
    *) args+=(--index-cache "${SACDFS_INDEX_CACHE:-/cache}") ;;
esac
case "${SACDFS_VERBOSE:-1}" in 0) ;; 1) args+=(-v) ;; *) args+=(-vv) ;; esac

# How long sacdfs may take to mount (#232): it lists every image first, which on a large
# library on a slow disk takes minutes. Keep the health check's start period at least as long.
MOUNT_TIMEOUT=${SACDFS_MOUNT_TIMEOUT:-300}
[[ $MOUNT_TIMEOUT =~ ^[1-9][0-9]{0,4}$ ]] && [ "$MOUNT_TIMEOUT" -le 86400 ] ||
    die "SACDFS_MOUNT_TIMEOUT must be a whole number of seconds from 1 to 86400 (got '$MOUNT_TIMEOUT')"

# --- SMB settings -----------------------------------------------------------------------
SHARE=${SHARE_NAME:-sacd}
[[ $SHARE =~ ^[A-Za-z0-9._-]{1,64}$ ]] || die "SHARE_NAME must be 1 to 64 of A-Z a-z 0-9 . _ -"
write_smb_conf() {
    local access
    if [ -n "${SMB_USER:-}" ]; then
        [[ $SMB_USER =~ ^[a-z_][a-z0-9_-]{0,31}$ ]] || die "SMB_USER must be a lower-case user name"
        local pw=${SMB_PASSWORD:-}
        if [ -z "$pw" ] && [ -n "${SMB_PASSWORD_FILE:-}" ]; then pw=$(<"$SMB_PASSWORD_FILE"); fi
        [ -n "$pw" ] || die "SMB_USER needs SMB_PASSWORD or SMB_PASSWORD_FILE"
        id "$SMB_USER" >/dev/null 2>&1 || useradd -M -N -s /sbin/nologin "$SMB_USER"
        printf '%s\n%s\n' "$pw" "$pw" | smbpasswd -s -a "$SMB_USER" >/dev/null
        access="valid users = $SMB_USER
   guest ok = no"
        log "SMB: share '$SHARE', user '$SMB_USER'"
    else
        access="guest ok = yes
   guest only = yes"
        log "SMB: share '$SHARE', guest access"
    fi
    # Track titles may hold characters Windows forbids in names (" * : < > ? \ |). Plain
    # Samba shows such a name mangled (0JL0SQ~I.DSF). "sfm" (the default) maps each to the
    # private-use character macOS and the Linux kernel client (mapposix, its default) map
    # back, so they see the real name; Windows shows a placeholder glyph. "fullwidth" shows
    # look-alikes (：) that read well everywhere, but a title that already holds one of
    # those look-alikes (common on Japanese discs) then cannot be opened. "none": mangled.
    local names="" map=""
    case "${SMB_NAME_MAPPING:-sfm}" in
        sfm) map="0x22:0xf020,0x2a:0xf021,0x3a:0xf022,0x3c:0xf023,0x3e:0xf024,0x3f:0xf025,0x5c:0xf026,0x7c:0xf027" ;;
        fullwidth) map="0x22:0xff02,0x2a:0xff0a,0x3a:0xff1a,0x3c:0xff1c,0x3e:0xff1e,0x3f:0xff1f,0x5c:0xff3c,0x7c:0xff5c" ;;
        none) ;;
        *) die "SMB_NAME_MAPPING must be sfm, fullwidth or none" ;;
    esac
    [ -n "$map" ] && names="vfs objects = catia
   catia:mappings = $map"
    cat >/etc/samba/smb.conf <<CONF
# Written by entrypoint.sh at every start; edits are lost.
[global]
   server role = standalone server
   server min protocol = SMB2_10
   map to guest = Bad User
   guest account = nobody
   load printers = no
   printing = bsd
   printcap name = /dev/null
   disable spoolss = yes
   disable netbios = yes
   smb ports = 445
   # Logs to standard output (smbd --debug-stdout), level from SMB_LOG_LEVEL.
   log level = ${SMB_LOG_LEVEL:-1}
   # sacdfs has no extended attributes or DOS attributes: don't ask for them.
   ea support = no
   store dos attributes = no
   map archive = no
   # Everything here is read-only, so say so: each file and folder gets the DOS read-only
   # attribute, from its mode (no extra request to sacdfs). Clients then don't offer writes (#217).
   map readonly = yes
   # Nothing can change on a read-only mount: no change notifications, no oplock breaks.
   kernel change notify = no
   # A folder's listing changes behind Samba's back (images added, art changed), and Samba
   # breaks a directory lease only for changes made through Samba, never for kernel change
   # notifications (#325). With leases, the Linux kernel client and Windows keep a folder's
   # old listing for as long as they keep using it (#205, #325). Without leases a new image
   # shows at the next listing; Windows then reopens a folder per file it looks at (#312).
   smb3 directory leases = no
   # The size and free space don't change while mounted: ask sacdfs once a minute, not for
   # every client's volume query (#205).
   dfree cache time = 60
   ${SMB_EXTRA_GLOBAL:-}

[$SHARE]
   path = $MNT
   comment = SACD images as DSF and WAV files (sacdfs)
   read only = yes
   browseable = yes
   # Samba reopens each folder it lists through /proc/self/fd with RESOLVE_NO_XDEV, which
   # fails there (the link leads out of /proc's mount), and then asks for the filesystem type
   # (one STATFS to sacdfs per folder) to rule out an automount point before it retries.
   # Nothing in this share is an automount point: open it directly (#205).
   vfs_default:VFS_OPEN_HOW_RESOLVE_NO_XDEV = no
   $names
   $access
CONF
    testparm -s /etc/samba/smb.conf >/dev/null 2>&1 || die "invalid smb.conf (SMB_EXTRA_GLOBAL?)"
}

# --- NFS settings -----------------------------------------------------------------------
# Under a memory limit, NFS-Ganesha's cache of entries is kept to what an eighth of the limit holds,
# at about 2 KiB each. Its default of 100,000 entries takes about 200 MiB of the limit when a client
# goes through a large library, and sacdfs can't free any of that. No limit: its defaults.
ganesha_cache_conf() {
    local limit
    limit=$(cat /sys/fs/cgroup/memory.max 2>/dev/null || cat /sys/fs/cgroup/memory/memory.limit_in_bytes 2>/dev/null || echo max)
    { [[ $limit =~ ^[0-9]+$ ]] && [ "${#limit}" -lt 16 ]; } || return 0
    local entries=$((limit / 8 / 2048))
    [ "$entries" -lt 100000 ] || return 0
    [ "$entries" -ge 1000 ] || entries=1000
    log "NFS: at most $entries entries cached, for the memory limit" >&2
    local chunks=$((entries / 128 + 1))
    printf 'MDCACHE {\n    Entries_HWMark = %d;\n    Chunks_HWMark = %d;\n    Chunks_LWMark = %d;\n}\n' \
        "$entries" "$chunks" $((chunks / 2))
}

write_ganesha_conf() {
    local clients=${NFS_CLIENTS:-*}
    [[ $clients =~ ^[][A-Za-z0-9.:/*,_-]+$ ]] || die "NFS_CLIENTS: a comma-separated list of hosts, networks (a.b.c.d/n) or *"
    cat >/etc/ganesha/ganesha.conf <<CONF
# Written by entrypoint.sh at every start; edits are lost.
NFS_CORE_PARAM {
    Protocols = 4;
    NFS_Port = 2049;
    Enable_NLM = false;
    Enable_RQUOTA = false;
    # PR_SET_IO_FLUSHER needs CAP_SYS_RESOURCE. It guards against memory-reclaim deadlocks
    # when a client on the same host writes through the server; nothing writes here.
    allow_set_io_flusher_fail = true;
}
NFSV4 {
    # A read-only export holds no state worth recovering: serve at once after a restart
    # instead of waiting out the 90 s grace period.
    Graceless = true;
    RecoveryBackend = fs;
    # 4.0 for macOS and libnfs, 4.1 and 4.2 for Linux (its default).
    Minor_Versions = 0, 1, 2;
}
NFS_KRB5 { Active_krb5 = false; }
$(ganesha_cache_conf)
EXPORT_DEFAULTS {
    Access_Type = None;
}
EXPORT {
    Export_Id = 1;
    Path = $MNT;
    Pseudo = /$SHARE;
    Protocols = 4;
    Transports = TCP;
    SecType = sys;
    Squash = all_squash;
    Anonymous_Uid = 65534;
    Anonymous_Gid = 65534;
    # FUSE has no device number that stays the same: give the export a fixed one.
    Filesystem_Id = 101.1;
    # What the clients see as the filesystem id is that one, but each file handle carries
    # Ganesha's own id of the backing filesystem. FUSE reports none, so Ganesha uses the
    # mount's device number, which changes when the kernel hands the next mount another one
    # (Podman: nearly every restart), and every handle then goes stale (#218). Major64 keeps
    # only the device's major number in the handle, always 0 for FUSE.
    FSAL { Name = VFS; fsid_type = Major64; }
    CLIENT {
        Clients = $clients;
        Access_Type = RO;
    }
}
LOG {
    Default_Log_Level = ${NFS_LOG_LEVEL:-EVENT};
    Facility { name = STDOUT; destination = STDOUT; enable = active; }
}
CONF
    log "NFS: export /$SHARE, clients $clients"
}

# --- checks before starting (#282) -----------------------------------------------------
# Each problem met on NAS container managers gets one line that says what to change, instead
# of a mount error that names the mount point.
# has_cap SET BIT: whether bit BIT is set in the capability set SET (CapEff, CapBnd) of
# /proc/self/status. Unknown counts as set: the mount then says what is wrong.
has_cap() {
    local key val
    while read -r key val; do
        [ "$key" = "$1:" ] || continue
        [[ $val =~ ^[0-9a-fA-F]{1,16}$ ]] || return 0
        (((16#$val >> $2) & 1)) && return 0 || return 1
    done </proc/self/status
    return 0
}
# port_taken PORT: whether a TCP socket listens on PORT in this network namespace, which with
# host networking is the host's (a NAS's own SMB and NFS hold 445 and 2049).
port_taken() {
    local want f _ local_addr _rem state _rest
    want=$(printf ':%04X' "$1")
    for f in /proc/net/tcp /proc/net/tcp6; do
        [ -r "$f" ] || continue
        while read -r _ local_addr _rem state _rest; do
            [ "$state" = 0A ] && [ "${local_addr: -5}" = "$want" ] && return 0
        done < <(tail -n +2 "$f")
    done
    return 1
}
# The security label the container runs under: an AppArmor profile, or an SELinux context.
security_label() {
    local f l
    for f in /proc/self/attr/apparmor/current /proc/self/attr/current; do
        l=$(tr -d '\0' <"$f" 2>/dev/null) || continue
        [ -n "$l" ] && { printf '%s' "$l"; return; }
    done
}
preflight() {
    [ -c /dev/fuse ] || die "the container has no /dev/fuse: add \`--device /dev/fuse\` (Compose: devices: [/dev/fuse]). If the host has none either, load FUSE there first, as root: \`modprobe fuse\`"
    has_cap CapEff 21 || die "the container may not mount (it lacks CAP_SYS_ADMIN): add \`--cap-add SYS_ADMIN\` (Compose: cap_add: [SYS_ADMIN])"
    if on "${NFS:-1}"; then
        has_cap CapEff 2 || log "warning: NFS needs CAP_DAC_READ_SEARCH, which the container lacks: add \`--cap-add DAC_READ_SEARCH\` (Compose: cap_add: [DAC_READ_SEARCH]), or set NFS=0"
        ! port_taken 2049 || die "port 2049 (NFS) is already in use here; with host networking the NAS's own NFS service holds it. Use bridge networking and publish another port (\`-p 12049:2049\`), or set NFS=0"
    fi
    if on "${SMB:-1}"; then
        ! port_taken 445 || die "port 445 (SMB) is already in use here; with host networking the NAS's own file sharing holds it. Use bridge networking and publish another port (\`-p 10445:445\`), or set SMB=0"
    fi
    # A host folder bound over /srv shares the mount with players on the same host
    # (docs/deployment.md, "Players on the same host"): only with shared propagation.
    if mountpoint -q /srv; then
        local line="" a b c d mp rest
        while read -r a b c d mp rest; do
            if [ "$mp" = /srv ]; then line="$a $b $c $d $mp $rest"; fi
        done </proc/self/mountinfo
        case " ${line%% - *} " in
            *" shared:"*) ;;
            *) log "warning: /srv is bound from the host without shared propagation, so players on the host won't see the mount (SMB and NFS still work). Bind it with \`:rshared\` (Compose: bind: propagation: rshared), from a host folder on a shared mount (docs/deployment.md, \"Players on the same host\")" ;;
        esac
    fi
}
preflight

# --- start ------------------------------------------------------------------------------
pids=()
# Stop in order: the servers first (NFS-Ganesha drops its exports and file handles, smbd
# its open files), then sacdfs, so that no server is left serving an unmounting or empty
# directory. Startup is the reverse: sacdfs mounts first, the servers start after (below).
stop_all() {
    trap - TERM INT
    log "stopping the servers"
    local p
    for p in "${pids[@]:1}"; do kill -TERM "$p" 2>/dev/null || true; done
    # Until no process but sacdfs uses the mount (NFS-Ganesha keeps the export's root open
    # until it has exited), at most 10 s. Only while the mount exists: once it is gone (sacdfs
    # was signalled too, or exited), fuser -m lists every process on the container's root
    # filesystem, which is never empty, and the loop would always run its full 10 s (#283).
    for _ in $(seq 100); do
        mountpoint -q "$MNT" || break
        users=$(fuser -m "$MNT" 2>/dev/null | tr -s ' ' '\n' | grep -vx -e '' -e "${pids[0]}" || true)
        [ -z "$users" ] && break
        sleep 0.1
    done
    for p in "${pids[@]:1}"; do kill -KILL "$p" 2>/dev/null || true; done
    log "stopping sacdfs"
    kill -TERM "${pids[0]}" 2>/dev/null || true
    for _ in $(seq 100); do
        mountpoint -q "$MNT" || break
        sleep 0.1
    done
    if mountpoint -q "$MNT"; then fusermount3 -u -z "$MNT" 2>/dev/null || true; fi
    wait 2>/dev/null || true
}
trap 'stop_all; exit 0' TERM INT

mountpoint -q "$MNT" && fusermount3 -u -z "$MNT" 2>/dev/null || true
# A host directory bound over /srv to share the mount with other containers on the same host
# (docs/deployment.md, "Players on the same host") hides the image's /srv/sacd: make it there.
# A dead mount left there by a sacdfs that was killed fails the mkdir ("Transport endpoint is
# not connected"); sacdfs detaches that one itself (#57), so the error is not fatal.
mkdir -p "$MNT" 2>/dev/null || true
log "sacdfs ${args[*]} $SOURCE $MNT"
sacdfs "${args[@]}" "$SOURCE" "$MNT" &
pids+=($!)
for _ in $(seq $((MOUNT_TIMEOUT * 10))); do
    mountpoint -q "$MNT" && break
    if ! kill -0 "${pids[0]}" 2>/dev/null; then
        case "$(security_label)" in
            *"(enforce)") die "sacdfs exited before mounting (its message is above). The container runs under the AppArmor profile '$(security_label)', which forbids mounting: add \`--security-opt apparmor=unconfined\` (Compose: security_opt: [apparmor=unconfined])" ;;
            *) die "sacdfs exited before mounting; its message above says why" ;;
        esac
    fi
    sleep 0.1
done
mountpoint -q "$MNT" || die "sacdfs did not mount within ${MOUNT_TIMEOUT} s (raise SACDFS_MOUNT_TIMEOUT for a large library on a slow disk)"
log "mounted"

if on "${SMB:-1}"; then
    write_smb_conf
    mkdir -p /run/samba /var/lib/samba/private /var/log/samba
    smbd --foreground --no-process-group --debug-stdout &
    pids+=($!)
fi
if on "${NFS:-1}"; then
    write_ganesha_conf
    mkdir -p /var/lib/nfs/ganesha /run/ganesha
    ganesha.nfsd -F -L STDOUT -f /etc/ganesha/ganesha.conf -p /run/ganesha/ganesha.pid &
    pids+=($!)
fi
printf '%s\n' "${pids[@]}" >/run/sacdfs-pids

# Any of them stopping ends the container.
set +e
wait -n "${pids[@]}"
rc=$?
log "a server stopped (status $rc)"
stop_all
exit $((rc == 0 ? 1 : rc))
