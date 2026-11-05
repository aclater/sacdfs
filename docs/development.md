# Development: leak hunting and memory profiling

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

## The soak

```sh
cargo build --release
python3 scripts/soak.py --sacdfs target/release/sacdfs --images DIR --hours 2 --out soak.tsv
```

`DIR` holds `.iso` images, at least two; real discs give realistic reads. The script builds
a library of links to them in a work directory (under `/var/tmp` by default) and mounts it
with `--tags`. Then:

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
- their least-squares trend over the second half is at most 4 MiB/h, or adds less than 4 MiB
  over that half (`--trend-floor-mb`), so that a short run's noise does not fail it;
- threads do not climb: the median and the end level (the last three samples) of the second
  half are at most 4 above the first half's median, and no sample passes `--thread-spike`
  (default 16) above it. The staller makes sacdfs spawn spare threads on purpose (#167, #168),
  so one sample taken during a stall may be several threads high; that does not fail a run,
  while threads that accumulate still do;
- open files do not climb: the second half's maximum is at most 8 above the first half's;
- the cache directory plateaus.

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
`sacdfs` processes that the mount tests start; `fusermount3` is setuid and is skipped. Any
definite or indirect leak, or any memory error, fails the run. The logs go to
`target/leak-check/`, one per process, and suppressions to `scripts/leak-check.supp`, each
with its reason.

*Why memcheck rather than LeakSanitizer:* sacdfs and scarletbook are pure Rust, with no C
code in the dependency graph. Rust's LeakSanitizer needs a nightly compiler
(`-Zsanitizer=leak`). clang's runtime trick for C (`-shared-libasan` with `LD_PRELOAD`) does
not instrument Rust code. memcheck needs neither and also follows the child `sacdfs`
processes.

## Fuzzing

Not a leak tool, but it runs the same way: `fuzz/` holds cargo-fuzz targets for sacdfs's own
parsers and paths (#161), for about 60 s each on every pull request and 20 minutes each every
week. See [fuzz/README.md](../fuzz/README.md).

## Results

What these tools found, and what was fixed, is in docs/design.md under "Leaks", and in the
pull request for #151.
