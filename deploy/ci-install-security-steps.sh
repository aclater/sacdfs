#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Who can change what root runs, and folder names with shell characters: deploy/install.sh
# and deploy/sacdfs-ctl as root, in a container. Plain POSIX sh, for BusyBox. Called by
# deploy/ci-install-test.sh (mode "security"):
#
#   ci-install-security-steps.sh URL T OLD MUSIC REF
#
# URL: the releases URL of that test's server. T: a folder with install.sh (checksums built
# in), bad/install.sh (a wrong checksum built in), plain/install.sh (none built in) and
# sacdfs-ctl. OLD: an earlier release's install.sh, sacdfs-ctl and sacdfs, for the upgrade.
# MUSIC: three corpus images (read-only); REF: sha256 lines of every file their mount shows.
# Prints "RESULT <step>: pass|FAIL".
url=$1 t=$2 old=$3 music=$4 ref=$5
fails=0
res() {
    echo "RESULT $1: $2"
    case $2 in pass*) ;; *) fails=$((fails + 1)) ;; esac
}
inst() { SACDFS_RELEASES_URL=$url sh "$t/install.sh" "$@"; }
same() { [ "$(sha256sum <"$1")" = "$(sha256sum <"$2")" ]; }
fuse_mounts() { grep -cE ' fuse(\.[^ ]+)? ' /proc/mounts; }
# hashes DIR: sha256 lines of every file under DIR, as REF has them.
hashes() { (cd "$1" && find . -type f | LC_ALL=C sort | while read -r f; do sha256sum "$f"; done); }
# images DIR: the corpus images copied into DIR (made, root's, 0755).
images() { mkdir -p "$1" && cp "$music"/*.iso "$1"/; }
log=/tmp/sec.log

# --- 1. A prefix or mount folder that other users can change: refused, nothing written.
mkdir -p /w /g /u /okvol
chmod 0777 /w
chmod 0775 /g
chown 1000 /u
chmod 0755 /u
for case in "w|--prefix /w/sacdfs" "g|--prefix /g/sacdfs" "u|--prefix /u/sacdfs" "w-mount|--mount /w/sacd" "g-deep|--prefix /g/a/b/sacdfs"; do
    name=${case%%|*}
    # shellcheck disable=SC2086 # option and value, split on purpose
    set -- ${case#*|}
    before=$(fuse_mounts)
    inst --iso-dir "$music" --platform linux --method ctl --prefix /okvol/sacdfs "$@" >"$log" 2>&1
    rc=$?
    cat "$log"
    if [ "$rc" != 0 ] && grep -q 'Nothing was changed' "$log" && [ -z "$(ls -A /w)$(ls -A /g)$(ls -A /u)$(ls -A /okvol)" ] &&
        [ "$(fuse_mounts)" = "$before" ]; then
        res "unsafe-folder-$name" "pass (refused, nothing written)"
    else
        res "unsafe-folder-$name" "FAIL (status $rc)"
        inst --iso-dir "$music" --platform linux --method ctl --prefix /okvol/sacdfs "$@" --uninstall >/dev/null 2>&1
    fi
    rm -rf /w/* /g/* /u/* /okvol/*
done

# --- 2. Folder names with &, spaces and brackets: a full install, the files, a second run
# that changes nothing, uninstall. In the images folder, the prefix and the mount folder.
for n in 'R&B' 'Rock & Roll' 'Live (1972)'; do
    images "/vol/$n"
    before=$(fuse_mounts)
    inst --iso-dir "/vol/$n" --platform linux --method ctl --prefix "/opt/$n" --mount "/srv/$n sacd" >"$log" 2>&1
    rc=$?
    cat "$log"
    hashes "/srv/$n sacd" >/tmp/got.sha 2>/dev/null
    if [ "$rc" = 0 ] && [ -s /tmp/got.sha ] && same /tmp/got.sha "$ref"; then
        res "name-install [$n]" "pass ($(wc -l </tmp/got.sha | tr -d ' ') files identical)"
    else
        res "name-install [$n]" "FAIL (status $rc)"
    fi
    inst --iso-dir "/vol/$n" --platform linux --method ctl --prefix "/opt/$n" --mount "/srv/$n sacd" >"$log" 2>&1
    if grep -q 'Nothing changed' "$log"; then res "name-second-run [$n]" pass; else cat "$log"; res "name-second-run [$n]" FAIL; fi
    inst --iso-dir "/vol/$n" --platform linux --method ctl --prefix "/opt/$n" --mount "/srv/$n sacd" --uninstall >"$log" 2>&1
    rc=$?
    if [ "$rc" = 0 ] && [ ! -e "/opt/$n" ] && [ ! -e "/srv/$n sacd" ] && [ "$(fuse_mounts)" = "$before" ]; then
        res "name-uninstall [$n]" pass
    else
        cat "$log"
        res "name-uninstall [$n]" "FAIL (status $rc)"
    fi
done

# --- 3. A folder name that is a command: refused before anything is written, and nothing runs.
images '/vol/a;id>/tmp/out'
rm -f /tmp/out
inst --iso-dir '/vol/a;id>/tmp/out' --platform linux --method ctl >"$log" 2>&1
rc=$?
cat "$log"
if [ "$rc" != 0 ] && [ ! -e /tmp/out ] && [ ! -e /opt/sacdfs ] && [ ! -e /srv/sacd ]; then
    res "command-in-name-installer" "pass (refused, nothing ran or was written)"
else
    res "command-in-name-installer" "FAIL (status $rc)"
    inst --iso-dir '/vol/a;id>/tmp/out' --platform linux --method ctl --uninstall >/dev/null 2>&1
fi

# --- 4. sacdfs-ctl and settings files: as older installers wrote them (bare values, and
# SACDFS_OPTIONS with the index cache), read as data; a command in a value never runs.
mkdir -p /opt/c /srv
cp "$t/sacdfs-ctl" "$old/sacdfs" /opt/c/
chmod 0755 /opt/c /opt/c/sacdfs-ctl /opt/c/sacdfs
images '/data/R&B'
mkdir -p /opt/c/cache
cat >/opt/c/sacdfs.conf <<'EOF'
# Written by install.sh (sacdfs). Plain NAME=value lines: systemd reads this file too.
SACDFS_IMAGES=/data/R&B
SACDFS_MOUNT=/srv/oldconf
SACDFS_BIN=/opt/c/sacdfs
SACDFS_OPTIONS="--index-cache /opt/c/cache"
EOF
chmod 0644 /opt/c/sacdfs.conf
ctl() { SACDFS_CONF=/opt/c/sacdfs.conf /opt/c/sacdfs-ctl "$@"; }
ctl start >"$log" 2>&1
rc=$?
cat "$log"
hashes /srv/oldconf >/tmp/got.sha 2>/dev/null
args=$(tr '\000' ' ' <"/proc/$(cat /var/run/sacdfs.pid 2>/dev/null)/cmdline" 2>/dev/null)
if [ "$rc" = 0 ] && [ -s /tmp/got.sha ] && same /tmp/got.sha "$ref" && case $args in *' --index-cache /opt/c/cache '*) true ;; *) false ;; esac then
    res "old-conf-read" "pass (bare R&B path, index cache option; $(wc -l </tmp/got.sha | tr -d ' ') files identical)"
else
    res "old-conf-read" "FAIL (status $rc)"
fi
ctl stop >/dev/null 2>&1
rm -f /tmp/out
printf 'SACDFS_IMAGES=/x/a;id>/tmp/out\nSACDFS_MOUNT=/srv/x\nSACDFS_BIN=/opt/c/sacdfs\n' >/opt/c/sacdfs.conf
ctl status >"$log" 2>&1
cat "$log"
if [ ! -e /tmp/out ]; then res "command-in-conf-value" "pass (not run)"; else res "command-in-conf-value" FAIL; fi
# shellcheck disable=SC2016 # a command substitution, written as text on purpose
printf 'SACDFS_IMAGES=$(touch /tmp/out)\nSACDFS_MOUNT=/srv/x\n' >/opt/c/sacdfs.conf
ctl status >"$log" 2>&1
rc=$?
cat "$log"
if [ "$rc" = 1 ] && [ ! -e /tmp/out ] && grep -q 'never run as shell code' "$log"; then
    res "command-substitution-in-conf" "pass (refused, not run)"
else
    res "command-substitution-in-conf" "FAIL (status $rc)"
fi

# --- 5. As root, sacdfs-ctl refuses a settings file, program or its own folder that another
# user could change.
printf 'SACDFS_IMAGES=/data/R&B\nSACDFS_MOUNT=/srv/x\nSACDFS_BIN=/opt/c/sacdfs\n' >/opt/c/sacdfs.conf
for case in "conf-owner|chown 1000 /opt/c/sacdfs.conf" "conf-group-writable|chmod 0664 /opt/c/sacdfs.conf" \
    "bin-owner|chown 1000 /opt/c/sacdfs" "own-folder-writable|chmod 0777 /opt/c" "own-file-writable|chmod 0757 /opt/c/sacdfs-ctl"; do
    name=${case%%|*}
    ${case#*|}
    ctl status >"$log" 2>&1
    rc=$?
    cat "$log"
    if [ "$rc" = 1 ] && grep -q 'not run as root' "$log"; then res "ctl-refuses-$name" "pass (refused)"; else res "ctl-refuses-$name" "FAIL (status $rc)"; fi
    chown 0 /opt/c /opt/c/sacdfs.conf /opt/c/sacdfs /opt/c/sacdfs-ctl
    chmod 0755 /opt/c /opt/c/sacdfs /opt/c/sacdfs-ctl
    chmod 0644 /opt/c/sacdfs.conf
done
ctl status >"$log" 2>&1
rc=$?
if [ "$rc" = 3 ]; then res "ctl-accepts-root-only" pass; else cat "$log"; res "ctl-accepts-root-only" "FAIL (status $rc)"; fi
rm -rf /opt/c /srv/oldconf /srv/x

# --- 6. Downloads: a wrong checksum built in, or none over plain HTTP: refused.
for case in "bad|doesn't match the checksum built into" "plain|no checksum built in"; do
    name=${case%%|*}
    SACDFS_RELEASES_URL=$url sh "$t/$name/install.sh" --iso-dir "$music" --platform linux --method ctl >"$log" 2>&1
    rc=$?
    cat "$log"
    if [ "$rc" != 0 ] && grep -q "${case#*|}" "$log" && [ ! -e /opt/sacdfs ] && [ ! -e /srv/sacd ]; then
        res "download-$name-checksum" "pass (refused, nothing installed)"
    else
        res "download-$name-checksum" "FAIL (status $rc)"
        SACDFS_RELEASES_URL=$url sh "$t/$name/install.sh" --iso-dir "$music" --platform linux --method ctl --uninstall >/dev/null 2>&1
    fi
done

# --- 7. Upgrade on QNAP (getcfg and setcfg stand-ins, /tmp/config a tmpfs): an earlier
# install in the shared folder is moved to the volume's .sacdfs, autorun.sh follows, the old
# files go; then --uninstall. And --uninstall straight from an earlier install.
vol=/share/CACHEDEV1_DATA
mkdir -p /etc/config "$vol/Multimedia"
chmod 0755 /share "$vol"
chmod 0777 "$vol/Multimedia"
images "$vol/Multimedia/music"
printf '[Misc]\n' >/etc/config/uLinux.conf
cat >/usr/bin/getcfg <<'EOF'
#!/bin/sh
sed -n "s/^$1\.$2 = //p" "$4" 2>/dev/null
EOF
cat >/usr/bin/setcfg <<'EOF'
#!/bin/sh
grep -v "^$1\.$2 = " "$5" >"$5.new"; echo "$1.$2 = $3" >>"$5.new"; mv "$5.new" "$5"
EOF
chmod 0755 /usr/bin/getcfg /usr/bin/setcfg
autorun_has() { grep -qF "$1" /tmp/config/autorun.sh 2>/dev/null; }
running_exe() { readlink "/proc/$(cat /var/run/sacdfs.pid 2>/dev/null)/exe" 2>/dev/null; }
old_install() {
    sh "$old/install.sh" --iso-dir "$vol/Multimedia/music" --platform qnap --method ctl >"$log" 2>&1
    rc=$?
    cat "$log"
    if [ "$rc" = 0 ] && [ -f "$vol/Multimedia/sacdfs/sacdfs-ctl" ] && autorun_has "$vol/Multimedia/sacdfs/sacdfs-ctl" &&
        [ "$(running_exe)" = "$vol/Multimedia/sacdfs/sacdfs" ]; then
        res "qnap-old-install$1" "pass (the earlier release's layout, in the shared folder)"
    else
        res "qnap-old-install$1" "FAIL (status $rc)"
    fi
}
base=$(fuse_mounts)
old_install ""
inst --iso-dir "$vol/Multimedia/music" --platform qnap --method ctl >"$log" 2>&1
rc=$?
cat "$log"
hashes "$vol/sacd" >/tmp/got.sha 2>/dev/null
# shellcheck disable=SC2012 # the mode string of .sacdfs
if [ "$rc" = 0 ] && [ "$(running_exe)" = "$vol/.sacdfs/sacdfs" ] && [ -s /tmp/got.sha ] && same /tmp/got.sha "$ref" &&
    [ ! -e "$vol/Multimedia/sacdfs" ] && [ ! -e "$vol/Multimedia/sacd" ] &&
    autorun_has "'$vol/.sacdfs/sacdfs-ctl'" && ! autorun_has Multimedia && [ "$(fuse_mounts)" = $((base + 1)) ] &&
    [ -f "$vol/.sacdfs/qnap-autorun-switched-on" ] && [ "$(LC_ALL=C ls -ldn "$vol/.sacdfs" | cut -c1-10)" = drwxr-xr-x ]; then
    res "qnap-upgrade-moves" "pass (running from $vol/.sacdfs, old files gone, autorun.sh updated)"
else
    ls -la "$vol" "$vol/Multimedia" "$vol/.sacdfs" 2>&1
    cat /tmp/config/autorun.sh
    res "qnap-upgrade-moves" "FAIL (status $rc, running $(running_exe))"
fi
inst --iso-dir "$vol/Multimedia/music" --platform qnap --method ctl --uninstall >"$log" 2>&1
rc=$?
cat "$log"
if [ "$rc" = 0 ] && [ ! -e "$vol/.sacdfs" ] && [ ! -e "$vol/sacd" ] && ! autorun_has sacdfs &&
    [ "$(getcfg Misc Autorun -f /etc/config/uLinux.conf)" = FALSE ] && [ "$(fuse_mounts)" = "$base" ]; then
    res "qnap-uninstall" "pass (all gone, autorun switched off again)"
else
    res "qnap-uninstall" "FAIL (status $rc)"
fi
old_install "-again"
inst --iso-dir "$vol/Multimedia/music" --platform qnap --method ctl --uninstall >"$log" 2>&1
rc=$?
cat "$log"
if [ "$rc" = 0 ] && [ ! -e "$vol/Multimedia/sacdfs" ] && [ ! -e "$vol/Multimedia/sacd" ] && ! autorun_has sacdfs &&
    [ "$(fuse_mounts)" = "$base" ] && [ -z "$(running_exe)" ]; then
    res "qnap-uninstall-finds-old" "pass (the earlier install in the shared folder removed)"
else
    ls -la "$vol/Multimedia" 2>&1
    res "qnap-uninstall-finds-old" "FAIL (status $rc)"
fi
umount "$vol/Multimedia/sacd" 2>/dev/null

# --- 8. Upgrade on ASUSTOR (init.d): an earlier install with --prefix in a shared folder is
# found through S99sacdfs and moved to the default.
mkdir -p /volume1/Public /usr/local/etc/init.d
chmod 0755 /volume1
chmod 0777 /volume1/Public
images /volume1/Public/music
sh "$old/install.sh" --iso-dir /volume1/Public/music --platform asustor --method ctl --prefix /volume1/Public/sacdfs >"$log" 2>&1
rc=$?
cat "$log"
if [ "$rc" = 0 ] && [ "$(running_exe)" = /volume1/Public/sacdfs/sacdfs ]; then
    res "asustor-old-install" "pass (the earlier release, --prefix in a shared folder)"
else
    res "asustor-old-install" "FAIL (status $rc)"
fi
inst --iso-dir /volume1/Public/music --platform asustor --method ctl >"$log" 2>&1
rc=$?
cat "$log"
hashes /volume1/sacd >/tmp/got.sha 2>/dev/null
if [ "$rc" = 0 ] && [ "$(running_exe)" = /volume1/sacdfs/sacdfs ] && same /tmp/got.sha "$ref" &&
    [ ! -e /volume1/Public/sacdfs ] && grep -qF "'/volume1/sacdfs/sacdfs-ctl'" /usr/local/etc/init.d/S99sacdfs; then
    res "asustor-upgrade-moves" "pass (found through S99sacdfs, moved to /volume1/sacdfs)"
else
    cat /usr/local/etc/init.d/S99sacdfs
    res "asustor-upgrade-moves" "FAIL (status $rc, running $(running_exe))"
fi
inst --iso-dir /volume1/Public/music --platform asustor --method ctl --uninstall >"$log" 2>&1
rc=$?
if [ "$rc" = 0 ] && [ ! -e /volume1/sacdfs ] && [ ! -e /usr/local/etc/init.d/S99sacdfs ] && [ "$(fuse_mounts)" = "$base" ]; then
    res "asustor-uninstall" pass
else
    cat "$log"
    res "asustor-uninstall" "FAIL (status $rc)"
fi

exit "$fails"
