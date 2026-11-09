# Development: leak hunting, memory profiling, skipped tests and real-disc tests

sacdfs runs for days on a NAS, so memory it keeps alive by mistake adds up. A map that only
grows, nodes kept after a re-read, or the buffers of abandoned requests all count. In Rust,
leaks are usually of this kind, not a missing `free`. There are four tools for finding them
(#151). Only the first runs on every pull request; the owner asked to keep CI fast.

| Tool | What it finds | Where it runs | Time |
|---|---|---|---|
| `tests/no_growth.rs` (dhat-rs) | Heap still live after an unmount that grows from one mount cycle to the next | Every PR, in the `tests` job | about 15 s |
| `scripts/soak.py` | A resident or anonymous memory trend, climbing threads or open files, and a growing cache directory, over hours of churn | A test host (2 h or more); weekly as a 20-minute run on the corpus | hours |
| `scripts/leak-check.sh massif` | The allocation sites still live at the end of a soak segment, and how each grew | A test host, in a container | 30 to 60 min |
| `scripts/leak-check.sh memcheck` | Definite and indirect leaks, and memory errors, in every test, including the `sacdfs` processes the FUSE tests start | A test host, in a container; weekly in CI | about 1 h |

The weekly runs are in `.github/workflows/leaks.yml` (Mondays, and on demand from the
Actions tab).

## The no-growth test

`tests/no_growth.rs` mounts the filesystem in its own process, on a `fuser` background
session, so that dhat-rs (its global allocator) counts every allocation. It then runs eight
cycles of:

1. mount;
2. list and read everything, twice;
3. add an image and a sidecar, then remove them (the re-read, #101);
4. edit the tags file (the reload, #121);
5. unmount.

The heap still live after the second cycle and after the last may differ by at most 16 KiB.
Leaking one tree per cycle adds about 40 KiB a cycle, so the test fails on that by a wide
margin. The first cycle is a warm-up: process-wide state is set up once (the rayon pool, the
tags store, the handle cache).

```sh
SACDFS_FUSE_TESTS=1 cargo test --test no_growth -- --nocapture
```

It prints the heap after each cycle. dhat allows one profiler per process, so the file holds
one test.

## What a listing holds

```sh
cargo run --release --example listing_mem -- SOURCE
RUSTFLAGS='--cfg heap_dhat' cargo run --release --example listing_mem -- SOURCE
```

`examples/listing_mem.rs` walks SOURCE, lists every directory and makes every file's node, with no
mount and no bound (#237). The first form prints the process's anonymous memory and glibc's accounting
at each step. The second writes `dhat-heap.json` while the tree is still alive, so the "end" figures
of each allocation site are what the listing holds. The breakdown in
[design](design.md#memory-a-budget-for-listings-237) came from these.

## The soak

```sh
cargo build --release
python3 scripts/soak.py --sacdfs target/release/sacdfs --images DIR --hours 2 --out soak.tsv
```

`DIR` holds `.iso` images, at least two; real discs give realistic reads. The script builds
a library of links to them in a work directory (under `/var/tmp` by default; `--work DIR`
to choose; on the GitHub Ubuntu runner a mount under `/var/tmp` was refused with "Permission
denied" and one under the runner's home worked, so the weekly run uses `--work`) and mounts it with
`--tags`. If the mount fails, the soak prints sacdfs's stderr and log. Then:

- **Readers** (`--readers 4`) read the mounted files, in three patterns: long stretches from
  random offsets, bursts of small reads at random seeks, and a library scan's
  start/middle/end reads.
- **The churner** acts every 5 s. It adds and removes album folders with an image in them,
  adds and removes cover images, adds and removes an `Artwork/` folder, and edits tags files.
- **The staller** acts every 3 min. It points an image's link at a FIFO and reads until the
  watchdog fails the request (#141), then releases the stuck open and restores the link.
- **The remounter** acts every 5 min. It mounts the library a second time with the same
  caches, reads, and unmounts.

Each minute it writes one TSV row with:

- the main sacdfs's resident memory (`rss_kb`, `hwm_kb`);
- its anonymous memory (`anon_kb`: the malloc heap plus thread stacks);
- its threads and open files;
- the size of the cache directory (seek index and tags state);
- running counts of the work done.

The verdict looks only at what follows the 10-minute warm-up. **PASS** needs all of these:

- resident and anonymous memory stay within the cache cap (`--cache-mb`, default 64) plus
  32 MiB of noise above their level at the end of the warm-up;
- their trend over the second half is at most 4 MiB/h, or adds less than 3 MiB over the part of
  it that is judged (`--trend-floor-mb`), so that a short run's noise does not fail it. The
  trend is taken with the largest single step set aside: the second half is cut into six equal
  runs of samples, each reduced to its median, and the largest change between neighbouring
  medians (up or down) is dropped with the time it spans. What is left, as a rate, is the
  trend. *Why:* the memory of a healthy run moves in steps of 3 to 6.5 MiB, both ways and
  unexplained ([#247](https://github.com/aclater/sacdfs/issues/247)), and a line through the
  second half of a 2-hour run counted such a step as a climb of 8 to 9 MiB/h. A step shows in
  one change between medians, a leak in all of them; a step down is dropped too, or it would
  hide a leak beside it. *Limit:* in a 2-hour run (about 0.6 h is left after the step) this
  fails a steady climb of about 5 MiB/h or more; a slower one needs a longer run (an 8-hour run
  fails at 4 MiB/h). Two steps in the same direction count as a climb, one of them is not
  set aside;
- threads do not climb: the median and the end level (the median of the last sixth of the
  second half, at least five samples) are at most 4 above the first half's median. The staller
  makes sacdfs spawn spare threads on purpose (#167, #168), and a spare thread ends 10 s after
  its work, so a sample taken during a stall may be well above the median: a sample over
  `--thread-spike` (default 16) above the first half's median is allowed, up to twice that, as
  long as the samples above the first line don't span `--spike-hold` (default 120 s, so three
  in a row at the default interval). One sample past twice `--thread-spike` fails, and threads
  that accumulate fail. *Why:* the peak of healthy 2-hour runs reached the single line of
  first-half median + 16 on one rig (80 of 80 on 0.4.1-rc.3; 79 of 80 on 0.4.0), so one more
  thread would have failed a run that did nothing different;
- open files do not climb: the second half's maximum is at most 8 above the first half's;
- the cache directory plateaus;
- no read error is unexpected. Reads fail on purpose in some places, and the soak sorts every
  failure by path and errno: `churn` (a path in a churn album the churner has removed: ENOENT
  or ENOTDIR, or EIO within 30 s of the removal), `stall` (EIO, ETIMEDOUT, ENOENT or EINTR
  under the `stall` folder while a stall is on, and for three request timeouts after), `tags`
  (ENOENT on a `.dsf` after a tags edit since the file list was taken, or within 60 s of it),
  `sidecar` (ENOENT on a `cover-*.jpg` the churner has removed, or under `Artwork/` within
  60 s of the churner toggling it; sacdfs lists a removed file until it has re-read the
  folder, which takes seconds on real discs). A `cover.jpg` that fails to read is not expected
  (#235: it used to be counted apart, as `cover`, for the second after a cover was replaced).
  Any other error, such as EIO on a stable disc, fails the run, with no tolerance. The counts
  per class are columns of the TSV, and `<out>.errors.tsv` has class, errno and area. The summary lists up to ten unexpected paths only with `--sample-paths`: leave it off
  on a real library, whose names must not be published. A failed read returns at once and is
  retried after `--read-pause`, so the counts are attempts; the summary also gives how many
  different files failed.

The exit status is 0 on PASS and 1 on FAIL. `--help` lists the knobs.

On a hybrid CPU, pin the soak to the efficiency cores so that it leaves the host's
benchmarks alone, for example `taskset -c 12-19 python3 scripts/soak.py ...` on a
13th-generation Core i9.

## Heap profile of a soak segment (massif)

```sh
podman build -t sacdfs-leak -f scripts/leak-check.Containerfile scripts
podman run --rm --device /dev/fuse --cap-add SYS_ADMIN -v "$PWD":/src:z -v DIR:/images:ro,z \
    -w /src sacdfs-leak scripts/leak-check.sh massif /images 40
```

This runs the soak for 40 minutes, with the mounted sacdfs under valgrind massif. It then
prints:

- the heap over the run;
- the allocation sites still live at the last detailed snapshot;
- each site's growth from the first detailed snapshot after the warm-up to the last.

`scripts/massif-summary.py` makes that report. A site is the first sacdfs or scarletbook
function on the stack. Where `ms_print` exists (it is not in UBI), its full output goes to
`target/leak-check/massif.txt`.

The container is UBI 10 with RHEL 10's Rust and valgrind. heaptrack would be faster, but it
is not in UBI, and massif gives the same two answers.

## The test suite under memcheck

```sh
podman run --rm --device /dev/fuse --cap-add SYS_ADMIN -v "$PWD":/src:z -w /src \
    sacdfs-leak scripts/leak-check.sh memcheck
```

This runs every test binary under valgrind memcheck, one test at a time. It follows the
`sacdfs` processes that the mount tests start; `fusermount3` is setuid and is skipped, and so
are the system tools the tests run (`/usr/bin/*`: `mkfifo`, `ls`, `df`, `awk`). On the
GitHub runner those are uutils coreutils, which leaks at exit (`mkfifo` alone shows 43 KiB
definitely plus indirectly lost), and they are not sacdfs. Any definite or indirect leak, or
any memory error, in sacdfs or a test fails the run. The logs go to
`target/leak-check/`, one per process, and suppressions to `scripts/leak-check.supp`, each
with its reason. `ONLY=REGEX` runs only the test binaries whose path matches, for example
`ONLY='/(mount|stall)-' scripts/leak-check.sh memcheck`.

The suppressions match demangled names (`fun:*fuser*mnt*Mount*new`), not `_ZN...` symbols, so
they hold under both symbol manglings: the container's Rust 1.92 uses the legacy scheme, the
1.99 toolchain of the weekly CI run uses v0. A suppression written against the mangled name
stops matching after a toolchain change and fails the run on a block that was never a leak.
There are two: the mount that `serve` keeps on purpose (55 bytes in three blocks, once per
process), and a read of uninitialised memory inside the `nix` crate's `recvmsg` when fuser
reads fusermount's reply (once per mount, the value is discarded).

*Why memcheck rather than LeakSanitizer:* sacdfs and scarletbook are pure Rust, with no C
code in the dependency graph. Rust's LeakSanitizer needs a nightly compiler
(`-Zsanitizer=leak`). clang's runtime trick for C (`-shared-libasan` with `LD_PRELOAD`) does
not instrument Rust code. memcheck needs neither and also follows the child `sacdfs`
processes.

## Skipped tests

A Rust test can't skip itself at run time. A guard that returns early reports as `ok`, and no
count shows it. In pysacd, that hid a comparison job that had stopped comparing for six releases
(aclater/pysacd#131). So a guard here goes through `common::skip!` (`tests/common/mod.rs`), and
never a bare `return`:

```rust
common::skip_without_fuse!(); // fails instead under SACDFS_FUSE_TESTS=1
if !handles_available(&m.at, &files[0]) {
    common::skip!(NO_HANDLES);
}
```

It prints `SKIPPED binary::test: reason` directly to stderr, past libtest's capture, so every
`cargo test` shows it without `--nocapture`. `#[ignore]` tests print their reason themselves.

The `tests`, `tests (arm64)`, `coverage`, `nfs export` and `file handles (arm64)` jobs save cargo's output and then run
`scripts/check_skips.py`, as a step of the same job. It writes the job summary: how many tests ran
(libtest's `passed` less the guards' tests), how many were skipped by a guard, how many were
ignored, and each one with its reason. The step fails on a skip that isn't listed for that job in
`tests/expected-skips.json`, on a listed skip that has stopped, on a changed reason, and on output
with no test results. To add a skip, add it to the list and say in `why` what it's legitimate for,
and where the test does run.

To run the same check locally:

```sh
SACDFS_FUSE_TESTS=1 cargo test --locked 2>&1 | tee target/test-output.txt
python3 -I scripts/check_skips.py --job tests target/test-output.txt
```

As a normal user with FUSE, this gives the `tests` job's result. On a machine without FUSE, leave
out `SACDFS_FUSE_TESTS=1` to see which tests skip; the check then reports each as unexpected.
`python3 -I scripts/test_check_skips.py` tests the script itself. `python3 -I scripts/test_soak.py`
tests the soak's verdict on built series (a step, a leak, a thread spike).

*Why it reads cargo's output rather than a report file, as pysacd does:* the reasons of `#[ignore]`
tests are only there, and the output needs nothing passed through `sudo` (the `nfs export` job) or
into a container. The release workflow's binary tests don't run this check yet.

## Real-disc tests

Three tests run on real discs, which no CI job has: images can't go into a public repository
(copyright), and the corpus is synthetic. They are `#[ignore]`d, so a plain `cargo test` lists them
as ignored. They check that

- `dsf_header::real_discs_head_reads`: a tagger's and a player's reads of every DSF file start
  read-ahead only where they should (#78);
- `library_index::real_discs`: 300 random ranges per area, located by sacdfs, equal reads through
  a complete library index and a straight read;
- `wav::real_discs`: every WAV file's audio equals its track's positions of the area stream, and
  random ranges equal the straight read.

Run them with:

```sh
scripts/real-discs.sh DIR
```

`DIR` holds `.iso` images of SACDs only, at least one, searched recursively (links to files are
followed). `SACDFS_PCM_RATE=88200` or `352800` picks the WAV rate (default 176400); `CARGO_FLAGS`
adds cargo options. The script runs `cargo test --release` with `SACDFS_REAL_TESTS=1`, and exits
0 only when all three tests ran and passed. Without a directory it prints `SKIPPED` and exits 77,
which is a skip and not a pass. The same holds under plain cargo: with `SACDFS_REAL_IMAGES` unset,
the tests print `SKIPPED` (counted by `check_skips.py`), and with `SACDFS_REAL_TESTS=1` they fail.

A run that looked at nothing fails: a directory that doesn't exist or holds no image, and an image
that doesn't open, has no area, or serves no file, each fail the run, because the tree and
`check_image` drop such images silently. So don't put non-SACD images in `DIR`.

The tests check that sacdfs agrees with itself and with the library on the image as it is. A truncated
image is not caught: the sector count comes from the file's size, so the audio area is cut
short and every comparison still agrees. A checksum against the source checks image integrity.

The WAV test dominates the time: 6 images (about 8.6 GB) took 12 min and 2 images (1.5 GB) 3 min on a
busy 32-core host (load over 50, so not a speed figure). On main at b46c5e0, 18 images (41 GB: stereo and multichannel, DSD and DST, hybrid and
single-layer, one early TOC version) passed 3 of 3 in 18 min on an otherwise idle 32-core host at
`nice` (DSF head reads 13 s, library index 2 min, WAV 16 min; 20 areas, 117,198 random-range
positions compared).

The release checklist runs this before every release candidate ([test plan](test-plan.md#real-disc-tests)).

## Fuzzing

Not a leak tool, but it runs the same way: `fuzz/` holds cargo-fuzz targets for sacdfs's own
parsers and paths (#161), for about 60 s each on every pull request and 20 minutes each every
week. See [fuzz/README.md](../fuzz/README.md).

## Results

What these tools found, and what was fixed, is in docs/design.md under "Leaks", and in the
pull request for #151.
