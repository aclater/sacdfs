#!/bin/sh
# SPDX-License-Identifier: GPL-3.0-or-later
#
# deploy/install.sh on systems it can't support (#382): with a fake uname, getconf and ldd
# (and a fake docker for the container route), each case must stop with status 1, print the
# system report and the link to the form, change nothing, and make no request to the releases
# URL it is given. Then --report: status 0, the block, nothing written, and no host name or
# --iso-dir in it. Plain POSIX sh, for BusyBox. Called by deploy/ci-install-test.sh:
#
#   ci-install-unsupported-steps.sh SCRIPT URL LOG
#
# SCRIPT: install.sh, fed to sh on stdin as the one-liner does (so nothing next to it is used);
# URL: a releases URL whose server logs every request to LOG. Prints "RESULT <case>: pass|FAIL".
script=$1 url=$2 log=$3
fails=0
res() {
    echo "RESULT $1: $2"
    case $2 in pass*) ;; *) fails=$((fails + 1)) ;; esac
}
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
iso=$work/isodir-must-not-show-up
mkdir -p "$iso"
real_uname=$(command -v uname)
real_getconf=$(command -v getconf || echo false)
same() { [ "$(sha256sum <"$1")" = "$(sha256sum <"$2")" ]; }
state() {
    ls -d /opt/sacdfs /srv/sacd /etc/default/sacdfs /etc/systemd/system/sacdfs.service \
        /usr/local/bin/sacdfs "$work/sacdfs" "$work/sacd" 2>/dev/null
    grep -cE ' fuse(\.[^ ]+)? ' /proc/mounts
}
requests() { grep -c '"GET ' "$log" 2>/dev/null || true; }

# fakes DIR MACHINE LONG_BIT LIBC: uname -m, getconf LONG_BIT, and the C library version
# ("glibc 2.12", "musl 1.2.4", or "real").
fakes() {
    mkdir -p "$1"
    cat >"$1/uname" <<EOF
#!/bin/sh
case \$1 in
    -m) echo $2 ;;
    -s) echo Linux ;;
    *) exec $real_uname "\$@" ;;
esac
EOF
    case $4 in
        glibc*) libc="GNU_LIBC_VERSION) echo '$4' ;;" ;;
        musl*) libc="GNU_LIBC_VERSION) echo 'getconf: Invalid argument' >&2; exit 1 ;;" ;;
        *) libc='' ;;
    esac
    cat >"$1/getconf" <<EOF
#!/bin/sh
case \$1 in
    LONG_BIT) echo $3 ;;
    $libc
    *) exec $real_getconf "\$@" ;;
esac
EOF
    if [ "${4%% *}" = musl ]; then
        # musl's ldd: two lines on stderr, status 1.
        printf '#!/bin/sh\necho "musl libc (x86_64)" >&2\necho "Version %s" >&2\nexit 1\n' "${4#musl }" >"$1/ldd"
    fi
    chmod 0755 "$1"/*
}

# stops NAME FAKEDIR EXPECT [install.sh options...]: status 1, the block with EXPECT as the
# check that stopped it, the form's link, nothing changed, no request.
stops() {
    name=$1 dir=$2 expect=$3
    shift 3
    echo "=== $name: $*"
    state >"$work/state0"
    before=$(requests)
    PATH="$dir:$PATH" SACDFS_RELEASES_URL=$url sh -s -- --iso-dir "$iso" "$@" <"$script" >"$work/out" 2>&1
    rc=$?
    cat "$work/out"
    state >"$work/state1"
    after=$(requests)
    why=
    [ "$rc" = 1 ] || why="$why status $rc;"
    grep -q '^--- sacdfs system report' "$work/out" || why="$why no report;"
    grep -q "^stopped by: *$expect" "$work/out" || why="$why not stopped by '$expect';"
    grep -q 'issues/new?template=unsupported-system.yml' "$work/out" || why="$why no link;"
    grep -q 'downloading' "$work/out" && why="$why downloading;"
    same "$work/state0" "$work/state1" || why="$why something changed;"
    [ "$before" = "$after" ] || why="$why $((after - before)) requests;"
    if [ -n "$(ls "$dir/docker.log" 2>/dev/null)" ] && grep -qE '(^| )(pull|up|run|create)( |$)' "$dir/docker.log"; then
        why="$why docker was asked to pull or start;"
    fi
    if [ -z "$why" ]; then res "$name" pass; else res "$name" "FAIL ($why)"; fi
}

f=$work/f
fakes "$f/i686" i686 32 real
fakes "$f/armv7l" armv7l 32 real
fakes "$f/aarch64-32" aarch64 32 real
fakes "$f/glibc212" x86_64 64 "glibc 2.12"
fakes "$f/musl" x86_64 64 "musl 1.2.4"

stops i686 "$f/i686" 'processor (i686)'
stops i686-dry-run "$f/i686" 'processor (i686)' --dry-run
stops armv7l "$f/armv7l" 'processor (armv7l)'
stops aarch64-32bit "$f/aarch64-32" '32-bit programs'
stops aarch64-32bit-ctl "$f/aarch64-32" '32-bit programs' --method ctl
stops glibc-2.12-ctl "$f/glibc212" 'C library (glibc 2.12)' --method ctl
stops glibc-2.12-systemd "$f/glibc212" 'C library (glibc 2.12)' --method systemd
stops musl-ctl "$f/musl" 'C library (musl)' --method ctl
if ! command -v docker >/dev/null 2>&1 && ! command -v podman >/dev/null 2>&1; then
    # No engine here: the automatic route is the binary one, so the C library counts.
    stops glibc-2.12 "$f/glibc212" 'C library (glibc 2.12)'
    stops musl "$f/musl" 'C library (musl)'
fi

# The container route on a processor with no image: a docker (and Compose) that answers, and
# logs what it is asked. It must not be asked to pull or start anything.
for m in i686 armv7l; do
    fakes "$f/docker-$m" "$m" 32 real
    cat >"$f/docker-$m/docker" <<EOF
#!/bin/sh
echo "\$*" >>"$f/docker-$m/docker.log"
exit 0
EOF
    chmod 0755 "$f/docker-$m/docker"
    stops "container-$m" "$f/docker-$m" "processor ($m)"
    stops "container-$m-dry-run" "$f/docker-$m" "processor ($m)" --dry-run --method compose
done

# --method compose where no engine answers (a docker and podman that fail).
mkdir -p "$f/noengine"
printf '#!/bin/sh\nexit 1\n' >"$f/noengine/docker"
cp "$f/noengine/docker" "$f/noengine/podman"
chmod 0755 "$f/noengine"/*
if [ "$(id -u)" = 0 ]; then
    stops compose-no-engine "$f/noengine" 'container engine' --method compose
else
    stops compose-no-engine "$f/noengine" 'container engine' --method compose --dry-run
fi

# No /dev/fuse and no way to load it (a container without the device), on a supported machine.
if [ ! -e /dev/fuse ] && [ "$(id -u)" = 0 ]; then
    stops no-fuse "$f/noengine" 'FUSE'
fi

# --report: status 0, the block, nothing written (an empty folder as the working and temporary
# folder), no request, and no host name or --iso-dir value in it; also without root or --iso-dir.
echo "=== --report"
host=$(cat /proc/sys/kernel/hostname 2>/dev/null || hostname)
mkdir -p "$work/empty"
state >"$work/state0"
before=$(requests)
(cd "$work/empty" && TMPDIR=$work/empty SACDFS_RELEASES_URL=$url sh -s -- --report --iso-dir "$iso" <"$script") >"$work/out" 2>&1
rc=$?
cat "$work/out"
state >"$work/state1"
after=$(requests)
why=
[ "$rc" = 0 ] || why="$why status $rc;"
for k in '^--- sacdfs system report' '^stopped by: *nothing' '^processor: ' '^cpu: ' '^memory: ' '^os: ' '^kernel: ' \
    '^c library: ' '^fuse: ' '^systemd: ' '^containers: ' 'unsupported-system.yml'; do
    grep -q "$k" "$work/out" || why="$why no '$k';"
done
[ -n "$host" ] && grep -qiF "$host" "$work/out" && why="$why the host name is in it;"
grep -qF "$iso" "$work/out" && why="$why the --iso-dir path is in it;"
grep -qF 'isodir-must-not-show-up' "$work/out" && why="$why the --iso-dir name is in it;"
[ -z "$(ls -A "$work/empty")" ] || why="$why it wrote files;"
same "$work/state0" "$work/state1" || why="$why something changed;"
[ "$before" = "$after" ] || why="$why $((after - before)) requests;"
if [ -z "$why" ]; then res report pass; else res report "FAIL ($why)"; fi
if sh -s -- --report <"$script" >/dev/null 2>&1; then res report-no-options pass; else res report-no-options FAIL; fi

# A supported machine with a fake getconf saying 64-bit and glibc 2.17: no stop before the
# root check or the download (the dry run fetches the release, so only the report is used here).
fakes "$f/ok" x86_64 64 "glibc 2.17"
PATH="$f/ok:$PATH" sh -s -- --report <"$script" >"$work/out" 2>&1
if grep -q '^c library: *glibc 2.17' "$work/out" && grep -q '^processor: *x86_64, 64-bit' "$work/out"; then
    res report-fakes pass
else
    res report-fakes FAIL
fi
exit "$fails"
