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
# in), bad/install.sh (a wrong checksum built in), plain/install.sh (none built in),
# sacdfs-ctl, archive.tar.gz (the release archive served, its install.sh with the checksums
# of its release files) and other.tar.gz (another architecture's, served too, whose sacdfs
# fails the first time it runs and then prints a version). OLD: an earlier release's install.sh, sacdfs-ctl and sacdfs, for the upgrade.
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

# A folder to be made straight inside a sticky folder that all users can write to (as /tmp):
# refused, since another user could make it first; made by root beforehand, it is fine.
mkdir -p /s
chmod 1777 /s
for case in "prefix|--prefix /s/sacdfs" "mount|--mount /s/sacd"; do
    name=${case%%|*}
    # shellcheck disable=SC2086 # option and value, split on purpose
    set -- ${case#*|}
    inst --iso-dir "$music" --platform linux --method ctl --prefix /okvol/sacdfs "$@" >"$log" 2>&1
    rc=$?
    cat "$log"
    if [ "$rc" != 0 ] && grep -q 'could make it first' "$log" && [ -z "$(ls -A /s)$(ls -A /okvol)" ]; then
        res "new-folder-in-sticky-$name" "pass (refused, nothing written)"
    else
        res "new-folder-in-sticky-$name" "FAIL (status $rc)"
    fi
    rm -rf /s/* /okvol/*
done
mkdir -m 0755 /s/sacdfs
inst --iso-dir "$music" --platform linux --method ctl --prefix /s/sacdfs --dry-run >"$log" 2>&1
rc=$?
if [ "$rc" = 0 ]; then res "made-by-root-in-sticky" "pass (accepted)"; else cat "$log"; res "made-by-root-in-sticky" "FAIL (status $rc)"; fi
rm -rf /s

# Debian's /usr/local and /usr/local/bin, root:staff 2775: accepted only while "staff" has no
# members (listed, or as a primary group), and only those two folders.
cp /etc/group /tmp/group.orig
cp /etc/passwd /tmp/passwd.orig
grep -v '^staff:' /tmp/group.orig >/etc/group
echo 'staff:x:4050:' >>/etc/group
mkdir -p /usr/local/bin /usr/local/lib
chgrp 4050 /usr/local /usr/local/bin /usr/local/lib
chmod 2775 /usr/local /usr/local/bin /usr/local/lib
staffcase() { # NAME PREFIX EXPECT(accept|refuse) WORD
    inst --iso-dir "$music" --platform linux --method ctl --prefix "$2" --dry-run >"$log" 2>&1
    rc=$?
    cat "$log"
    if [ "$3" = accept ] && [ "$rc" = 0 ]; then
        res "staff-$1" "pass (accepted)"
    elif [ "$3" = refuse ] && [ "$rc" != 0 ] && grep -q -- "$4" "$log"; then
        res "staff-$1" "pass (refused, names $4)"
    else
        res "staff-$1" "FAIL (status $rc)"
    fi
}
staffcase empty-group /usr/local/bin/sacdfs-t accept
staffcase usr-local-lib /usr/local/lib/sacdfs-t refuse /usr/local/lib
sed -i 's/^staff:x:4050:$/staff:x:4050:alice/' /etc/group
staffcase member /usr/local/bin/sacdfs-t refuse alice
sed -i 's/^staff:x:4050:alice$/staff:x:4050:/' /etc/group
echo 'bob:x:4051:4050::/:/bin/sh' >>/etc/passwd
staffcase primary-group /usr/local/bin/sacdfs-t refuse bob
cp /tmp/passwd.orig /etc/passwd
chmod 2777 /usr/local/bin
staffcase other-writable /usr/local/bin/sacdfs-t refuse drwxrwsrwx
cp /tmp/group.orig /etc/group
chgrp 0 /usr/local /usr/local/bin /usr/local/lib
chmod 0755 /usr/local /usr/local/bin /usr/local/lib
rm -f /tmp/group.orig /tmp/passwd.orig

# A prefix inside the images folder, both with a space in their names: refused.
images "/vol3/my music"
inst --iso-dir "/vol3/my music" --platform linux --method ctl --prefix "/vol3/my music/sacdfs" >"$log" 2>&1
rc=$?
cat "$log"
if [ "$rc" != 0 ] && grep -q 'is inside --iso-dir' "$log" && [ ! -e "/vol3/my music/sacdfs" ]; then
    res "prefix-inside-iso-dir-with-space" "pass (refused)"
else
    res "prefix-inside-iso-dir-with-space" "FAIL (status $rc)"
    inst --iso-dir "/vol3/my music" --platform linux --method ctl --prefix "/vol3/my music/sacdfs" --uninstall >/dev/null 2>&1
fi
rm -rf /vol3

# /tmp that can't run programs: the binary would be tried on the data volume, which here
# other users can write to: refused before the download.
mkdir -p /nx /volume2/music
mount -t tmpfs -o noexec tmpfs /nx
chmod 0777 /volume2
cp "$music"/*.iso /volume2/music/
TMPDIR=/nx inst --iso-dir /volume2/music --platform asustor --method ctl --prefix /opt/nxp --mount /srv/nxm >"$log" 2>&1
rc=$?
cat "$log"
if [ "$rc" != 0 ] && grep -q 'tried from /volume2' "$log" && [ ! -e /opt/nxp ] && [ -z "$(find /volume2 -name '.sacdfs-install*')" ]; then
    res "noexec-tmp-unsafe-volume" "pass (refused)"
else
    res "noexec-tmp-unsafe-volume" "FAIL (status $rc)"
    TMPDIR=/nx inst --iso-dir /volume2/music --platform asustor --method ctl --prefix /opt/nxp --mount /srv/nxm --uninstall >/dev/null 2>&1
fi
umount /nx
rm -rf /volume2 /nx

# install.sh verifies release files beside it against built-in checksums. The release archive
# unpacked by a user (uid 1000) and run by root, as with sudo: its files are used, nothing is
# downloaded. One of them altered, next to the install.sh published on its own: none is used
# (the altered one leaves a mark if it runs), the release is downloaded and installed. A copy
# of install.sh with no checksums of release files built in uses none of them either.
mkdir -p /home/u1000
tar -C /home/u1000 -xzf "$t/archive.tar.gz"
lpkg=$(cd /home/u1000 && echo sacdfs-*-linux)
lv=/home/u1000/$lpkg
chown -R 1000 /home/u1000
before=$(fuse_mounts)
SACDFS_RELEASES_URL=$url sh "$lv/install.sh" --iso-dir "$music" --platform linux --method ctl --prefix /opt/lv --mount /srv/lv >"$log" 2>&1
rc=$?
cat "$log"
hashes /srv/lv >/tmp/got.sha 2>/dev/null
if [ "$rc" = 0 ] && grep -q "match the checksums built in ($lpkg): using them" "$log" && ! grep -q 'downloading' "$log" &&
    same /opt/lv/sacdfs "$lv/sacdfs" && same /opt/lv/sacdfs-ctl "$lv/sacdfs-ctl" && same /tmp/got.sha "$ref"; then
    res "verified-files-used" "pass (used, nothing downloaded, $(wc -l </tmp/got.sha | tr -d ' ') files identical)"
else
    res "verified-files-used" "FAIL (status $rc)"
fi
SACDFS_RELEASES_URL=$url sh "$lv/install.sh" --iso-dir "$music" --platform linux --method ctl --prefix /opt/lv --mount /srv/lv --uninstall >"$log" 2>&1
rc=$?
if [ "$rc" = 0 ] && [ ! -e /opt/lv ] && [ "$(fuse_mounts)" = "$before" ]; then res "verified-files-uninstall" pass; else cat "$log"; res "verified-files-uninstall" "FAIL (status $rc)"; fi
cp "$lv/sacdfs-ctl" /tmp/ctl.orig
cp "$t/install.sh" "$lv/install.sh"
printf '#!/bin/sh\ntouch /tmp/altered-ran\nexit 3\n' >"$lv/sacdfs-ctl"
rm -f /tmp/altered-ran
SACDFS_RELEASES_URL=$url sh "$lv/install.sh" --iso-dir "$music" --platform linux --method ctl --prefix /opt/lv --mount /srv/lv >"$log" 2>&1
rc=$?
cat "$log"
if [ "$rc" = 0 ] && [ ! -e /tmp/altered-ran ] && grep -q "sacdfs-ctl doesn't match the checksum built into install.sh" "$log" &&
    grep -q 'downloading' "$log" && grep -q 'checksum OK (built in)' "$log" && ! same /opt/lv/sacdfs-ctl "$lv/sacdfs-ctl" &&
    same /opt/lv/sacdfs-ctl /tmp/ctl.orig; then
    res "altered-file-not-used" "pass (none used, the release downloaded and installed)"
else
    res "altered-file-not-used" "FAIL (status $rc)"
fi
SACDFS_RELEASES_URL=$url sh "$t/install.sh" --iso-dir "$music" --platform linux --method ctl --prefix /opt/lv --mount /srv/lv --uninstall >/dev/null 2>&1
cp /tmp/ctl.orig "$lv/sacdfs-ctl"
cp "$t/plain/install.sh" "$lv/install.sh"
printf '#!/bin/sh\ntouch /tmp/altered-ran\necho "sacdfs 0.0.0-local"\n' >"$lv/sacdfs"
rm -f /tmp/altered-ran
SACDFS_RELEASES_URL=$url sh "$lv/install.sh" --iso-dir "$music" --platform linux --method ctl --prefix /opt/lv --dry-run >"$log" 2>&1
rc=$?
cat "$log"
if [ ! -e /tmp/altered-ran ] && grep -q 'has no checksums of them built in' "$log" && [ ! -e /opt/lv ]; then
    res "no-built-in-checksums-not-used" "pass (none used, status $rc)"
else
    res "no-built-in-checksums-not-used" "FAIL (status $rc)"
fi
rm -rf /home/u1000 /opt/lv /srv/lv /tmp/ctl.orig /tmp/altered-ran

# The install.sh published on its own takes only the release files of the archive it would
# download for this machine: next to it, another architecture's (all matching their
# checksums) are not used, and this machine's archive is downloaded. And a local binary that
# doesn't start is replaced by the download for this machine (here with uname reporting the
# other architecture, so that the other archive's files are this machine's): no stop.
mkdir -p /home/u1000
tar -C /home/u1000 -xzf "$t/other.tar.gz"
opkg=$(cd /home/u1000 && echo sacdfs-*-linux)
other=${opkg%-linux}
other=${other##*-}
lv=/home/u1000/$opkg
cp "$t/install.sh" "$lv/install.sh"
chown -R 1000 /home/u1000
rm -f /tmp/other-ran
SACDFS_RELEASES_URL=$url sh "$lv/install.sh" --iso-dir "$music" --platform linux --method ctl --prefix /opt/lv --dry-run >"$log" 2>&1
rc=$?
cat "$log"
if [ "$rc" = 0 ] && [ ! -e /tmp/other-ran ] && grep -q "they are $opkg's, and this machine takes" "$log" &&
    grep -q 'downloading .*/sacdfs-[^/]*-linux\.tar\.gz' "$log" && ! grep -q "downloading .*/$opkg\.tar\.gz" "$log" &&
    grep -q 'checksum OK (built in)' "$log"; then
    res "other-machines-files-not-used" "pass (not used, this machine's archive downloaded)"
else
    res "other-machines-files-not-used" "FAIL (status $rc)"
fi
mkdir -p /tmp/fakeuname
real_uname=$(command -v uname)
# shellcheck disable=SC2016 # $1 and $@ belong to the stand-in uname
printf '#!/bin/sh\ncase $1 in -m) echo %s ;; *) exec %s "$@" ;; esac\n' "$other" "$real_uname" >/tmp/fakeuname/uname
chmod 0755 /tmp/fakeuname/uname
rm -f /tmp/other-ran
PATH=/tmp/fakeuname:$PATH SACDFS_RELEASES_URL=$url sh "$lv/install.sh" --iso-dir "$music" --platform linux --method ctl --prefix /opt/lv --dry-run >"$log" 2>&1
rc=$?
cat "$log"
if [ "$rc" = 0 ] && grep -q "match the checksums built in ($opkg): using them" "$log" &&
    grep -q "the sacdfs next to install.sh doesn't run here" "$log" && grep -q "downloading .*/$opkg\.tar\.gz" "$log" &&
    grep -q 'checksum OK (built in)' "$log" && grep -q '0.0.0-other runs here' "$log"; then
    res "local-binary-not-starting-downloads" "pass (the download for this machine used, no stop)"
else
    res "local-binary-not-starting-downloads" "FAIL (status $rc)"
fi
rm -rf /home/u1000 /opt/lv /tmp/fakeuname /tmp/other-ran

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
    # The repository's copy refuses without --version (it names no release); the test gives the one it serves.
    ver=$(sed -n 's/^DEFAULT_VERSION=\([^ ]*\).*/\1/p' "$t/$name/install.sh")
    SACDFS_RELEASES_URL=$url sh "$t/$name/install.sh" --iso-dir "$music" --platform linux --method ctl --version "$ver" >"$log" 2>&1
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

# The old QNAP folder replaced by a link to another folder: nothing there is touched, by an
# install or by --uninstall.
victim() { # DIR: a folder with files that look like sacdfs's
    mkdir -p "$1/cache"
    echo compose >"$1/compose.yaml"
    echo conf >"$1/sacdfs.conf"
    echo important >"$1/cache/important"
}
victim_ok() { [ "$(cat "$1/compose.yaml" "$1/sacdfs.conf" "$1/cache/important" 2>/dev/null)" = "compose
conf
important" ]; }
victim /victim
rm -rf "$vol/Multimedia/sacdfs"
ln -s /victim "$vol/Multimedia/sacdfs"
inst --iso-dir "$vol/Multimedia/music" --platform qnap --method ctl >"$log" 2>&1
rc=$?
cat "$log"
if [ "$rc" = 0 ] && victim_ok /victim && grep -q 'symbolic link: nothing there' "$log" && [ "$(running_exe)" = "$vol/.sacdfs/sacdfs" ]; then
    res "qnap-old-folder-link-install" "pass (installed; the link's target untouched)"
else
    find /victim
    res "qnap-old-folder-link-install" "FAIL (status $rc)"
fi
inst --iso-dir "$vol/Multimedia/music" --platform qnap --method ctl --uninstall >"$log" 2>&1
rc=$?
cat "$log"
if [ "$rc" = 0 ] && victim_ok /victim && [ ! -e "$vol/.sacdfs" ] && [ "$(fuse_mounts)" = "$base" ]; then
    res "qnap-old-folder-link-uninstall" "pass (uninstalled; the link's target untouched)"
else
    find /victim
    res "qnap-old-folder-link-uninstall" "FAIL (status $rc)"
fi
rm -f "$vol/Multimedia/sacdfs"

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

# No hook names it (plain Linux): an earlier install in a shared folder is found through the
# running sacdfs's own path, and moved.
mkdir -p /pidshare
chmod 0777 /pidshare
sh "$old/install.sh" --iso-dir "$music" --platform linux --method ctl --prefix /pidshare/sacdfs >"$log" 2>&1
rc=$?
cat "$log"
inst --iso-dir "$music" --platform linux --method ctl >"$log" 2>&1
rc2=$?
cat "$log"
hashes /srv/sacd >/tmp/got.sha 2>/dev/null
if [ "$rc" = 0 ] && [ "$rc2" = 0 ] && [ "$(running_exe)" = /opt/sacdfs/sacdfs ] && [ ! -e /pidshare/sacdfs ] && same /tmp/got.sha "$ref"; then
    res "pid-found-old-install-moves" "pass (found through the running sacdfs, moved to /opt/sacdfs)"
else
    res "pid-found-old-install-moves" "FAIL (status $rc/$rc2, running $(running_exe))"
fi
inst --iso-dir "$music" --platform linux --method ctl --uninstall >/dev/null 2>&1
rm -rf /pidshare

# The hook names a settings file through a link: nothing there is touched by --uninstall.
victim /victim2
ln -s /victim2 /volume1/Public/lnk
printf '#!/bin/sh\nexport SACDFS_CONF=/volume1/Public/lnk/sacdfs.conf\n' >/usr/local/etc/init.d/S99sacdfs
inst --iso-dir /volume1/Public/music --platform asustor --method ctl --uninstall >"$log" 2>&1
rc=$?
cat "$log"
if [ "$rc" = 0 ] && victim_ok /victim2 && grep -q 'symbolic link: nothing there' "$log"; then
    res "asustor-hook-link-uninstall" "pass (the link's target untouched)"
else
    find /victim2
    res "asustor-hook-link-uninstall" "FAIL (status $rc)"
fi
rm -f /volume1/Public/lnk /usr/local/etc/init.d/S99sacdfs /usr/local/etc/init.d/K01sacdfs

exit "$fails"
