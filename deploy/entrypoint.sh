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
args=(-f --allow-other --format "${SACDFS_FORMATS:-dsf,wav}" --cache-mb "${SACDFS_CACHE_MB:-256}")
[ -n "${SACDFS_THREADS:-}" ] && args+=(--threads "$SACDFS_THREADS")
[ -n "${SACDFS_MAX_OPEN_IMAGES:-}" ] && args+=(--max-open-images "$SACDFS_MAX_OPEN_IMAGES")
[ -n "${SACDFS_PCM_RATE:-}" ] && args+=(--pcm-rate "$SACDFS_PCM_RATE")
[ -n "${SACDFS_PCM_CUTOFF:-}" ] && args+=(--pcm-cutoff "$SACDFS_PCM_CUTOFF")
[ -n "${SACDFS_PCM_GAIN:-}" ] && args+=(--pcm-gain "$SACDFS_PCM_GAIN")
on "${SACDFS_FLATTEN:-0}" && args+=(--flatten)
on "${SACDFS_MC_ALBUM_SUFFIX:-1}" || args+=(--no-multichannel-album-suffix)
[ -n "${SACDFS_SIDECARS:-}" ] && args+=(--sidecars "$SACDFS_SIDECARS")
case "${SACDFS_INDEX_CACHE:-/cache}" in
    "" | off | none | 0) args+=(--no-index-cache) ;;
    *) args+=(--index-cache "${SACDFS_INDEX_CACHE:-/cache}") ;;
esac
case "${SACDFS_VERBOSE:-1}" in 0) ;; 1) args+=(-v) ;; *) args+=(-vv) ;; esac

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
   map readonly = no
   # Nothing can change on a read-only mount: no change notifications, no oplock breaks.
   kernel change notify = no
   ${SMB_EXTRA_GLOBAL:-}

[$SHARE]
   path = $MNT
   comment = SACD images as DSF and WAV files (sacdfs)
   read only = yes
   browseable = yes
   $names
   $access
CONF
    testparm -s /etc/samba/smb.conf >/dev/null 2>&1 || die "invalid smb.conf (SMB_EXTRA_GLOBAL?)"
}

# --- NFS settings -----------------------------------------------------------------------
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
    FSAL { Name = VFS; }
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
    # until it has exited), at most 10 s.
    for _ in $(seq 100); do
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
log "sacdfs ${args[*]} $SOURCE $MNT"
sacdfs "${args[@]}" "$SOURCE" "$MNT" &
pids+=($!)
for _ in $(seq 600); do
    mountpoint -q "$MNT" && break
    kill -0 "${pids[0]}" 2>/dev/null || die "sacdfs exited (needs --device /dev/fuse and CAP_SYS_ADMIN)"
    sleep 0.1
done
mountpoint -q "$MNT" || die "sacdfs did not mount within 60 s"
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
