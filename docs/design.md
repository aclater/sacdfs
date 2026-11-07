# sacdfs design

sacdfs is a read-only FUSE filesystem that shows the audio of Super Audio CD disc images
as ordinary DSF files, and as WAV files (24-bit PCM) next to them, decoded and converted on
demand. Point a player, a tagger or a file manager at the mount point and every track of
every disc is there, with tags, without extracting anything first.

This page describes the design as it is, and why each part is the way it is. The project's
decisions across its repositories, one line each with where they are recorded, are in the
[decision log](https://github.com/aclater/sacd/blob/main/DECISIONS.md). Measurements are in
[performance.md](performance.md), the converter's quality in [pcm-quality.md](pcm-quality.md).

## Goals

1. **Correct audio.** A track read through the mount is bit-identical to the same track
   extracted with a reference extractor (pysacd's `pysacd-extract`, or scarletbook's
   `bench decode --sink raw`), checked by per-channel SHA-256 of the DSD stream. A WAV
   file's audio is exactly what the converter makes of the track's frames, each with its
   neighbours in the area, whichever reads asked for it.
2. **Fast, and low latency** (the targets of
   [#6](https://github.com/aclater/sacdfs/issues/6)). A straight read runs within 1.5x of
   `bench decode` at the same thread count, and a WAV file within 1.5x of its DSF file. A
   cold seek returns its first bytes in under 20 ms from a local disk, under 25 ms with
   sacdfs on the NAS, under 50 ms (NFS) or 80 ms (SMB) with the images on a share. Four
   players and a tagger see no read over 50 ms.
3. **Cheap to mount.** Mounting a library of hundreds of discs reads no image; the master
   and area TOCs are read lazily, when a directory is first listed. Nothing is decoded
   until a file's audio is read.
4. **Flat memory.** A bounded cache of decoded and converted frames, whatever the library
   size or the number of open files.
5. **Safe on hostile images.** Every image is untrusted input. A bad image shows up as a
   missing directory or an I/O error on one file, never as a crash of the mount.

Not goals: writing, DSDIFF (DFF) output, FLAC output (deferred, #10), DoP, the 48 kHz PCM
family, PS3 network sources, macOS and Windows natively (#22; they read the files over SMB
or NFS from the [container](deployment.md)). The plan for PCM output, and every decision
behind it, is in #11.

## Engine

All SACD parsing and DST decoding comes from
[scarletbook](https://github.com/aclater/scarletbook-rs), the clean-room Rust library
(MIT OR Apache-2.0), used through its public API as a pinned git dependency until it is
published on crates.io. sacdfs adds no SACD format knowledge of its own beyond what that
API exposes: sector reading (`Image`, positional reads through `ReadAtReader`), seeking and frame
reassembly (`Disc`, `TrackCursor`, `AreaIndex`), TOCs and text (`Toc`), loudspeaker layouts (`Area`) and decoding
(`DstDecoder`, `audio::decode_frame`).

- **The pin is scarletbook 0.5.9, from crates.io (`=0.5.9`)** (0.1.0 shipped with v0.5.0, the latest release when its release
  preparation started, #45). *Why:* a release ships against the newest scarletbook, taken
  while it passes the correctness gates, and then frozen so that the performance round
  measures the code that ships; each move of the pin is a change of its own, gated like a
  release (DSF 93/93, the real-disc and corpus comparisons with scarletbook's exact seek
  index). 0.5.2 was taken for 0.2.0 for its seeking (#28); 0.5.3 for its DSD-to-PCM converter,
  `scarletbook-pcm` from the same release, which sacdfs now uses (#84,
  [PCM conversion](#pcm-conversion)). 0.5.6 (#128) for the plain-DSD speed it restored
  (scarletbook-rs#216) and its run-time DST dispatch, with unchanged bytes; *why not stay:* it
  removes the known plain-DSD WAV slowdown for free, and every gate passed; *trade-off:* none
  measured (no read got slower). 0.5.7 (#155) came from crates.io, pinned exactly: *why:* the
  registry is the only source a published crate may use and cargo's checksum pins the bytes; the
  tag is no longer needed; bytes unchanged, gated as above. 0.5.8 (#206): a seek-index
  fix (`AreaIndex::from_bytes` refuses a form of saved index the library never writes) and no
  speed change; *why:* the pin follows the newest release that passes the gates; bytes unchanged,
  gated as above. 0.5.9 (after 0.4.1): faster DST decoding and a rewrite of the DSF partial-row code; *why:* same rule; bytes unchanged, gated as above; *trade-off:* a DSF read on damaged media can now fail because of a bad neighbouring position (library notes).
- **Seeking is scarletbook's** (since 0.2.0, #28): its seek index per area, provisional (cold)
  readings and seek hints, through range cursors; sacdfs's own frame locator was removed.
  *Why:* it measured within the owner's 15 % leeway and no less exact
  ([Seeking](#seeking-scarletbooks-28)). Not used: cancellation (`CancelToken`) and progress
  (`Progress`); cancellation could later back FUSE `INTERRUPT` (#45). The tests use
  scarletbook's exact seek index as a reference.
- **Built without scarletbook's `parallel` feature;** sacdfs decodes on its own worker pool.
  *Why:* memory stays bounded by `--cache-mb` however many files are open, and a parallel
  library cursor would hold a window of its own (512 positions, about 48 MB).

### Memory (#73)

Measured with `scripts/import-replay.py`, a replay of a library import's reads (Roon: 768 KiB
at each DSF file's start, 36 KiB at its middle, 33 KiB at its end) over 60 images (five discs,
twelve times; 1 116 files) on a 32-thread host; peak resident memory (`VmHWM`) at the end:

| | `--cache-mb 0` | `--cache-mb 16` | `--cache-mb 256` | straight reads, 256 |
|---|---|---|---|---|
| before | 83 MiB | 125 MiB | 473 MiB | 965 to 1 066 MiB |
| after | 32 MiB | 58 MiB | 271 MiB | 327 MiB |

("Straight reads": three areas' DSF files and one area's WAV files read through, on one mount.)
So `--cache-mb C` now costs about C + 15 to 40 MiB under this pattern, against C + 220 MiB
before. Two changes, each needed:

- **One malloc arena** (`engine::limit_malloc_arenas`: glibc's `M_ARENA_MAX` set to 1 at
  start, unless `MALLOC_ARENA_MAX` or `GLIBC_TUNABLES` sets it). *Why:* glibc gives every
  thread that allocates an arena of its own, up to eight per CPU, and memory freed in an arena
  stays there for that arena. Decoder, FUSE and read-ahead threads all allocate and free
  frames and read buffers, so each arena held its own share of the working set: 50 MiB more
  with no cache at all, and up to four times the cap after straight reads. *Trade-off:* one
  lock for all allocation; DST straight reads took 4 % longer (DSF 11.11 → 11.55 s, WAV
  13.52 → 13.98 s for a 6-channel area; plain DSD 1.69 → 1.59 s), inside the 5 % allowed.
  *Alternatives measured:* eight arenas, 2 % slower and 287 MiB at 256 (C + 31); two and four
  arenas held more than one without the cache's change below. *Not tried:* jemalloc or
  mimalloc as the global allocator (a C dependency and a new cost for every allocation, for
  what one setting does); `malloc_trim` after evictions (gives back only whole free pages,
  and walks the heap). This is the one `unsafe` call outside the daemonising code: `mallopt`
  takes two integers. *The limit comes before the start-up walk,* which reads the source's
  top-level folders on 16 threads (#175): called after it (as in 0.4.1-rc.1), the walk's threads
  had each made an arena, 31 of them on a library of 414 images, and the mount held 95 to
  98 MiB after a scan instead of 73 to 77 MiB (`tests/arenas.rs` checks that none is left).
  *Alternatives measured for that walk:* `malloc_trim` after it freed nothing (95 MiB); two
  arenas, 79 to 85 MiB.
- **The cache's frames live in slabs outside the heap** (`engine::Pool`, `FrameBuf`; this
  replaced the reuse of evicted frames' buffers, `FrameCache::spares`, of the first fix).
  Each frame takes a slot of a slab of slots of its length (512 KiB, mapped with `mmap`), DST
  frames are decoded straight into it, and its slot is given back when the frame is dropped.
  The slabs together never pass the cap; a slab is unmapped only when it is empty and a
  frame of another length needs the room. When the slabs are full, a new frame evicts the
  least recently used frames until a slot is free. *Why:* frames on the heap left holes when
  they were evicted, because frames of 2, 5 and 6 channels and PCM frames differ in size and
  readers drop frames late; the allocator could neither give these back nor fill them with
  frames of another size. Reusing evicted buffers of the same size (the first fix) helped,
  but a full-library replay (271 images, on 4 CPUs) still ended with 7 MiB free in the heap
  at `--cache-mb 256` and 12 MiB at 16. With slabs, the cache's memory is its cap: `--cache-mb
  16` went from 73 to 62 MiB (66 to 53 MiB on 4 CPUs), and `--cache-mb 256` from 304 to
  300 MiB (292 MiB either way on 4 CPUs), about what `--cache-mb 0` holds plus the cap. Cached frames still leave an eighth of the cap (at most 32 MiB) free, now as free
  slots (`free_reserve`). *Why:* with frames filling the whole cap, each new frame evicted one
  first and straight reads of a plain DSD stereo area took 5 to 10 % longer; with the reserve
  they take free slots, as the spare buffers did. *Why 512 KiB:* a slab empties soon after
  its frames stop being used, and a 256 MiB cache is 512 mappings, far below the kernel's default limit of 65 530. *Measured
  alternatives:* `malloc_trim` after the replay gave back only 5 MiB of the 7 MiB of holes at
  256; slabs mapped with `MAP_POPULATE`, or 2 MiB slabs with `MADV_HUGEPAGE`, read no faster
  (stereo plain DSD, medians of 6: 1.92 and 1.98 s against 1.95 s). *Cache colouring:* slots start at multiples of the frame length, so the frames a
  thread decodes one after another do not all start at the same offset within a page, which
  would make them collide in the CPU's cache sets ([Drepper, "What every programmer should
  know about memory", 2007](https://www.akkadia.org/drepper/cpumemory.pdf),
  on cache associativity; [mmap(2)](https://man7.org/linux/man-pages/man2/mmap.2.html):
  mappings are page-aligned). *Trade-off:* `unsafe` code for the mapping and its slots
  (`mmap`, `munmap` and raw slices into the slab), kept to one type.
- **What is left above the cap** (#73 item 3), measured with a heap walk (glibc's chunks and
  `/proc/PID/pagemap`) after the 271-image replay: about 45 to 50 MiB on a 32-thread host,
  none of it the cache's:
  - about 7 MiB for the program at mount;
  - about 16 MiB for the images and their files once listed (the tree and each image's TOC, its text and track lists; one copy since #136, was
    two: scarletbook's and one `Disc` kept), about 60 KB per image before, about half now. Raw-sector images (2 054 or 2 064 bytes per sector) also keep an 80 KiB
    read buffer in scarletbook (the replay's library had 54 of them: 9 MiB);
  - about 18 MiB more during the replay: each decoder thread's DST tables and buffers (roughly
    300 to 350 KB per thread that decoded DST; 17 threads here), the seek indexes' pending readings
    (80 bytes per seek, at most 256 per area) and seek hints, and smaller objects;
  - the FUSE threads' 16 MiB receive buffers are allocated but almost never touched (under
    1 MiB resident in all).

  It grows with the number of images listed (see above) and with decoder threads,
  not with the cache, and does not depend on `--max-open-images`.
- **One TOC per image** (#136). `Disc::toc()` is a method over scarletbook's copy instead of a
  clone held in a field. *Why:* the owner's target is peak RSS within the cache cap plus 50 MB
  (#73), and the second copy was about 60 KB per image (16 MB over 271 images). *Alternative:*
  an `Arc<Toc>` shared by `Disc` and the library: not possible without changing scarletbook,
  and the borrow from the library's disc costs nothing per read. *Trade-off:* the TOC is
  borrowed from the library's disc, so it lives as long as the `Disc`, as before.

### Memory: a budget for listings (#237)

A large library under a container memory limit got sacdfs killed while listing, which ended the
service for every client. An allocation failure can't be relied on, because the kernel's
out-of-memory killer acts first, so sacdfs has to stay under the limit by itself.

**What the memory was.** About 57 KiB per image, measured with dhat-rs and glibc's own accounting at 395, 1,600
and 5,000 images (`examples/listing_mem.rs`), at 24.8 files per image: 47% is built per file (header,
tag bytes, the structs), 18% is the TOC and text, 19% the tree's nodes, entries and names, 2% the
walk, and 13% the allocator's overhead. Over 90% of the bytes requested can be rebuilt from the image.

**The decision.** The owner's direction was for running short of memory to make sacdfs slower rather
than kill it, with re-reading the TOC as the accepted cost. So the unit dropped is the whole image:
its disc, areas and files. *Why it stays correct:* numbers come from paths (see [`ident`](../src/ident.rs)),
and a number not in the tree is already looked for along its path, for NFS handles kept across a
restart. A dropped image goes through that same path, and its names, numbers and generations come
back the same while the image is unchanged. If the image did change, it is what a remount would show.

**Alternatives.**
- *Drop only the TOC and text:* saves 18%, where 20,000 images at 256 MiB needs about 90%.
- *A memory-mapped listing file per library,* the first proposal on #237: names, sizes and times are a
  small part of the memory, so it moves little by itself, and it needs a key covering every image's
  size and mtime, the tags and the output options, and a format. It also needs this path as its
  fallback when the cache directory isn't writable. It could still help a warm start (#232); that's a
  speed question, unmeasured, and the bound doesn't depend on it.
- *Strict least-recently-used:* needs a lock on requests answered from memory. The clock order here
  only sets a flag, and a scan that goes through everything once evicts in nearly the same order.

**Trade-offs accepted.** Going back to a dropped image costs a TOC read. A file open keeps its whole
image, so readers can hold more than the budget. Below the budget, nothing is dropped and nothing is
added but a flag. The count of nodes in `statfs` falls as images are dropped.

**Details that matter.**
- A source directory opens its images in batches of 64 under a bound. One directory can hold more
  images than the bound allows at once.
- The persistent seek index holds each disc it has registered until the next flush, 30 s later. A scan
  goes through thousands of images in that time, so it now lets go of dropped discs with nothing to
  save as soon as a batch has been dropped.
- A request that holds a directory's node while its image is dropped sees a mark on that node and asks
  the tree again.
- A closed image that can't be opened again fails the request and is not remembered, so it isn't
  shown as empty to a scanner because of a passing error.

**The memory watch.** Item 3 of the issue. What's counted is the cgroup's anonymous and shared memory
rather than `memory.current`, which includes the page cache and sits at the limit on any machine that
has been reading. The order is what costs least first: listings cost a disc read for whoever comes
back; the cache and decoders cost a reader some decoding; refusing fails one request; a kill fails
every client. The alternative of pressure stall information was left out: it reports that reclaim is
already hurting, and doesn't say how close the kill is.

**Why 3/8 of the limit.** A quarter dropped images from a library that already fitted: at 1,600 images
under 256 MiB, a second pass read TOCs again. At 3/8, about 1,800 images stay in memory at 256 MiB, close to
the 2,000 that earlier versions documented as working there. The rest of the limit is for the process, Samba and NFS.

**A race found on CI.** A number looked for along its path could be made and dropped again before it was
taken, which a client would see as "no such file". A lookup now asks again, up to 16 times, if the image
is still there: 3 failures in 400 runs of the racing test without that, none in 400 with it.

**The defaults from the limit** (#231). Cache 8 up to 512 MiB. The first choice was 16, which showed no
cost to a single reader, but the fresh-container measurements of #230 changed it: with whole-file
scanners running, a 6-channel WAV player held real time at 8 and didn't at 16. On this branch, on E-cores
with a fresh container per condition, 3 rounds each, player speed relative to real time, cache 16 then 8:

| | 2 cores, 256 MiB | 4 cores, 512 MiB |
|---|---|---|
| 1 whole-file scanner | 0.51, 1.00 | 0.96, 1.00 |
| 4 whole-file scanners | 0.55, 0.98 | 0.77, 1.00 |
| Whole-file WAV scan alone, MB/s | 34, 14.5 | 63, 24 |
| Headroom of a reader alone, times real time | 9.9, 4.3 | 18.8, 7.1 |

The trade-off is that scans take 2.4 to 2.6 times as long and headroom falls by about as much, which still
leaves over 4 times real time. On a small machine, playback that doesn't stop is worth more than a scan
that ends sooner. The alternative of keeping 16 and waiting for the fix to #230 is open: if that fix holds
real time at 16, this default should be measured again. Limits above 512 MiB keep 32; they weren't measured.
Threads at one per 64 MiB, at least 2: each costs about 3.7 MiB. With no limit, these
defaults don't change.

**What real clients and tags files added.** Three costs lay outside the budget, and running under real
NFS clients and `--tags` found them.

- *NFS-Ganesha's cache of entries.* Its default of 100,000 entries reached about 180 MiB at 256 MiB, with
  sacdfs down to 59 MiB and refusing for as long as that cache stayed full, as nothing sacdfs could give
  up would help. The entrypoint now sets `Entries_HWMark` to what an eighth of the limit holds at about 2 KiB
  each, and `Chunks_HWMark` to match. The alternative of having sacdfs refuse earlier would fail requests that
  the memory could serve. The trade-off is that entries past the mark are looked up again from sacdfs.
- *Covers kept by each disc's tags.* Tags kept each disc's picture for as long as the mount lasted, up to
  a few GiB for a large library, with or without a bound. A tag copies its picture, so it's needed only
  while a disc's files are made; only the last 16 read are kept now. This one applies with no limit too. The
  alternative of counting them in the budget would keep them in memory without any use. The trade-off is
  a read of the picture each time a disc is listed, and a cover image edited during a mount can show
  then, without a new generation.
- *Freed covers in the heap.* Blocks of hundreds of KiB, made and freed for each file, raise glibc's mapping
  threshold, and then stay in the heap: over 100 MiB more than was in use. Under a limit with a tags directory,
  the threshold is fixed at 128 KiB. Only then, as other mounts don't make such blocks while listing.
  The budget also runs as each directory is listed now, since one image with covers can hold tens of MiB,
  and the watch trims the heap at the Caches level, at most every 2 s, since a trim holds up every thread
  that allocates.

### Memory: the default cache and the fixed overhead (#150)

The owner asked for lower memory use after 0.3.0: peak RSS under 100 MiB at the default cache and
under 64 MiB at the smallest, on the import replay, with straight reads and seeks within 5 % of
0.3.0. Measured on the five bench images (`scripts/import-replay.py`; a 32-thread host):

- **Where the fixed overhead was** (0.3.0, `--cache-mb 8`, WAV import replay, `/proc/PID/smaps`
  and heaptrack's peak): program and libraries 6 MiB; heap 46 MiB, of which the WAV files'
  headers 23 MiB, PCM converters 14 MiB, DST decoders 4 MiB and the rest (tree, TOCs, seek
  state) about 5 MiB; other anonymous memory under 1 MiB (thread stacks, and the FUSE threads'
  16 MiB receive buffers, almost all untouched). After: heap 8 MiB (converters 2, decoders 2,
  the rest 3 to 4).
- **A WAV file's header no longer stores its padding** (`wav::Header`). *Why:* a WAV file's
  audio starts at 256 KiB ([WAV files](#wav-files)), and every listed WAV file kept that
  header whole, zeros included, for as long as it was shown: about 256 KiB per WAV file, 23 MiB
  for the 93 WAV files of the bench images, growing with the library. The zeros are now
  written when a read asks for them. *Trade-off:* none measured; the served bytes are the same
  (DSF gate 93/93, every file's first and last MiB identical to 0.3.0's).
- **DST decoders and PCM converters are pooled, not one per thread** (`engine::DECODERS`,
  `CONVERTERS`). *Why:* each decoder thread kept, for good, a decoder and a converter per
  channel count it had ever used (14 MiB of converters in all in the replay above), though a
  library import decodes a few frames at a time. Now there are only as many as were ever in
  use at once. *Trade-off:* a lock taken twice per frame or run of frames; no read got slower.
- **An idle engine gives its memory back** (`Engine::start_idle_release`): once its cache
  has gone unused for 60 s (`IDLE_RELEASE`), the cached frames are dropped, empty slabs
  unmapped, pooled decoders and converters dropped and the heap trimmed (`malloc_trim`).
  After a WAV import and a 300 MB read at the default cache, RSS went from 57 to 11 MiB.
  *Why:* a mount spends most of its life idle between scans and playback. *Why 60 s:* longer
  than any gap between a client's reads; a reader that comes back decodes its next frames
  again (a few ms), and the kernel's page cache usually still holds what it read.
- **The default `--cache-mb` is 32** (was 256). *Why:* the sweep (8, 16, 32, 64, 128, 256 MiB;
  [performance](performance.md#cache-size-150)) found 32 the smallest size that costs no more
  than 5 % against 256 anywhere. At 16, four parallel readers of one DST image took 10 to 16 %
  longer; at 8, WAV straight reads took 4 to 7 times as long (the fast-reader read-ahead window,
  about 8 MiB of PCM, no longer fits). *Alternatives:* 16 (the earlier NAS recommendation:
  fine for scans and players, not for parallel fast readers); 64 (no faster than 32 anywhere).
  *Trade-off:* a client that re-reads parts of files the kernel has dropped decodes them again
  sooner.
- *Not changed:* the decoder threads (idle rayon threads cost only their touched stack, a few
  KiB each) and the FUSE receive buffers (virtual, not resident). There is no read-ahead
  buffer of its own to drop: read-ahead decodes into the cache.

## Layout

```
MOUNT/
  <relative dir of the image>/<image file stem>/
    Stereo/
      01 - <title>.dsf
      01 - <title>.wav
      02 - <title>.dsf
      02 - <title>.wav
    Multichannel/
      01 - <title>.dsf
      01 - <title>.wav
```

- SOURCE is one image file or a directory, scanned recursively for `*.iso` files (any
  case). Symbolic links to files are followed, links to directories are not.
- **Lazy tree.** At mount time only the names of the source tree are walked (no file is
  opened), and directories with no `*.iso` anywhere below them are left out. A directory's
  images are opened, and their master and area TOCs read, when that directory is first
  looked up or listed (in parallel, a few small reads per image). An area's files and tags
  are made when the area directory is first looked into. *Why:* mounting a library costs a
  walk of its names, whatever its size, and a seek right after a listing finds the image
  open.
- A file that is not an SACD image is left out, and so is a folder with none below it, sidecars
  included (#175). The mount's walk of names reads 8 bytes of each `*.iso` (the master TOC
  signature, where each sector format puts it) to tell; the image is opened (its TOCs read)
  only when its folder is first listed. *Why at the walk:* the parents of a folder are listed
  before it, so they can only leave it out if the walk knows. *Alternative:* open each image
  when its parent is listed, which would read its TOCs in every parent listing; one small read
  per image is far less. *Parallel walk:* the source's top-level folders are read on 16 scoped threads (joined before
  the fork into the background, which needs one thread), so the probes of a network share
  overlap; the result is sorted by name as before. *Rescans:* the folder's non-SACD images, and those of the subfolders
  left out for holding no SACD, are on its watch list (`SourceDir::rejected`) with their
  modification times, so a file written into place (which changes the file, not its folder)
  brings the folder back at the next refresh. The answer for each image is kept while its size
  and time are the same, so a rescan reads nothing of an image it has looked at: only a `stat`
  of each rejected image. Nothing is stored across mounts: the persistent seek index holds
  only discs that opened. An image that cannot be read at the walk counts as an SACD, so that
  opening it later reports why. An image that has the signature but fails to open is left out
  as before; its folder shows up with its sidecars only.
- An image directory whose name collides with a source subdirectory gets ` (2)`. A track
  that fails scarletbook's `check_track` (empty, too many channels, or past the area's
  capacity) is left out. Names are cut to fit 255 bytes.
- `Stereo` and `Multichannel` appear only for areas the disc has.
- File names come from the track text (title), with `/`, NUL and control characters
  replaced; a track without a title is `NN - Track NN.dsf` ([#185](#discs-without-text-185)).
  Duplicates get ` (2)`, ` (3)`.
- **DSF and WAV files side by side** in the same folder (owner decision, #11): `--format dsf`,
  `wav` or `dsf,wav` (the default) selects which appear. *Why:* one mount and one tree, no
  duplicate folders to browse or configure; players and taggers handle two files with the
  same name and different extensions. Listing, `stat` and tag reads cost nothing for the
  extra files (below), and `--format` is there for library scanners that would otherwise
  convert every WAV file. A track's files are made one after the other, DSF first, through
  the same de-duplicator, so its two names always differ only in the extension, ` (2)`
  suffixes included.
- Inode numbers say what an entry is and are the same on every mount (see
  [File identity and NFS export](#file-identity-and-nfs-export)). Sizes are exact and known
  from the TOC alone.

**Hybrid discs.** sacdfs shows only the SACD audio areas of an image. The CD (Red Book)
layer of a hybrid disc is not in SACD images at all, because SACD rippers read only the
high-density layer; CD rips are ordinary files outside sacdfs. The disc's hybrid flag is not
shown either: no consumer needs it, and it would add a tag no player uses.

### Flattening (`--flatten`)

Libraries often keep an image in a rip folder inside a release folder, so a disc sits
several folders below its artist. With `--flatten` (off by default, so existing mounts keep
their paths), a chain of source directories whose only content is one image is shown as
its outermost directory, which then holds the image's `Stereo` and `Multichannel` folders:

```
Artist/Year - Album/Artist - Album (rip)/Album/disc.iso
  -> Artist/Year - Album/Stereo/01 - Title.dsf      (without it: .../Album/disc/Stereo/...)
```

- "Only content" means after the usual hiding: other files, and folders with no `*.iso`
  below them, don't count. A folder that holds an image and another folder with images, or
  two images, is kept; a chain inside it can still collapse.
- The top-level directories of SOURCE (usually artists) are never collapsed, so an artist
  with one album keeps both folders. A single image file as SOURCE is unaffected.
- Collisions get ` (2)` (the collapsed folder keeps its own name, so it can collide only
  where an image of the same stem sits next to it, exactly as without the option). A
  collapsed folder has its image's inode number, so the image, its areas and its files have
  the same numbers with and without the option; only the folders that collapse away (or
  come back) differ.
- **Decided from the names alone** (`SourceDir::single_image`, a walk down the chain already
  in memory), when the parent directory is listed. No image is opened for it, so mounting
  costs the same with and without the option, and listing a folder of collapsed albums opens
  nothing: the collapsed folder (`DirKind::Flat`) opens its image when it is first looked
  into, exactly when the image's own folder would have been. A file that has no SACD master TOC
  signature is not an image to the walk (#175), so it does not make a chain; one that has the
  signature but fails to open still counts as the chain's image, and its collapsed folder shows
  up with its sidecars only.
- *Why these rules* (#13): always flattening would change paths for existing users;
  collapsing the top level too would drop the album name of a single-album artist; naming
  folders from the TOC's text is a separate option if ever wanted, since disc titles are
  often less descriptive than the owner's folder names. Deciding from the TOCs
  would mean opening every image at mount, which is the cost the lazy tree exists to avoid; the
  walk reads only the signature of each (#175).

### Sidecar files (#79, #75)

The owner's goal for sacdfs (#82, 2026-11-01) is to architect it for reclaiming storage
space: people delete the DSF files they extracted
and lose nothing. The first gap was everything else in the album folders. sacdfs showed only
what it makes from images, so a player reading through it saw no local art (in a real Roon
test on the owner's NAS, Roon fell back to its online art), and a playlist could not live on
the share (Roon imports playlist entries only from inside the same storage location, #75).

So sacdfs passes **sidecar files** of SOURCE through, read-only and byte for byte, at their
relative path (`src/sidecar.rs`, listed by `tree.rs`):

- **An allow-list, set with `--sidecars`** (`SACDFS_SIDECARS` in the container). The default
  is `jpg,jpeg,png,webp,m3u,m3u8,Artwork/,Scans/`: the image formats players read as folder
  art, playlists, and two folder names whose whole content passes (scans are often PDF or
  TIFF, and in subfolders). Matched in any ASCII case; hidden names (`.` first, which
  includes macOS's `._folder.jpg`) never pass. The list is applied from left to right
  (`default`, `ext`, `Name/`, `-item`, `none`), so one option widens (`default,cue`), narrows
  (`default,-webp`), replaces (`jpg,png`) or turns it off (`none`, exactly 0.1.0's tree).
  *Why an allow-list, not "everything that isn't an image":* the owner's library keeps the
  extracted DSF, DFF and FLAC files next to the images (#82 counts them); passed through,
  every track would show twice in a player, once as the extract and once as sacdfs's file,
  and logs, `.cue` sheets pointing at files that aren't there, or `Thumbs.db` would ride
  along. Naming what passes keeps the tree predictable, and the option covers the rest.
  `.iso` cannot be an item.
- **Placement.** Next to the image's folder, at the same relative path:
  `SOURCE/A/Album/folder.jpg` is `MOUNT/A/Album/folder.jpg`, beside `MOUNT/A/Album/disc/`.
  Only directories that hold images (directly or below) list sidecars, plus the sidecar
  folders inside them: the tree still leaves out folders with no image. An empty sidecar
  folder is not shown. Sidecars are listed after the directory's subdirectories and images,
  through the same de-duplicator, so **no existing name or number changes**: a sidecar that
  collides (an `Artwork/` next to `Artwork.iso`) takes the ` (2)`.
- **`--flatten`:** the sidecars of every directory of a collapsed chain show in the collapsed
  folder, after `Stereo` and `Multichannel`, outermost directory first (`chain_sidecars`), a
  repeated name with ` (2)`. *Why:* the collapsed folder is where the album now is; its
  art belongs next to its areas, and nothing in the chain is lost. A collapsed image that
  turns out not to be an SACD image still shows its sidecars, as a source folder would.
  Sidecars don't count as "content" for collapsing (as other files didn't before), so a
  chain collapses exactly as it did.
- **Identity (#18).** A sidecar file's inode number is a key in its own domain (3) over its
  relative path, entry bits 0; a sidecar folder is a directory (domain 1) over its path, so it
  keeps its number if it later holds an image. Both are in `Ids`, the index of names made at
  mount, with their parent directory, so a number from an NFS handle is found again after a
  restart: the walk goes down the chain of directories, and with `--flatten` a collapsed
  folder (numbered as its image) stands for the chain directories it covers
  (`Tree::collapsed_over`). A sidecar's number is the same with and without `--flatten`.
  The new domain sorts after directories and images, so on a key collision a directory or
  image keeps its key: adding sidecars never moves an existing number (except through the
  fallback-key case already described below, which needs about 16 million paths).
  **Generation:** 32 bits of a hash of the source file's size and mtime, never 0, so a handle
  to a replaced cover is stale (`ESTALE`) instead of serving other bytes; directories keep 0.
- **Attributes pass straight through.** Size, blocks, and the access, modification and change
  times are the source file's, read with one `fstatat` when its directory is first listed
  (in parallel, as images are opened). Mode (0444), owner and inode number are sacdfs's, as
  for every entry. *Why no derived mtime, unlike #64:* a sidecar's bytes are its source's and
  depend on no option or output version, so its own mtime already moves exactly when its
  bytes do; the generation follows the same two values. *Why read them at listing, not at
  mount:* mounting stays a walk of names (one `read_dir` per folder, which the scan already
  did); `readdir` lists only names, so the `stat` is needed anyway before a name can be shown
  without lying about it.
- **Changes while mounted.** See "Folder art in the areas" and "Re-reading changed folders"
  below. Opening a sidecar whose source size or mtime differs from what was listed fails
  with `ESTALE`. *Why:* the kernel may hold the listed size, so new bytes would be cut off or
  padded; a clear error until the folder is read again is better than wrong bytes. The
  re-read shows the new file under the same number with a new generation.
- **Reads** go to the request threads, a `pread` of the source file opened at `open` (one
  descriptor per open file, closed at release), with `FOPEN_KEEP_CACHE`. *Why the request
  threads:* the source may be a network share, and a slow read must not hold up the FUSE
  threads.
- **Untrusted paths (#67).** Images and every other file are untrusted, and SOURCE can change
  under a mount. Every sidecar is reached from a descriptor of SOURCE, one name at a time,
  with `O_NOFOLLOW` (`openat`), so a directory swapped for a link after the scan cannot lead
  outside. A sidecar that is itself a link is followed once: its target is read
  (`readlinkat`), resolved by name against the link's directory (or, if absolute, only when
  under SOURCE), must stay inside SOURCE and outside MOUNTPOINT, and is then opened the same
  way, so a link to a link, or through a linked folder, is not followed. Links that leave
  are skipped with a warning. Only regular files are served, opened with `O_NONBLOCK` so a
  FIFO put in place cannot hold the open. Nothing under MOUNTPOINT (when it is inside SOURCE)
  is a sidecar: reading it would be a FUSE request to sacdfs itself, the hang of #56. *Why
  not `openat2(RESOLVE_BENEATH)`:* it needs Linux 5.6, and the oldest distributions sacdfs
  supports run 4.18 to 5.4 (#58); it would also follow links into MOUNTPOINT inside SOURCE.
  Directory links are never followed, as for images.
- **NFS export and `readdir`.** Sidecar files are regular files with stable numbers, so the
  kernel NFS server and the container's NFS-Ganesha export them like track files, and their
  handles survive a restart (`tests/export.rs`, `tests/nfs-loopback.sh`). `readdir` lists them
  as regular files and sidecar folders as directories; `..` of a sidecar folder is its
  directory (with `--flatten`, the collapsed folder).
- **Nothing changes for an image without sidecars.** The audio files' bytes, names, numbers,
  generations and mtimes are untouched (`tree::tests::nothing_changes_for_images_and_their_files`,
  and the DSF hash gate of 93 tracks on real discs with a sidecar beside them); a directory
  with no sidecar costs nothing more; `--sidecars none` is 0.1.0's tree.
- **Folder art in the areas (#95).** The image sidecars of an image's folder (the jpg, jpeg,
  png and webp items, not playlists, `Artwork/` or `Scans/`; `sidecar::is_art`) are also
  listed inside each area folder, after the tracks, as the same node under a second name:
  one inode number, so a library server that walks both sees one image, and an NFS handle
  to it is the sidecar's usual one. *Why:* Lyrion Music Server, MPD (`albumart`) and
  MinimServer look for folder art only in the audio file's own folder, and Plex treated
  `Stereo/` as the album folder (found with real-client tests, 4 of 5 servers). *Why not
  embed the art in the DSF and WAV tags (the issue's other option):* it changes the audio
  files' bytes, which must stay identical (#82 requirement 3; the DSF hash gate stays
  93/93). *Why the same inode, not a copy:* nothing is stored or made, a changed cover
  changes in both places, and a server that walks both finds one file. *Selection:* it uses
  the `--sidecars` list, so a removed extension is not shown in either place and `none` shows
  no art; there is no separate option, since the owner's wish is that nothing is
  configured client-side (#95 acceptance) and the list already gives control. *Source of the
  list:* `Art` holds the image sidecars of the image's folder (with `--flatten`, of the chain,
  as the collapsed folder shows them); images in one folder share it. `nlink` stays 1 (the
  kernel does not need it, and a changing count would need an invalidation).
  *MPD's name (#148):* MPD's `albumart` finds a cover only in a file named `cover.<ext>`, so
  when the art holds no `cover.*` (any case), the area listing adds one more name for the first
  of it (`folder.*`, then `front.*`, then by name) as `cover.<ext>`, with the extension in
  lower case and `jpeg` as `jpg`: the same node again, not a copy, and only in the areas
  (the album folder is the source's). *Why only when there is no `cover.*`:* the owner's own
  cover wins, and a library that already names its covers sees no change. *Alternative:*
  name the art `cover.*` in the source, which a library of thousands cannot be asked to do.
  *Trade-off:* a `Cover.PNG` (other case) counts as a cover and gets no alias, so if MPD matches
  names by case (not tested), it finds no art for it; the file can be renamed.
- **Re-reading changed folders (#101).** A tree made `watching` (the mount, when SOURCE is a
  directory) re-reads a folder of SOURCE when it is listed or looked up and the folder's
  modification time differs from the one noted at its last reading (`Listing::watch`,
  compared at most once per `REFRESH`, 2 s, so that a busy scanner costs one `stat` per
  folder per interval). *Why mtime:* it is what a directory's change is, costs one `stat`,
  and works over NFS and SMB sources, where inotify does not. A re-read reads that folder's
  names again (`SourceDir::reread`), keeps the sub-folders already known as they were read
  (they re-read themselves when listed) and scans new ones. *Unchanged entries keep their
  nodes and numbers:* images are not opened again, sidecar folders keep their node (they list
  themselves), a sidecar file keeps its node if its size and time are the same, else it gets a
  new node under the same number (numbers are by path, so a new generation shows the
  change). *Removed entries:* they leave the listing, so a lookup gives `ENOENT`, but the node
  stays in the tree (its number still resolves, as an NFS handle needs, and a handle open
  before keeps reading). *Folders that were skipped:* a sub-folder with no image is not in
  the tree, and when it gains one its parent's mtime does not move, so a copy made folder first, files after would
  be missed; the watch list of a folder therefore includes the mtimes of its skipped
  sub-folders. Folders deeper than that are noticed when something above them changes. With
  `--flatten`, a collapsed chain re-reads its sidecars (and the art of its areas) but keeps
  its image: *why:* the entry was named and numbered after that image when its parent was
  listed. The folder's shown modification time follows its source's. *Art* has its own
  version: an area is out of date when its folder's art changed, since the area has no
  source folder of its own. *Kernel caches:* the 24-hour `TTL` and cached listings would
  hide all of this, so for a folder that is re-read (and for the names looked up in it)
  entries are remembered for the refresh interval only, and its `readdir` is not cached.
  Everything else (the audio, the images' own areas) keeps the 24 hours, and a mount that
  does not watch (a single image as SOURCE) is as before. *Why not inotify or
  `inval_entry` notifications:* inotify does not see changes made by other clients of a
  network source, and a short TTL needs no notifier thread. *Not done:* a changed image file
  under the same name (an image's changes are for `handles`), and a folder collapsed by
  `--flatten` that gains a second image.
- **Not done:** embedding art in the DSF and WAV tags (a separate question, #79) and rewriting
  playlist entries when `--flatten` or `--format` change paths (kept byte for byte, #75).

## File identity and NFS export

For the server-side deployment (#17, #18): an NFS client keeps file handles, which hold the
inode and generation numbers, across server restarts. Numbers handed out in walk order would
change on every mount, and clients would get `ESTALE` on open files and cached directories
after every NAS reboot or app update. In the container, NFS-Ganesha's handles must not depend
on the mount's device number either (`fsid_type = Major64`, #218). *Why that setting:* FUSE
reports no filesystem id, so Ganesha uses the device number, and a new mount can get another
one. *Alternatives:* `Filesystem_Id` only sets what clients see, not what handles hold; `One64`
and the others still include the varying minor number; a fixed device number can't be asked of the
kernel for a FUSE mount, and the kernel NFS server needs a privileged container (see
deployment.md). *Trade-off:* a handle can't tell two filesystems of one export apart, which an
export of one FUSE mount doesn't have; and old clients go stale once on the upgrade.

**Inode numbers** (`src/ident.rs`) are 64 bits:

```
 63                                16 15 14 13 12         5  4  3      0
+------------------------------------+-----+--+------------+--+--------+
| key: 48 bits of SHA-256 of a path  |kind |sl| track no.  |wv| flavour|
+------------------------------------+-----+--+------------+--+--------+
```

- A source directory's key comes from its path relative to SOURCE; an image's from the
  image's relative path (with a domain byte, so a directory and an image never share an
  input). Kind 0 is that directory itself (a source directory, an image's directory, or a
  collapsed `--flatten` folder, which takes its image's number); kind 1 an area (`sl` is the
  slot); kind 2 a track file: slot, track number (1 to 255, unique in an area by
  construction), `wv` for WAV, and 4 bits of the PCM flavour (0 for DSF). The root is 1, as
  FUSE requires; key 0 is never used.
- *Why the image's key and the entry bits in one number:* after a restart the number is all
  sacdfs gets (the kernel asks for `.` of it). With the image's key in the number, the image
  is found from the index of names made at mount, and the area and file from the low bits,
  without opening any other image. A plain hash of every entry would need a persistent table,
  or all images opened, to be found again.
- *Why SHA-256:* its output is fixed forever (std's `DefaultHasher` may change between Rust
  releases, which would turn an app update into a restart with all handles stale), the crate
  is already a dependency, and hashing every name walked costs little at mount. A unit test
  pins one number so that an accidental change of the scheme fails.
- *Why 48 bits of key:* 16 bits are needed for the entries; with 48 bits, a collision
  becomes likely only around 16 million paths, and is detected anyway.
- **Collisions** are detected at mount, where every key is computed from the names walked
  (`Ids::new`). The path that sorts first (domain, then bytes) keeps the key; every other one
  takes the first free key of the hash of its path with an attempt number appended (1, 2,
  ...), free meaning no path's first key and not yet handed out. This does not depend on the
  walk order, but a fallback key can change on a later mount if a new path's first key takes
  it. A second area in the same slot (only a hostile image has one) is not shown, rather than
  given a number that is not its own.
- A passed-through sidecar file (#79) has a key of its own domain over its relative path and
  entry bits 0; a sidecar folder is a directory ([Sidecar files](#sidecar-files-79-75)).
- **What changes the numbers:** renaming or moving a folder or image changes its number and
  everything below it. `--flatten` changes only the folders that collapse away. `--format`
  hides files without changing the others'. `--pcm-rate`, `--pcm-cutoff` and `--pcm-gain`
  change the WAV files' 4 flavour bits (so usually their numbers) and always their
  generations. Adding or removing an image changes no other number, except in the
  collision case above.

**Multichannel album suffix (#97).** The album tag of a multichannel area's DSF and WAV files
ends in ` (Multichannel)` unless `--no-multichannel-album-suffix`. *Owner decision:* multichannel
copies get their own album name in the tags by default, with an option to turn it off.
*Why:* Navidrome, LMS, MPD and MinimServer all showed a disc's two copies as one album.
*Sources:* the client tests on #82. *Alternatives:* no change; a separate album-artist or
comment tag (servers group by album name); hiding an area (loses content). *Trade-off:* the
tag bytes differ from an extract's, which the owner allows (only audio must match); the
output versions move (DSF 3, WAV 4). The option is a process-wide switch in `dsf.rs`, and
toggling it does not move mtimes. A tags file's album name wins and gets no suffix
([Tag layer](#tag-layer-81)).

**Generation numbers.** A track file's generation is 32 bits (all the kernel keeps) of a hash
of its image's sector count and mtime, its format, size and an output version per format, and
for WAV the full PCM flavour; never 0. A handle to a file whose bytes may have changed then
fails with `ESTALE` instead of serving other audio. The output versions (`DSF_VERSION`,
`WAV_VERSION`) are bumped by any change that makes the same track's bytes differ, unless
the change reaches only some tracks and tells which, as a tags file (#81) and text filled in
for a disc without it ([#185](#discs-without-text-185)) do.
Directories have generation 0: they are containers found by name, and a handle to one whose
content changed should still work (a missing area or file is then not found).
*Why not the image's file size:* the sector count is the image's size in sectors, already in
the TOC; together with the mtime it catches any rewrite, without another `stat` per lookup.
**Modification times (#64).** A track file's bytes depend on more than its image: on the
output version of its format, and for WAV on `--pcm-cutoff` and `--pcm-gain`, which keep the
file's size. Showing the image's mtime alone, a copy by size and mtime (rsync's default check)
or a library scanner keyed on mtime would miss such a change. So a file's mtime is its image's
plus two offsets, in whole seconds:

- for WAV, a PCM offset: 0 for the default options, and for every other cutoff and gain (in
  steps of 0.01 dB from -60 to +24 dB) a different number below 25,203 (7 hours);
- a version offset: 25,203 s per output version after the first (`DSF_VERSION`,
  `WAV_VERSION`), more than any PCM offset.

A directory shows the newest mtime of what is shown below it (#123), see
[Directory times](#directory-times-123) below.

- *Why derive it, not document "re-copy after changing options":* rsync, Plex, Roon and the
  like rely on mtime and would silently keep stale WAV copies or analyses. Deriving costs
  nothing per request (an addition) and needs no state.
- *Stable across remounts:* the mtime is a function of the image's mtime, the output version
  and the options only, so the same options always show the same times.
- *Why a version step larger than every PCM offset:* a version bump then moves every file
  past every time it showed under the previous version, whatever the options, so even a tool
  that only looks at newer files (beets' `update`) sees it. Considered: the date of the bump
  (mtime never older than it). That moves only files whose image predates the date, and misses
  an image changed after the bump was made but served by the old version until the upgrade.
- *Why an exact offset, not a hash of the options:* every cutoff and 0.01 dB gain step gets its
  own offset, so two option sets never show the same mtime (gains closer than 0.01 dB share one,
  and the CLI accepts any gain in range).
  The default options at version 1 add nothing, so a mount with defaults shows exactly what
  0.1 showed. Changing the options back and forth moves the mtime back and forth: still a
  change for rsync, Plex and Roon, though not for a scanner that wants newer files (beets
  reads only tags, which the options do not change).
- *Why not the rate:* `--pcm-rate` changes the file's size (and usually its inode number),
  which every tool already sees.
- *Cost:* a file can show a time ahead of its image's (up to 7 hours with non-default
  options, 7 more per version bump), so an image changed in the last hours can show a time
  slightly in the future. Harmless on a read-only filesystem; `tar` extracting a copy may warn.
- **Generations stay consistent:** everything the mtime is derived from (the image's mtime,
  the output version, the full PCM flavour) is already in the generation hash, so a file whose
  mtime changed also has a new generation, and an NFS handle to the old bytes is stale. The
  generation hash itself is unchanged from 0.1, so an upgrade keeps every handle valid. Unit
  tests (`ident::tests::mtime_offsets`, `tree::tests::pcm_options_change_wav_numbers_and_generations_only`)
  and a mount test (`tests/probes.rs`) check this.

### Directory times (#123)

A directory's mtime is the newest of its source directory's own mtime and the mtimes of
everything shown below it, at any depth: source directories, sidecar files and folders, and
each image's track files (its image's mtime moved by the largest offset of the formats shown,
`ident::newest_offset`, which counts the extra version of a file with filled-in text whether
the image has such files or not, [#185](#discs-without-text-185)). With an image below, the
other times count one step later too (`ident::FILLED_STEP`), for the same reason. An image's own folder and its area folders show their image's files
and their folder art. Up to the mount root, then, every directory moves when anything below it
moves: an output-version bump, new tags (which come with one), a new, removed or changed
sidecar or image.

- *Why:* Roon (and other scanners that keep a directory's mtime from the last scan) skip a
  directory whose mtime did not change, and everything below it. Found testing 0.2.0-rc.1 with
  Roon (#123): an upgrade moved every DSF file's mtime and a new playlist sat next to an image,
  yet Roon reported nothing, because the directories above them kept their source's mtime. The
  owner decided to fix it and ship the fix in 0.2.0 (as rc.2).
- *How, at mount:* the scan `stat`s every image and sidecar it notes (one `stat` each, beside
  the `readdir` it already does; over NFS the attributes usually come with the listing) and keeps
  a `tree::Stamp` per source directory: its own mtime, the newest of the rest, and the newest
  image. The output offset is added when the time is shown, so `--format` and the PCM options
  count without a rescan. No disc is opened for it.
- *How, while mounted (#101):* every time a directory is re-read, its stamp is passed up to each
  ancestor (`Tree::bump`). A change deep in the tree must also surface without anything above
  it being listed, since that is exactly what a scanner skips; so when the root's attributes are
  asked for (a scan starts there), at most once per refresh interval (2 s), `Tree::sweep`
  compares the mtime of every known source directory and sidecar folder (one `stat` each) and
  re-reads only those that changed (their own entries; a new subdirectory in full). A listing
  whose directory's shown mtime moved since it was made is compared with its source at once,
  not after the refresh interval: in rc.2 the sweep could move a folder's mtime while its
  re-read was still throttled, and an SMB client, which keeps a listing until the folder's mtime
  moves, then kept the old listing under the new mtime for good (a new playlist never listed).
  Change detection still uses each listing's stored source mtimes, never the shown one.
  A directory's *first* listing compares its source mtime (each directory's of a `--flatten`
  chain) with the one recorded when what it would be listed from was read (the mount's scan,
  or its parent's re-read), and re-reads it if they differ. In rc.3 a folder not listed since
  the mount was listed from the scan unchecked, so a sidecar added after the mount never
  showed in it (the Roon-after-restart case). *Why compare, not always re-read:* one `stat`
  per folder, against a `readdir` and a `stat` per entry, on a first scan of every folder. *Considered:*
  inotify (not on NFS or SMB sources, which are the NAS case); re-reading a directory's
  descendants on its own `getattr` (the same `stat`s, made many times per scan).
- *Deterministic, no wall clock:* the time is a maximum of source mtimes and fixed offsets, so a
  fresh mount of the same source shows the same times (the NFS case, #17). While mounted the
  times only move forward; if the newest thing below is deleted, its directory's mtime (when it
  changed) is newer anyway, and a remount shows the new maximum. A file changed in place moves
  no directory mtime on the source, so the sweep does not see it (#101 does not either); its own
  mtime moves, for a scanner that reads it.
- *ctime moves with it, generations do not:* the kernel and NFS servers use ctime as the change
  attribute, so clients revalidate a directory's cached listing when it moves. A directory's
  inode number and generation (0) stay the same: its handle stays valid; only the time moves.
- *`--flatten`:* a collapsed folder shows the newest of its whole chain (the chain's directories
  and sidecars and its one image); a change in any directory of the chain moves it, as its
  source path is the chain's top.
- *Limits:* a folder gaining its first image is found by its parent's re-read (#101), not the
  sweep (which knows only folders that held something). An area folder's copy of the folder art
  follows when the area is re-read; the album folder, where the art file is, moves at once.
- *Cost:* one `stat` per image and sidecar at mount; one `stat` per source directory per sweep
  (at most every 2 s, only while the root is looked at). Tests:
  `tree::tests::directory_mtimes_show_the_newest_below`,
  `tree::tests::a_change_deep_below_moves_every_ancestor_and_matches_a_remount`, and the mount
  test `directory_mtimes_move_with_what_is_below_through_the_kernel`.

**Export support.** `init` asks for `FUSE_EXPORT_SUPPORT`. `lookup` answers `.` (the inode
itself) and `..` (its directory's parent), which the kernel sends to turn a file handle into
an inode and to connect a directory to the root. `Tree::get` of a number not made yet in this
mount finds it from `Ids`, the index of keys built at mount from the names already walked:
the key's chain of parent directories, then from the root down, each directory on the way is
loaded (by inode number, not name, so de-duplicated names and `--flatten` do not matter), then
the image's area and file. That opens exactly the images a lookup of the path would open (the
images of the directories on the way, as when a client walks there), never all of them.

**Restarting under an export.** The NFS server's open files and export cache hold the old FUSE
mount, which then cannot be unmounted; and with the export removed, a client is told `ESTALE`.
So the server is stopped around a restart of sacdfs, and its clients wait as for a server
reboot. NFSv3 is stateless and reads on; NFSv4 reclaims its open state in the grace period.

**Stopping a busy mount.** On SIGINT, SIGTERM or SIGHUP, a mount that cannot be unmounted (files open,
or held by an NFS server) is detached (`MNT_DETACH`, or `fusermount3 -u -z`) with a warning;
sacdfs serves the files still open and exits when the last is closed. *Why:* otherwise a
container or service manager waiting for sacdfs to stop would hang until it killed it,
leaving a dead mount behind; for users other than root, `fusermount3 -u -z` already detaches, so root
behaves the same.

**Restarting on the same mount point (#51).** A sacdfs stopping (after a signal, or after
`fusermount3 -u` from outside) unmounts or detaches only its own mount, and a new sacdfs never
mounts over a live FUSE mount. Unmounting goes by path, and nothing else names a mount:
`umount2` and `fusermount3 -u` act on whatever is on top at the path. Three rules:

- sacdfs serves the session with `Session::spawn` and never drops fuser's mount object, so
  the session's end unmounts nothing. *Why:* `Session::run` unmounts by path when the session
  ends, and the check meant to skip that when the mount is already gone always passes in
  fuser 0.18.0 (`is_mounted` reads `POLLERR` the wrong way round; fixed in fuser's main branch,
  not released). Every sacdfs unmounted from outside therefore unmounted the path again on
  its way out, removing a sacdfs mounted there in the meantime. Measured, 200 restarts each,
  as a user and as root: the new mount was lost in 200 of 200 when the old one was detached
  from outside (`fusermount3 -u -z`, a file open) and exited after the new one mounted, and in
  200 of 200 when the old one, unmounted, was held stopped until the new one had mounted.
  With the fix, 2 of about 5,000 fixed-binary restarts still lost the new mount; that was the
  device-number reuse of the next rule (#54). This was the failure seen in #51.
- On SIGINT, SIGTERM or SIGHUP, sacdfs unmounts (or detaches, if busy) only if the mount on top at
  its path still has its own device number, read from `/proc/self/mountinfo` right after
  mounting, **and** its own FUSE connection is still up (`poll` on a duplicate of its
  `/dev/fuse` descriptor shows no `POLLERR`); otherwise it logs why and leaves the path alone.
  The same check guards the detach after a failed session. *Why the connection (#54):* the
  kernel hands a freed device number (and mount id) to the next mount at once, so after an
  unmount from outside, a new mount on the same path usually has the old one's number (178 of
  200 restarts on Linux 7.2). An old sacdfs that got its signal only then (held stopped,
  busy, or simply late: a service manager sends SIGTERM and then SIGCONT) saw "its" number on
  top and unmounted the new mount: 28 of 50 restarts as a user and 13 of 20 as root, every
  one with the number reused. The kernel aborts a mount's FUSE connection when its superblock
  goes, before the number is freed, so a connection still up means the number read just
  before is this mount's. Both checks ask the file system nothing, so they cannot wait on a
  stopping session. With it: 0 lost in 200 such restarts, and 0 in a 14,000-iteration
  restart loop (user and root, 10 stop scenarios, 2,800 of them late-signal).
  Rejected: the 64-bit unique mount id (`statmount`/`listmount`, Linux 6.8+), which needs raw
  syscalls and a fallback on older kernels for the same answer; holding an `O_PATH` descriptor
  on the mount to pin its number, which makes a plain `fusermount3 -u` fail as busy.
  The #53 note that "neither sacdfs logged an unmount" came from runs at the default log
  level, where "signal N: unmounting" (info) is not shown.
- A new sacdfs whose mount point is still a live FUSE mount of its user's (the previous one
  has not unmounted yet) waits for it to go, up to 10 s, then fails with "still mounted there
  after 10 s"; a dead one is cleared at once (below, #57). *Why:* mounted
  over the old mount, the new one sat inside it. As root, the old one's unmount then removed
  the new mount instead (189 of 200 restarts started up to 20 ms before the old one was
  stopped failed); as a user, `fusermount3` refuses to mount over another user mount
  (`Permission denied`), so the restart failed (183 of 200; 171 of 200 with a file open), and
  when it raced the old one's detach the new mount went into the detached tree and vanished
  (seen once). The wait costs one read of
  the mount table when the mount point is free.

Considered: unmounting by the mount's identity instead of its path, but Linux has no unmount
by mount id or descriptor (`umount2` on `/proc/self/fd/N` follows to the top mount), and
aborting through `/sys/fs/fuse/connections` needs root and leaves a dead mount behind.
Mounting through `Session::from_fd` with sacdfs's own `fusermount3` handshake would also work
but duplicates fuser's mount code. When fuser releases its fix, `serve` can go back to
`Session::run`. The container is unaffected: sacdfs starts once per container, each start has
a new mount namespace, and the entry point clears a stale mount first.

**Mount safety (#55, #56, #57).** Three rules about the mount itself, from a review of
other FUSE projects' changelogs for problems that recur (libfuse, fuser, rclone, mergerfs,
gocryptfs, ratarmount and others):

- **The mount is named `sacdfs`, not after SOURCE** (the source field of
  `/proc/self/mountinfo`, `df` and `mount`). *Why:* fuser 0.18 passes the name to
  `fusermount3` as `fsname=...` inside its `-o` list without escaping it, so a comma in the
  path ended the name and the rest became options: `Bach, J.S.` failed with "unknown option
  ' J.S.'", and a folder named `x,allow_other` would have applied the option (rclone 1.58.1
  had the same bug; fuser fixed it in #424, not released). Escaping `\` and `,` as libfuse's
  `fuse_opt_add_opt_escaped` does is right only on that path: as root, fuser calls mount(2)
  itself with the name as the source argument, which nothing unescapes, so the mount table
  would show the backslashes; and fuser picks the path inside `Session::new` (mount(2) first,
  `fusermount3` on EPERM), so sacdfs cannot know which one it is escaping for. A fixed name
  is right on both paths, keeps the library's path out of a table every user can read, and
  identifies a dead mount as sacdfs's as root too, where the type is plain `fuse` (the
  `sacdfs` subtype is a `fusermount3` option; mounted by a user, the type is `fuse.sacdfs`).
  The own-mount check (#51) compares device numbers, never the name, so it is unaffected.
  The source is in the log (`-v`) instead. Tested with SOURCE paths holding `,`, `\`, a space,
  `,ro`, `,allow_other` and `\,rw`, as a directory and as the image file, as a user and as
  root.
- **Nothing sacdfs reads or writes may be inside its own mount.** It refuses to start, before
  the scan, when the canonical SOURCE is the mount point or lies inside it (`sacdfs X X`,
  `sacdfs X/sub X`), and when an `--index-cache` directory lies inside it; after the scan,
  when the mount point lies inside SOURCE and holds images. *Why:* sacdfs reads images by
  their source paths; inside the mount point those paths lead back into sacdfs, so each read
  is a FUSE request to itself that waits for itself. `sacdfs -f X X` mounted, and the first
  `ls X` (or anything else looking inside the new mount) hung in D state; even SIGKILL left
  it a zombie, and only an abort through `/sys/fs/fuse/connections` freed it (ratarmount
  0.14.2 and gocryptfs 1.4.2 fixed the same hang). Both paths are canonical, so a symbolic
  link cannot hide it. The default cache directory inside the mount point (a mount point at
  `$HOME`) is not an error: the cache is turned off with a warning, as for any cache that
  cannot be used, since the cache is only ever a speed-up. An empty mount point inside SOURCE
  is fine (`sacdfs ~/Music ~/Music/sacd`): sacdfs scans before mounting and never again.
  Not covered: an image that is a symbolic link to a file inside the mount point (the scan,
  in `tree.rs`, would have to resolve every link; a follow-up).
- **A non-empty mount point is allowed, with a warning.** *Why:* libfuse 3 and `fusermount3`
  allow it (libfuse 3.0 dropped the `nonempty` check), a service's mount point may hold a
  placeholder, and refusing it would refuse setups that work; the cases that can hang are
  refused precisely by the rule above. The warning says that the files there are hidden.
- **A dead mount is cleared at start, quickly** (#57). After sacdfs is killed (SIGKILL, a
  crash, the OOM killer), its mount stays, and every access fails with ENOTCONN. A new sacdfs
  waited the full 10 s for it to go and then failed with "already a FUSE mount (another
  sacdfs?)", so a service manager's restart loop never recovered. Now, while the mount point
  is a FUSE mount, sacdfs probes it with `statfs`, at once and then every 250 ms, each probe
  bounded to 500 ms. On ENOTCONN, a dead sacdfs mount (type `fuse.sacdfs`, or `fuse` named
  `sacdfs`) of the same user (its `user_id=` option) is detached, while it is still the mount
  on top at the path (by device number, as for the signal unmount), and sacdfs mounts; a dead
  mount that isn't sacdfs's, or a FUSE mount of another user, fails at once with what is
  there. A live mount of this user's is still waited for, up to 10 s (#51), and the message
  then says whether it answered. Measured: the restart after SIGKILL mounts in about 10 ms
  (as a user and as root), against 10 s and a failure before. *Why `statfs`, not `stat`:*
  the kernel answers `stat` from its attribute cache (valid for a day) without asking the
  server, so a dead mount can `stat` fine; a FUSE `statfs` always goes to the server. *Why
  bounded, and in a child process:* on a live mount the probe waits for the server, which
  may never answer (stopped, hung), and that wait cannot be interrupted; a thread would
  break the single-threaded fork of `daemonize` that follows, so the probe runs in a forked
  child that is killed and left behind if it takes too long. *Why only sacdfs's own:* #51's
  guarantee is that sacdfs never unmounts a mount it doesn't own; a dead mount of another
  file system is reported, not touched. libfuse's `auto_unmount` (a `fusermount3` that stays
  and unmounts when the server dies) was considered: it needs the `fusermount3` path, which
  root doesn't take, and an extra process per mount; clearing at the next start (as mergerfs
  2.42 does) covers both.
- **The signal handlers are set up before mounting** (#57). *Why:* they were set up after
  mounting, so a SIGTERM in between (a service stopped right after starting) killed sacdfs
  with its mount in place: a dead mount. Measured on `main`, SIGTERMs 0 to 30 ms after the
  start left a dead mount in 7 of 100 starts; with the fix, 0 of 100 (and 0 of 60 in the
  test). A signal from just before the mount on is held until the mount exists and then
  unmounts it; one that arrived before sacdfs was about to mount stops it without mounting.
  SIGHUP does what SIGTERM does: sacdfs has nothing to reload, and a foreground sacdfs whose
  terminal closes should not leave a mount behind; the daemon is in its own session, so a
  terminal closing doesn't reach it.

**Cost.** Mounting hashes every source directory and image name once and keeps one map entry
per directory and image (tens of bytes each). The node table is a hash map by inode number.
Lookups of files compute their generation (one SHA-256 of about 70 bytes, well under a
microsecond). Measured in [performance.md](performance.md#mount-listing-and-tag-walk-cost).

## DSF files

Since 0.2.0 scarletbook writes the DSF files (`output::Layout`, specification 8, 8.2; #28):
sacdfs builds a track's layout (`Layout::new` with the TOC, the track's frame range and
`Format::Dsf`) when its directory is listed, so that the file size is known, and serves every
read from it: `Layout::positions_for` gives the frames a byte range needs, sacdfs decodes them
(its own pool and frame cache), and `Layout::fill` writes the bytes, header and tag included.
sacdfs's own DSF header, block layout, bit reversal and ID3 writer were removed (`dsf.rs` went
from 539 lines to 144, all of it the options below).

- The file: the DSD chunk (28 bytes), the fmt chunk (52 bytes), the data chunk (12-byte header,
  then audio in 4096-byte blocks per channel, channel after channel, earliest sample in the
  least significant bit, the last block of each channel padded with zeros), and the ID3v2.4 tag,
  pointed to by the DSD chunk. Audio: the track's timeline positions `[start_frame, end_frame)`.
- **Writer's choices** (`dsf::options`): the ID3 tag without a footer or padding, its frames in
  sacdfs's order (TIT2 TPE1 TPE2 TALB TRCK TPOS TCOM TDRC TSRC TCON TPUB TCOP, others after),
  the album artist as TPE2, the date as TDRC; the channel type of an area without a defined
  loudspeaker layout from its channel count ([Speaker layouts](#speaker-layouts)). *Why:*
  they are what sacdfs 0.1 wrote, so that a file's bytes changed only where the library's
  rules differ.
- **Tag items** (`dsf::tag_set`): the library's from the TOC (`TagSet::from_toc`), with two
  choices of sacdfs's: the album title, artist, publisher and copyright from the album text,
  else the disc text (*why:* a set's discs share the album title, so players group them as one
  album; the library's default takes the disc's first), and no TPOS for a one-disc set
  ("1/1" says nothing).
- **What changed against 0.1** (all 93 real-disc files compared, header and tag): 53 are
  byte-identical; the other 40 differ only in TCOP, which the library writes as ID3v2.4 asks:
  the date's year, a space, then the copyright (a stored "Label 2007" becomes "2007 Label
  2007"); their tag and size grow by 5 bytes. The audio is the same (DSF gate 93/93). The DSF
  output version moved to 2, so those files' NFS generations and mtimes moved as #64 intends.
  *Decision (owner, #28):* adopt the library's writer, accept the conformant TCOP; preserving
  Roon play history and avoiding a one-time re-scan are not requirements. *Trade-off:* players and taggers that keyed on the old
  bytes or mtime see the files as changed once.
- **Serving:** the frame cache holds decoded frames as scarletbook gives them (interleaved);
  `fill` gathers each channel's bytes and reverses their bits per read. *Why per read, not
  once at decode as sacdfs did:* one copy of the layout code, and the kernel's page cache
  serves repeated reads; `fill` alone runs at 2.5 to 2.9 GB/s on one core here, above the
  mount's straight-read rate. A frame not decoded (a failed read) is filled with zeros.
- A position with no frame, or with an invalid DST frame, is filled as scarletbook fills it
  (0x69 / 0x55); a warning is logged once per file. A track the library refuses (its range
  fails the checks of specification 4, 4.8) is not shown.

## WAV files

A WAV file is a RIFF/WAVE file with a `WAVE_FORMAT_EXTENSIBLE` fmt chunk (24-bit PCM, the
channel mask below, the PCM subformat), padding, the data chunk, and then the tags:

```text
RIFF <size> WAVE   fmt <40>   JUNK <zeros>   data <frames x S x C x 3>   LIST <INFO ...>   id3 <ID3v2.4>
```

- **Rate and depth** (owner decisions, #11): `--pcm-rate` 176 400 Hz by default, or 88 200 or
  352 800; always 24-bit. *Why 176.4 kHz:* it is DSD64 divided by exactly 16, so the
  conversion needs no fractional resampling (cheapest and cleanest), and it holds the 40 kHz
  band with room for the filter's transition. *Why 24-bit only:* it needs no dither, so the
  output stays deterministic and seekable (a frame's PCM never depends on what was converted
  before it). 44.1 kHz/16-bit would need both.
- **Filter and level** (owner decisions, #11; the converter is #5): band-limited to 40 kHz by
  default, 30 kHz with `--pcm-cutoff 30`, following Daniel Weiss.
  `--pcm-cutoff wide` (about 80 kHz) is offered at 176.4 and 352.8 kHz only. *Why the option:*
  Philips listening tests (Nuijten and Reefman, AES paper 5399, 2001) found wideband filters at
  80 or 100 kHz undetected where 50 kHz was detected, and other sources (Reefman and Janssen,
  Stuart, Craven, JAES 2004) favour a wider channel. *Why not the default:* the owner keeps
  Weiss's 40 kHz, which keeps DSD's ultrasonic noise out of unknown playback chains; the
  listening tests don't answer that concern. *Why refused at 88.2 kHz:* that rate holds
  only up to 44.1 kHz, so sacdfs refuses `wide` with `--pcm-rate 88200` at startup, with a
  message, instead of mounting a filter that cannot be what was asked for. `--pcm-gain` from
  -60 to +24 dB, 0 by default, because 100 % DSD modulation is PCM full scale, so 0 dB never
  clips. A sample that would clip with a gain is clamped, and the log says once per file how
  many samples were clipped. *The range* guards against typos: +24 dB clips almost any disc,
  and below -60 dB the music is gone.
- **Samples per frame** S: an SACD frame is 1/75 s, so S = 1176, 2352 or 4704 at the three
  rates. Data byte `d` lies in frame `d / (S x C x 3)`; frames are stored one after the other,
  each as the converter writes it (interleaved samples, little-endian). The data length is
  `frames x S x C x 3`, so every size is known from the TOC alone.
- **RF64:** when the file does not fit RIFF's 32-bit sizes (over 4 GiB), it starts `RF64`,
  with a `ds64` chunk after `WAVE` holding the 64-bit RIFF size, data size and sample count,
  and both 32-bit sizes set to 0xFFFFFFFF. *Why only then:* plain RIFF is what every program
  reads; RF64 (EBU Tech 3306) is read by FFmpeg, sox, libsndfile and most current software but
  not by everything (Python's `wave`, for one). Stereo at 176.4 kHz reaches 4 GiB after about
  67 minutes, six channels after about 22, so long multichannel tracks need it. The decision is
  made per file, from its size, so no `JUNK` placeholder for a later `ds64` is written.
- **Channel mask and order:** from the area's loudspeaker layout, like the DSF channel type;
  see [Speaker layouts](#speaker-layouts).
- **Tags:** the DSF file's ID3v2.4 tag (scarletbook's, the bytes of its layout's tag region),
  in an `id3 ` chunk (FFmpeg, TagLib, mutagen, foobar2000 and mp3tag read it), and a
  `LIST`/`INFO` chunk for software that reads only that: INAM (title), IART (performer), IPRD
  (album), ITRK (the track number), ICRD (date), IGNR (the first genre), the first value of the
  same tag items, UTF-8 and NUL-terminated. *Why INFO from the items, not from the ID3 frames:*
  the library has no INFO writer, and built from one `TagSet` the two cannot disagree. The WAV
  output version moved to 2 with the DSF one (#28): the same 40 files' `id3 ` chunk changed. A chunk with
  nothing to hold is left out. *Why after the audio:* so that the header, and with it the
  offset of every audio byte, does not depend on the text, as in the DSF files. FFmpeg and
  libsndfile read chunks after the data.
- **Padding, so that tag reads never convert.** The `JUNK` chunk makes the audio start at
  256 KiB or a little later (less than 4 KiB more) and end on a 4 KiB page boundary. *Why:*
  the kernel reads a FUSE file in whole pages, and the first read of a file asks for up to twice
  the request: 16 KiB for a small read, 256 KiB for the 128 KiB that Python 3.14's buffered
  files read (mutagen, beets). Without padding, the audio would start at byte 68, so a tagger
  reading the header would make sacdfs locate, decode and convert the track's first frames
  (measured: 1.1 MB of image reads per file for the first 200 bytes, against 0 with padding),
  and the page with the end of the audio would hold the start of the tags. The cost is 256 KiB
  of zeros per file, about 0.05 % of a typical track. The padding takes up the ds64 chunk in
  RF64 files, so the audio offset is the same for both.
  *What padding cannot avoid:* a tagger that walks the chunks (mutagen, TagLib) reads the 8-byte
  header of the data chunk, which RIFF puts right before the audio, and the kernel then reads
  that page and about 16 KiB after it. So such a tagger converts the track's first one or two
  frames, a few milliseconds per file.
  *Not for DSF:* a DSF file's chunk layout is fixed (the audio starts at byte 92), so reading a
  DSF file's header still decodes its first frames ([Read-ahead](#read-ahead)). Reading its tag
  decodes almost nothing.
- Header and tag bytes are built when the area directory is listed, like the DSF files', so
  `stat` and `ls -l` never read the image, and reading the first 256 KiB or the tags of a WAV
  file decodes and converts nothing.

## Speaker layouts

The WAV channel mask (`dwChannelMask`) and the DSF channel type come from the area's
loudspeaker layout, which scarletbook gives as `Area::loudspeaker_layout` (from the channel
count and the low 3 bits of the area configuration byte, specification 3, 3.4.1) with the
speaker of each channel in decoded-frame order (`Area::channel_speakers`):

| Layout | Speakers, in decoded order | WAV mask | DSF channel type |
|---|---|---|---|
| Stereo (2 channels, configuration 0) | FL FR | 0x3 | 2, stereo |
| 5-channel 3/2 (5, configuration 3) | FL FR C SL SR | 0x37 (FL FR FC BL BR) | 6, 5 channels |
| 5.1 (6, configuration 4) | FL FR C LFE SL SR | 0x3F (FL FR FC LFE BL BR) | 7, 5.1 channels |
| Anything else (`Unspecified`) | not defined | by channel count | by channel count |

- *Why from the disc, not the count:* the count only guesses which speakers a disc meant;
  the layout is what the disc states. sacdfs keeps no SACD format knowledge of its own (see
  [Engine](#engine)), so it reads the layout through scarletbook's typed API instead of
  decoding the configuration byte itself.
- *No reordering:* a WAV file's channels must follow the order of their mask bits, and the
  DSF types fix the order FL FR C (LFE) SL SR. scarletbook's decoded-frame order is that
  order for all three layouts (its order is informative, the order existing tools label
  extracted channels in), so the samples are written as decoded. A unit test checks that the
  speakers' mask bits rise in decoded order, so a change upstream would fail it.
- *Surround on the back bits (BL BR), not the side bits:* what 5.0 and 5.1 mean to most
  players and to FFmpeg (0x37 is its "5.0", 0x3F its "5.1"; the side bits give "5.0(side)"
  and "5.1(side)"). The DSF types have one surround pair, so they do not distinguish.
- **`Unspecified`** (any other combination: 1, 3 or 4 channels, or a stereo or 5-channel
  count with another configuration; seen only on synthetic test images so far) gets the
  layout of the channel count: 1 FC; 2 FL FR; 3 FL FR FC; 4 FL FR BL BR; 5 FL FR FC BL BR;
  6 FL FR FC LFE BL BR (DSF types 1 mono, 2 stereo, 3 three channels, 4 quad, 6 five channels,
  7 5.1). *Why:* the disc does not say which speakers it means, the count gives players a
  usable default, and a file is always shown. sacdfs logs it at Info once per disc, when the
  image is opened, naming the areas and their stored fields, so that a surprising mask can be
  traced to the disc.

## Reading a range of a DSF file

A read of `[offset, offset + len)`:

1. Header and tag bytes come from the file's scarletbook layout, built once per file.
2. Audio bytes map to frames (`Layout::positions_for`): data byte `d` lies in block group
   `g = d / (4096 x C)`, channel `c = (d % (4096 x C)) / 4096`, channel byte
   `b = g x 4096 + d % 4096`, frame `b / 4704`. A read of 128 KiB of stereo covers about 14
   frames.
3. The frames come from the decoded-frame cache, or are decoded: a scarletbook range cursor
   gives the positions' frames as stored ([Seeking](#seeking-scarletbooks-28)), and the DST
   frames are decoded in parallel (one `DstDecoder` per worker thread, one frame per job).
4. `Layout::fill` lays the frames out in DSF order, bits reversed, as it copies them.

*Why one DST frame per job:* scarletbook's `DstDecoder::decode_many` decodes two to four
frames together on one thread, but grouping the pool's frames that way (always, or only when
there are enough frames for every thread) made no measurable difference to a straight read
through the mount: a FUSE read needs only a few dozen frames at a time, so longer jobs mostly
delay the reply.

### Seeking: scarletbook's (#28)

A read that does not continue an open file's cursor starts a new one, and since 0.2.0 that
is scarletbook's: every area of a disc keeps a scarletbook seek index (`Disc::enable_seek_index`,
or one loaded from the persistent seek index below), its seek policy is
`SeekPolicy::Speculative`, and a cursor is a `TrackCursor` of the frame range the read needs
(`Disc::range_frames_cursor`), grown for later reads with `Disc::continue_range_frames`.
sacdfs's own frame locator (a search of its own over the area's sectors, aimed with the TOC's
track starts, with an index of (frame, LSN) points learned from every read) was removed.

- **A cold seek is a provisional reading** (specification 4, 4.2 item 8): the library searches
  the frame records' timecodes for a sector whose first frame is numbered below the one wanted
  (interpolation between the TOC's track starts, then regula falsi; at most 5 read calls and
  160 KiB to the range's first frame, scarletbook #123), and reads on from there, the first read
  sized through the range sacdfs asks for. *Why the range is the read's frames:* the library
  sizes its reads to the range's end (up to 1 MiB a call); a range to the area's end made the
  first 128 KiB read of a seek fetch 494 KiB on average instead of 144.
- **Exact where it is cheap:** a range whose exact start (the area's first sector, or a
  checkpoint of the index, which a straight read from the area's start records) is at most 64
  sectors before it is read exactly (`min_gap_sectors`). *Why 64, not the library's 2 048:* with
  2 048, the seeks within 4 MiB of an area's start read every sector from the start, 32 to 67
  calls; at 0 and at 64 the calls and bytes were the same.
- **Suspect areas are read exactly** (`exact_when_suspect`): an area whose TOC places its audio
  past the image's end or over the other area, or a search that saw frame numbers decrease.
  These are only hostile or damaged images; there a seek costs a scan from the area's start.
- **Sequential reads** continue the cursor (a read up to 375 frames ahead of it skips forward,
  as before); each read grows its range to the frames it needs, so the library's next read is
  sized to them.
- **Warm seeks: seek hints.** The index records, from every cursor, sectors with the frame
  number of their first frame record (every 8 frames, up to 2 048 per area), and after a
  remount a provisional reading near a range read before begins at a hint, with one read call
  (scarletbook #124). The hints are what the persistent seek index keeps.
- **Memory:** an index per area of every listed disc, with an inline budget of 64 KiB for
  frames under assembly at its checkpoints (the library's default is 1 MiB), the checkpoints of
  stretches read from the area's start (about 24 bytes per 64 sectors) and the hints (8 bytes
  each). The `parallel` window does not apply: sacdfs still decodes on its own pool.

**Correctness.** A provisional reading's positions are exactly a scan's on every image whose
frames start in increasing frame number order, which is every well-formed disc; on a hostile
image they can differ, and only a pass of the index from the area's first sector can tell
(the reading is then *refuted*). sacdfs's locator had the same limit, unlabelled: it too
started a fresh reading at a sector found by frame numbers. On the image `tests/library_index.rs`
makes for it (a copy of a corpus image with its stereo area's last 32 audio sectors written again
near its start), sacdfs 0.1's locator differed from the exact reading at 2 of the ranges it reads,
and the library's provisional readings differ at 2 and are refuted; sacdfs 0.2 gives the exact
reading at every one, because the library's search sees the frame numbers decrease, marks the
reading suspect, and sacdfs reads suspect readings exactly. So the switch is a correctness gain
as well as one less implementation. On every corpus case, hostile ones included, and on the real
discs, every frame read through sacdfs equals the exact reading (`tests/library_index.rs`,
`tests/corpus.rs`, the DSF gate).

- *Not done: verifying in the background.* sacdfs makes no pass of an area to settle its
  readings (`Disc::verify_speculation`): on a network share that is a read of the whole image,
  the cost seeking exists to avoid. So no verdict arrives, and none is acted on; on a
  well-formed disc there is nothing to act on. Were a refuted reading to be served, the frames
  would sit in the frame cache and the kernel's page cache until evicted. A follow-up could run
  verification while idle on local images and then drop those frames (`notify_inval_inode`).
- *Why not keep the locator:* the owner's offload rule (adopt scarletbook when it is exact and
  within 15 % of sacdfs's own code on the measure that matters, here latency); the measurements
  are below. It removes the hardest code sacdfs had (`disc.rs` went from 1 022 lines to 584, 500 fewer in all).

Measured (details: [performance.md](performance.md#seeking-through-scarletbook-020-28)), (a)
the locator, (b) scarletbook, both on scarletbook 0.5.2:

| | (a) | (b) |
|---|---|---|
| cold seek, calls / KiB per seek, real discs | 4.44 / 137 | 3.06 / 132 |
| warm seek after a remount, calls / KiB | 1.06 / 110 | 1.00 / 84 |
| cold / warm seek in process, ms | 0.64 / 0.83 | 0.65 / 0.86 |
| cold seek through the mount, local, ms (plain DSD / DST, page cache) | 0.49 / 1.38 | 0.54 / 1.36 |
| the same, image evicted | 1.03 / 1.65 | 1.02 / 1.78 |
| warm seek through the mount, local, evicted | 0.65 / 1.31 | 0.73 / 1.45 |
| NFS cold seek, first touch, median ms (plain DSD / DST) | 34.4 / 41.0 | 26.3 / 29.6 |
| NFS cold seek, NAS cache warm (plain DSD / DST) | 2.2 / 3.2 | 2.4 / 2.8 |

- **Decision: switch.** Every seek time is within 15 % of (a)'s (the worst, a warm seek of an
  evicted image through the mount, +12 %), the calls per seek are fewer or equal everywhere, and
  the reads are exact on every image tested, with one the locator got wrong. *Alternative kept
  in mind:* keeping the locator for cold seeks only; not needed, as (b) wins on cold calls.
- *Trade-off accepted:* a cold seek on a plain DSD image reads about 10 % more bytes (one call in
  six more), and a library import's sequential reads make smaller, more numerous calls; neither
  showed in the times.

### Persistent seek index

Each image's seek indexes are kept across mounts in a cache directory
(`$XDG_CACHE_HOME/sacdfs`, `--index-cache DIR`, off with `--no-index-cache`), without their
exact checkpoints (`AreaIndex::without_checkpoints`: the seek hints, saved confirmations and the
TOC fingerprint, a few kilobytes per area), so that a seek after a remount or a reboot near a
range read before starts at a hint: one call to the image instead of three or four. *Why:*
client-side mounts (images on NFS or SMB, #20) pay a round trip for every call, plus a disk seek
on a NAS whose cache does not hold that part of the image. *Why without checkpoints:* they cover
only stretches read from an area's start, take about 24 bytes per 64 sectors (a few hundred
kilobytes per area played through) and a whole library's must fit the directory's bound; a
straight read from the start is exact and cheap without them.

- *Format* (`index_cache.rs`, version 2; version 1, sacdfs 0.1's points, is discarded): one file
  per image, named by the first 128 bits of a SHA-256 of the image's canonical path, so that no
  path appears in the cache; it holds the full hash, the image's size and modification time
  (seconds and nanoseconds), each area's first and end LSN and channel count, and each area's
  saved index as scarletbook writes it (`AreaIndex::to_bytes`, at most 1 MiB). A format version
  and a truncated SHA-256 guard it; a file that does not match in every field, or fails any
  check (a file over 4 MiB, a symbolic link), is ignored and replaced by the next save.
- *Checked by the library:* `AreaIndex::from_bytes` checks every bound before it allocates,
  `Disc::set_seek_index` refuses an index of a disc with another TOC, and a hint whose sector no
  longer holds its frame record is dropped (the cursor then searches). A loaded index the library
  refuses is replaced by an empty one. *Why sacdfs keys by path, size and mtime too:* the library
  cannot tell a re-rip with the same TOC and other audio, and asks callers to key by the file's
  identity. `tests/index_cache.rs` checks that damaged entries and a stale one (audio sectors
  moved under an entry with the same key) cost only speed.
- *Writing.* Lazily: every 30 seconds for discs whose saved form changed, and at unmount. Only
  discs whose cursors handed records to the index since the last flush have their saved form
  built and compared with what the entry holds (*why:* building it copies and encodes the
  index: `seek_latency`, which flushes with hundreds of discs open, measured 0.15 ms more per warm
  seek when every disc's was built); a disc whose form did not change is only touched (its mtime set)
  so that it counts as used. Atomically: a new temporary file (`O_EXCL`, mode 0600, not following
  links), then a rename over the old one. Not synced: a crash can at worst lose or damage an
  entry, which then costs speed. The directory is created on the first write (mode 0700), and
  bounded to 32 MiB in all: after a write, the least recently used files are removed until it
  fits. A cache shared with sacdfs 0.1 is rewritten by whichever ran last.

### Reading the image

scarletbook reads the image through `disc::Source`, a `ReadAt` that takes the image's handle from
the cache of open files ([Open images](#open-images)) for each call and reads with `pread`. The
library's disc, and the clone of its image each cursor reads through, keep one image identity for
the whole mount, which the cursors and the seek index are bound to. *Why not a file held by the
library's disc:* a disc is kept for every listed image, so the files would all stay open; the
cache bounds them and closes idle ones (#26). The disc is opened through the same source, so its
TOC is read through the handle kept for the audio reads (6 to 8 calls, as before).

## Reading a range of a WAV file

1. Header and tag bytes come from the per-file buffers.
2. Audio bytes map to frames: data byte `d` → frame `d / (S x C x 3)`. A read of 128 KiB of
   stereo at 176.4 kHz covers about 10 frames.
3. **Neighbours.** The converter is stateless per frame: the PCM of timeline position `t` is
   computed from DSD positions `t - 1`, `t` and `t + 1` (its filters span less than half a
   frame). At a track's first and last frames the neighbours are the area's positions next to
   it, from the previous or next track or the pause between them, so tracks join exactly as
   they would in one stream of the whole area (gapless). Only at the area's edges is a
   neighbour silence: before position 0, and at or after the *area end*, the later of the
   area's play time and the end of its last track. *Why the later:* the play time is the area's
   play range, but a TOC whose last track runs past it is still shown, and inside a track every
   frame has its real neighbours.
4. **Cache.** Converted frames go into the same byte-bounded LRU as the decoded DSD frames,
   keyed by (disc, area, position, kind), so `--cache-mb` is one budget for both. *Why one
   budget:* a mount that serves only WAV files spends all of it on PCM, and memory stays
   bounded whatever the number of open files. The DSD frames a conversion reads are not
   cached (the PCM frame is what is served again).
5. **Random reads.** A read of k frames that are not cached decodes k + 2 DSD frames (or
   fewer at the area's edges) and converts the k frames in parallel on the decoder pool.
6. **Pipeline.** On a plain DSD area, the request thread demultiplexes frames 16 at a time, and
   each PCM frame is handed to the pool as soon as its next neighbour is
   demultiplexed. (The request thread, not a pool thread, because demultiplexing reads the image:
   see [A stalled source](#a-stalled-source-63-72).) *Why:* there is no DST decoding there, so demultiplexing (serial) and
   conversion (parallel) would otherwise take turns; overlapping them took a straight read of
   a plain DSD stereo area from 3.28 s to 2.90 s. Once the pipeline meets a DST frame, the rest
   of the range is demultiplexed and decoded in one parallel batch first. *Why:* decoding 16
   DST frames at a time left most threads idle and made DST reads 1.8 to 1.9 times DSF's.
7. **Runs of frames** (#24). Frames of the plain DSD pipeline are converted up to four at a
   time, consecutive ones in one job (`Converter::convert_run`, byte-identical to converting
   each alone). *Why:* one job per frame spent much of the conversion's cost on scheduling and
   on each frame's margins; runs of four took a straight WAV read of a plain DSD area 15 %
   less time and 29 % less CPU. *Why only plain DSD:* a DST batch arrives all at once with
   enough frames for every thread, where runs of four made reads 5 % slower. *Why four:* two
   and eight gained less (11 and 12 %), and a short job (about 0.2 ms) keeps the 4 players and
   a tagger under 50 ms per read.
8. **Sequential reads.** Each open file keeps, next to its demultiplexer, the last two DSD
   frames it decoded (*carry*): the previous neighbour and the first frame of the next read.
   A read that continues the file therefore decodes every DSD frame once. Read-ahead decodes
   and converts ahead on the pool and keeps the carry up to date.
9. **Converters.** One `Converter` per pool thread and channel count, made on first use and
   kept in a thread-local, keyed by engine as well so that two engines in one process (tests)
   never share one. Conversion always runs on the pool, never on a request thread.

*Why convert per frame, with neighbours, rather than as a stream:* any byte of any file can be
produced from three DSD frames, with no state carried from earlier reads, so a seek costs the
same as for DSF plus two frames, and a frame's PCM is the same whichever read or file asked for
it. That is what makes the results cacheable and testable byte for byte.

## Read-ahead

When reads on a file are sequential, sacdfs decodes (and for WAV converts) the next window of
frames on the worker pool while the current read is answered, so a player reading in real
time, or `cp`, never waits for the decoder after the first read. The next window is queued
when the read reaches the middle of the current one; it runs on a request thread and holds
the open file's demultiplexer, so a read that needs those frames waits for them instead of
decoding them twice.

- **Window:** 2 s of audio (150 frames). For a WAV reader that has read at least 16 MiB at
  more than 4 times the file's byte rate (`cp`, a scanner), about 8 MiB of PCM, between 150
  and 600 frames (8 s of stereo at 176.4 kHz, 2 s of six channels at 352.8 kHz). *Why:* a fast
  reader of a plain DSD area used up a 2 s window before the next was ready; the longer window
  took the same read from 2.90 s to 2.29 s, within the 1.5x target of #6. *Why only for fast
  readers:* with it for every reader, four real-time players and a tagger saw reads of up to
  114 ms (target 50 ms), because players then convert in long bursts on the shared pool.
  Bounded in bytes, so that six-channel files at 352.8 kHz don't take 50 MB per reader.
- **What counts as sequential:** a read of audio that starts at most 1 MiB after, or 8 MiB
  before, the furthest byte read so far (the kernel sends its read-ahead requests
  concurrently, slightly out of order). Reads of the header or tags never start read-ahead.
- **Not on the first read of audio.** The first read of an open file that covers audio starts
  no read-ahead; the next one does. *Why:* a tagger walking a WAV file's chunks reads the
  header, then the data chunk's header 256 KiB later, which touches the first frame and looks
  like a continuing read; with read-ahead there, every tagged file converted seconds of audio
  (388 MB of image reads and 28 s of CPU for 186 files, against 0.16 s). A player or `cp`
  sends its next read at once.
- **Not within the kernel's own read-ahead** (#32): a read that ends within 256 KiB of where
  the first read of audio started starts none. *Why:* on a first read at the start of a file,
  the kernel asks for 128 KiB, then for 128 KiB more in the background, whatever the reader
  asked for, so a tagger reading a DSF file's first 4 KiB gets two "sequential" reads of
  audio. Counting the second made a DSF tag walk read 1.18 MB of the image per file, against
  222 KB.
- **Not within a scanner's read of the head** (#78): nor does a read that ends within 1 MiB
  of where the first read of audio started (`HEAD_WINDOW`, which holds the 256 KiB above). A
  player's reads go past 1 MiB and start read-ahead there. *Why:* Roon's import reads each
  file's first 768 KiB, 36 KiB at its middle and the tag, then the next file. Read-ahead
  after that head decoded 150 frames (2 s) nobody read and held the decoder threads while
  the scan moved on. With 12 decoder threads, per 6-channel DST file: sacdfs CPU 141–177 →
  42–44 ms, the head's reads 15.5–19.2 → 7.1–7.9 ms, the whole file 17.4–21.6 → 10.3–11.1 ms
  (stereo DST CPU 60–84 → 46–50 ms; plain DSD 2.1–2.5 → 1.2 ms). *Why 1 MiB:* 768 KiB with
  room for scanners that read a little more. *Trade-off:* a player's first 1 MiB (6 to 8
  reads) is decoded on demand rather than ahead, as its first 256 KiB already was; a
  128 KiB read of 6-channel DST decodes about 5 frames, a few ms. *Alternatives:* stopping
  the read-ahead when the file is closed (it is one batch that starts at once, so most of
  it would already be decoded); decoding only part of the head (the head is decoded once,
  and it is the reads' own frames).

**What a DSF header read still costs, and why it stays.** The 256 KiB the kernel reads hold
the track's first frames (about 28 in stereo), and the first page itself holds 4004 bytes of
the first frame, so a DSF tag walk reads audio sectors: 3.7 calls to the image per file on
average, all at the track's start. The options of #32 were measured:

- *Answer the header from a cache of track-start audio:* the kernel never asks for the header
  alone. With `max_readahead` at 4, 16 or 64 KiB or the default 128 KiB, it still asked for
  128 + 128 KiB on a 4 KiB read at offset 0; only 0 changed it (4 KiB requests, 11 times as
  many for the walk), which would also take read-ahead from every player and `cp`. A cache
  would therefore need about 28 decoded frames per track, about 256 KiB: some 3 GB for a
  thousand discs, against the seek index's 32 MiB, and it would not help the first walk of a
  library anyway.
- *Detect header-only reads and serve the audio part lazily:* the request is the same 128 KiB
  whether a tagger asked for 4 KiB or a player for 128 KiB, so there is nothing to detect; and
  a page must be answered whole, since a short reply tells the kernel the file ends there.
  `direct_io` would pass the reader's own size, but turns off the page cache and the kernel's
  read-ahead for every reader.
- *Accept the rest:* chosen. The WAV files hold no audio in their first 256 KiB (the `JUNK`
  chunk), so their header and tags still read nothing.

## Kernel caching

The filesystem is read-only, so entries and attributes are cached for 24 hours, files are
opened with `keep_cache`, and the kernel's page cache serves repeated reads. Reads of up to
1 MiB (`max_read`) and kernel read-ahead are enabled: the kernel read-ahead is asked for
1 MiB (the kernel grants at most its own default, typically 128 KiB, as fuser's `INIT` log
shows), and up to 64 background requests are allowed. The filesystem is mounted `ro`,
`nodev`, `nosuid`, `noexec`.

Art in an area is the exception to the long cache (#235). A listing is made again at most once per
refresh interval, so for up to that long it can name a file that was replaced or removed, and
`cover.jpg` (an alias of whichever image is picked, #148) can stand for a file that has just gone.
A lookup of such a name therefore compares the source file with what was listed (one `fstatat`); if
it changed, or an open finds it changed or gone (a file gone since the kernel was told of it is
`ESTALE`, so the kernel looks the name up again), a counter moves and every area reads its art again
at its next look, whatever its directory's time. Replies for these names carry a zero timeout, so the
kernel asks again each time and never keeps a number whose file is gone. *Why not keep the old
inode for `cover.jpg`:* the alias is the same node as the image's other name (the same inode, #148),
and a second, stable inode would break that. *Why not serve the old bytes:* once a file is replaced
there is nothing to read them from, and the sizes the kernel holds would clip the new bytes.
*Cost:* one extra `LOOKUP` and `fstatat` per access to an art file in an area.

Two kinds of probe are answered so that the kernel stops asking (#61):

- **Missing names** get a negative entry (inode 0, cached for the same 24 hours) instead of
  `ENOENT`. Players, file managers and SMB clients look for `cover.jpg`, `folder.jpg`,
  `desktop.ini` and the like in every folder, again and again; the kernel now answers the
  repeats itself. *Why it is safe:* a directory's listing is made once per mount and never
  changes, so a name missing from it stays missing (except in a folder that is re-read, #101, whose entries are remembered for the refresh interval only). Lookups of `.` and `..` (the NFS export
  code's) still fail with `ENOENT` when the inode is unknown, so a handle to something gone
  still gets `ESTALE`. Measured with fuser's request log on a 24-track area: three `stat`s of
  each of four missing names sent 3 `LOOKUP`s each before, 1 after.
- **Extended attributes (#61, #174):** `getxattr` answers `ENOSYS` and `listxattr` an empty
  list, at once, without touching the tree. The kernel remembers `ENOSYS` for the whole mount
  and answers every later `getxattr` itself, with `EOPNOTSUPP`: one request in all. An empty
  list is an answer about one file, so each `listxattr` reaches sacdfs: one per entry of an
  `ls -l`, about 4 µs each (10 runs of `ls -lR` over the corpus's 1245 entries: 27 ms against
  22 ms, debug build).
  - *Why not `ENOSYS` for `listxattr` too, as from 0.1.1 to 0.4.0-rc.3:* NFS-Ganesha forwards
    an NFSv4.2 client's `LISTXATTRS` to the mount, turns `EOPNOTSUPP` into `NFS4ERR_NOTSUPP`,
    and the Linux NFS client returns that as its kernel-internal `ENOTSUPP` (524), which `ls`
    printed as "Unknown error 524" for every entry of `ls -l` and `getfattr -d` (#174). With
    an empty list, Ganesha answers `LISTXATTRS` with an empty list. `security.selinux` is not
    involved: the client's own security module answers it, locally and over NFS.
  - *Why not `ENODATA` for `getxattr`, as in 0.4.0-rc.4:* like the empty list, it is an
    answer about one file, and Samba asks on its open and ACL paths for every file. A Roon
    force rescan through the container's Samba, counted with fuser's request log, sent
    21,767 `GETXATTR` on rc.4 (19,483 for `security.capability`, about 3.4 per file, and
    1,142 each for `system.posix_acl_access` and `system.posix_acl_default`), 35,892
    requests in all, against 0 `GETXATTR` and 14,970 in all on rc.3; an hour with no scan
    still sent 1,314. `LISTXATTR` stayed at 0 on both. On the corpus, `getfattr -n` on every
    file sends one `GETXATTR` per call with `ENODATA` (914 for `security.capability`, 1245
    for `user.x` each pass) and 1 in all with `ENOSYS`. A local `smbclient` listing and read
    did not show this: it does not take those paths.
  - *Why not configure Ganesha instead:* Ganesha 9.16 (the image's) has no setting that stops
    it advertising or forwarding xattrs for an FSAL_VFS export (its export options cover ACLs
    and security labels only), and dropping NFSv4.2 would break the documented `vers=4.2`
    mount.
  - *Known limit:* a named lookup over NFS (`getfattr -n user.x`) still gets
    `NFS4ERR_NOTSUPP` and prints "Unknown error 524"; locally it says "Operation not
    supported". `ls -l`, `getfattr -d` and file reads over NFS print no error.
  - *Measured* with fuser's request log on the corpus (914 files, 331 folders): a `stat` and
    `open` of every file, twice, sends no xattr request; `ls -lR` and `getfattr -R -d` send
    1245 `LISTXATTR` each and no `GETXATTR`. Tests: `tests/probes.rs` checks at most one
    `GETXATTR` for a mount (two `ls -l` and `getfattr -n` of two names on every file, twice);
    the CI share test runs `ls -lR` over SMB and NFS and `getfattr -R -d` over NFS, and fails
    on any error line.

**Filesystem size (#62, #217).** `statfs` reports the total size of the image files found at mount,
all of it used (0 free, 0 available), the number of entries made so far as inodes, and 0 free
inodes. With no images (or none readable) it reports the size of the filesystem holding SOURCE,
read again at most once a minute on a request thread, so a stalled source never holds up a `statfs`
reply. *Why not 0:* `df` showed a 0-byte filesystem before #62, and SMB clients were told 0
bytes total and free, which some treat as a broken share. *Why the images' total (#217):* it is
what the share holds. It used to be the source filesystem's capacity, which said 45.9 TB against 3.12
TB of listed files, and looked like a huge full disk. It takes one `stat` per image at mount, just
after the scan has done the same. *Why not the sum of the files' sizes:* the tree is made lazily, so
it is known only for the folders listed so far; it would grow while a client browses, and opening every
image at mount to know it costs a TOC read per image. *Why 0 free, not the source's real free
space:* the same as ISO 9660 and squashfs report; nothing can be written, and a non-zero value would
say otherwise. Roon and Plex have run with 0 free since #62, and whether they care about a
non-zero value hasn't been tested, so this keeps what has been run. Measured with the real free space:
`df` showed 58 % used and clients a normal disk, and writes were refused all the same.
*Trade-off:* images added later count from the next mount, because counting them would mean walking the
source each time; and 100 % used remains, though it's the convention for read-only media. `statvfs`
comes from `nix` (MIT, already a dependency of fuser, with the same `fs` feature), not `libc`: sacdfs
keeps `unsafe` to its daemonising code.

**Read-only attribute (#217).** The container's Samba has `map readonly = yes`, so each file and folder
carries the DOS read-only attribute, worked out from its mode with no extra request to sacdfs. *Why:*
the share is read-only and gvfs's `can-write` comes only from this attribute. The volume was already flagged
`FILE_READ_ONLY_VOLUME` (from the read-only FUSE mount), which gvfs can't see, as libsmbclient doesn't pass it on.
*Alternative:* `map readonly = permissions` asks the same of each open file, which costs more for the same
result on a mount with no write bits. *Trade-off:* folders get it too, as Samba can't separate them; a
plain `cp` on the Linux client keeps the missing write bit; and Windows and macOS haven't been tested.

## Concurrency

fuser reads requests from the kernel on 4 threads, and they only hand requests over: what
is in memory already (the attributes of a node made already, `opendir` and `access` of one)
is answered there, and everything that may read the source moves, with its reply, to a pool
of request threads. Reads go to one pool (`sacdfs-read`, as many threads as decoder threads,
at least 4 and at most 16, growing when busy), lookups, listings, opens and the root's
attributes to another (`sacdfs-meta`, 4, growing too). A worker decodes, copies or loads, and
replies, so a slow read or listing never blocks the others. DST frames are decoded, and frames
converted to PCM, on a separate rayon pool (`--threads`), with `DstDecoder`s and
`Converter`s (one per channel count) taken from pools shared by its threads (#150).

- *Why the request pools are not rayon pools:* a rayon worker that waits for another pool
  runs other jobs of its own pool meanwhile, and a second read of the same file would then
  wait, on the same thread, for the demultiplexer lock the first one holds.
- Every read runs under `catch_unwind`, so even an unexpected panic becomes an I/O error on
  that read.

### A stalled source (#63, #72)

A source can stop answering without failing: a hung NFS or SMB share, a disk retrying a bad
sector, a disk that spins up. A thread that reads it then blocks in the kernel, for as long
as the source takes, and nothing in sacdfs can stop it. Two things keep that from hanging
everything:

- **Nothing that reads the source runs on fuser's threads** (#63). *Why:* `lookup` and
  `readdir` used to load TOCs there, so four listings of a hung share stopped every other
  request of the mount, even of directories listed long before. *Trade-off:* a lookup or
  listing now costs a handover to another thread; the kernel keeps entries and attributes
  for a day (for the refresh interval on a re-read tree), so few requests pay it, and `getattr` of a
  node made already (the most frequent request, from `ls -l` and every scanner) is still
  answered in place. Measured on the bench images (186 files, a desktop test machine, under the bench
  lock, two runs each): a cold walk of the whole mount, every lookup, listing and TOC load,
  took 11.4 and 12.6 ms before, 14.0 and 12.2 ms after; a warm `stat` 0.91 and 1.29 µs
  before, 1.28 and 1.29 µs after. Both within the noise.
- **The request pools grow** when a request is queued and no thread is free, to at most 64
  threads each; a spare thread with nothing to do for 10 s ends. *Why:* a thread blocked on
  a stalled source is lost until the source answers. A fixed pool lost its threads one by
  one, and then every request, of any image, queued behind them. *Why 64:* the kernel sends
  at most 64 background requests at once (`max_background`), plus the few synchronous ones
  of its callers; the bound keeps a hung share from making threads without end. A thread is
  a stack of address space, mostly untouched, and costs nothing while parked. *The limit:*
  this delays the starvation of healthy images in a long hang, and doesn't prevent it. See
  the last point of this list.
- **No decoder thread reads the source.** The decoder pool is fixed and shared by every image, so
  a thread lost to a stalled source is lost to all of them. DST decoding and PCM conversion run
  there; audio reads of the image (the demultiplexing of both formats) run on the request
  thread, which the pool above can spare until it reaches its limit, and TOC loads run on
  `sacdfs-meta` and `sacdfs-toc`. *Why this matters more than the thread:* a
  pool thread waiting for its own decoding runs other queued jobs meanwhile (work stealing), so
  while WAV reads demultiplexed on the pool (until 0.4.1-rc.2), one could take up a read of the
  stalled image on top of a healthy read, which then could not return until the source
  answered, whatever the number of threads. A soak found it: 1 read of 320,656 on a healthy disc
  failed with `EIO`, in the same second as a stall. *Trade-off:* a frame to convert reaches
  the pool through its shared queue, not a worker's own (see [performance](performance.md#history)).
- **The watchdog fails what waits too long** (`--request-timeout`, 30 s by default, 0 for
  none; `src/watchdog.rs`). Each request handed over has its reply in a slot that the
  request thread takes when done, and that one watchdog thread, checking a few times per
  timeout, takes first if the timeout has passed: it logs the request (the image, track and
  offset of a read; the inode of a listing), whether it was queued or running and on which
  thread, and replies `EIO`. The request thread, when it finishes, finds the slot empty and
  only logs. *Why fail rather than wait:* fuser has no `FUSE_INTERRUPT` (fuser#14), so a
  request sacdfs is stuck on stays open for as long as the source hangs, and an SMB server
  re-exporting the mount keeps its client waiting with interim answers: #72 waited 6 h. An
  error lets the client retry or give up, and sacdfs logs what was stuck, which #72 lacked.
  libfuse 3.18 ("request timeouts"), JuiceFS 1.3 and macFUSE 5.5 (`daemon_timeout`) added
  the same. *Why `EIO`, not `EAGAIN`:* `EIO` is what a failing disk gives, and every client
  handles it; some programs retry a blocking read's `EAGAIN` at once, which would spin on a
  stalled source. *Why 30 s:* far above the slowest healthy request (a cold DST read, or the
  first listing of a directory of images on a share, takes milliseconds to a few seconds),
  and below the 60 s after which SMB clients give up on a server. *Trade-off:* a disk that
  takes longer than the timeout to spin up fails its first requests; raise the timeout for
  such disks. A client can see a few times the timeout before its error, because the kernel
  asks again for the page its failed read-ahead held, and programs retry a short read.
- A listing that timed out goes on loading on its thread; when the source answers, the
  directory is loaded, and the next listing is immediate.
- The root's `getattr` starts a sweep of the source for changes (#123). It does not wait for
  a sweep already running (held up by a stalled source, say): it answers with the times
  known, and the running sweep moves them when it is done.
- **What a long hang can still do ([#241](https://github.com/aclater/sacdfs/issues/241)).** A
  stalled image does not hold up other images for good only while the read pool has threads
  left. The watchdog replies `EIO` but can't free the thread, which stays blocked until the
  source answers, so each retry by a client takes one more. They needn't be simultaneous. At 64
  threads, a read of any image waits for a thread and the watchdog fails it. Measured on 2
  decoder threads, with clients looping `read()` on the stalled image: 8 WAV clients at the
  default 30 s timeout reached 64 threads in 362 s, and a healthy DSF read then got no answer
  in 10 s (2 of 2); 8 clients at a 2 s timeout took 26 s, in either format; 1 WAV client at 2 s
  took 142 s. Healthy reads recovered at once when the source came back, and spare threads
  ended at about one per 10 s. DSF reads have always had this limit; demultiplexing on the
  request thread (#239) did not add it.

**Not done, and why.** *A timeout on the source I/O itself:* a thread blocked in the kernel
cannot be stopped from user space; only the request can be answered. *Moving the TOC loads
off rayon's global pool:* a listing opens its images in parallel there (`sacdfs-toc`, at most
8 threads), so 8 images stalled at once leave later listings that open images queued, and
the watchdog fails them; listings of directories made already, and reads, are not affected.
A follow-up can give TOC loads a growing pool like the request pools.

**Audit of every wait on the read path (#72).** Looking for a wait that a missed wakeup or a
lost reply could make endless:

| Wait | Bounded by | Finding |
|---|---|---|
| A request job queued for a request thread (`mpsc` receive) | a free thread | the pool was fixed: threads blocked on the source starved every later request. Now it grows, and the watchdog bounds the wait |
| A read waiting for its file's demultiplexer (`Reader::stream`), held by an earlier read or the file's read-ahead | that holder's decode, or its source I/O | bounded unless the holder blocks on the source; the waiter is now failed by the watchdog |
| A read waiting for the DST pool (`ThreadPool::install` / `scope`) | the decode jobs, which always complete (a panic is carried back to the caller) | no lost wakeup: rayon sets the latch when the jobs end. This audit noted that the PCM path demultiplexed inside the pool, holding a decoder thread on a stalled source, and missed that a thread waiting in the pool can take up such a job on top of another read; the PCM path now demultiplexes on the request thread, so these jobs never wait for the source |
| The frame cache, its slab pool, the open-image cache (`handles`) | short critical sections | none of them is held across source I/O or another wait (the image `stat` and `open` run outside the lock) |
| Read-ahead | nothing waits for it, except through `Reader::stream` above | |
| The kernel's own read of a page that a failed or abandoned read-ahead held | the request for it, which the watchdog bounds | |

No missed wakeup or lost reply was found: every endless wait was a wait for the source, or for
a lock or thread held by something waiting for the source. What held #72's read is still not
known (its images were on local disks); the watchdog bounds it and its log line says what it
was waiting on, for the next occurrence.

### Open images

The handle an image's TOC was read through is kept (`handles.rs`, #26), and every audio read
of the disc uses it; a closed one is opened again on the next read, and refused if the
image's size changed since the TOC was read. The handles live in one least-recently-used
cache for the process, bounded by `--max-open-images` (default 64); a handle unused for 5
minutes is closed by a thread that looks every 30 seconds. A handle is checked against its
path (one `stat`) when it is used, at most once a second: if the file's size, modification
time, device or inode changed, the handle is closed and the image opened again, as a first
open would. A disc that is dropped closes its handle.

- *Why keep handles:* on a network share an open costs a round trip or two (0.6 to 0.9 ms
  over NFS), as much as the one read a seek with a warm seek index needs. The TOC handle is
  kept because a seek right after a directory listing (a player resuming) is the common case.
- *Why a bound and an idle timeout:* a library has hundreds or thousands of images, and a
  listing of a directory opens all of its images; the process's file limit is commonly
  1024. 64 covers several players and a tagger on different discs with room to spare. An
  idle mount should not keep files open on a share: a handle pins a deleted or replaced
  file on the server and keeps NFSv4 open state alive. Five minutes outlasts a pause in
  playback.
- *Why the check once a second, not on every read:* a `stat` of the path is a round trip
  on a share whose attribute cache has expired. Once a second costs nothing measurable on a
  straight read and bounds how long a replaced file is read through its old handle. The
  tree, the frame cache and the seek index still describe the image as it was mounted (the
  seek index checks every point it uses, so it costs only speed); a changed image needs a
  remount.
- *Why the cache drops handles instead of closing them:* a handle is shared (`Arc`), so a
  read in progress on an evicted handle finishes on it, and the file is closed when that
  read ends. No lock is held while opening a file, and the cache's lock never waits for a
  read, so eviction cannot deadlock or stall a read. The keys are per disc and unique in
  the process (not the disc id, which callers choose).

### Concurrent reads of one image

A handle is an `Arc<File>` read with positional reads (`pread`), through scarletbook's
`ReadAtReader` over `disc::Source` (#28, #46). Each cursor reads through a clone of the library
disc's `Image` (no I/O, no lock: a position of its own), and each call takes the cached handle,
so reads of one disc by several players, a tagger and read-ahead run at once.

- *Why not a lock per image:* on a local disk a read would hold it only for a copy from the
  page cache, but on a network share each read is a round trip, and on a NAS with hard disks
  often a disk seek: a tagger's or a second player's read would wait behind the first
  player's for the whole round trip. Measured: with a lock, a tagger's worst file over NFS
  took 38 to 163 ms, without it 7 to 13 ms
  ([performance.md](performance.md#concurrency-and-memory)).
- *Why the handle per call, not one per cursor or thread:* cursors live as long as their open
  file; per call, the bound on open images (`--max-open-images`) still holds between reads, and
  an evicted handle closes when the last read through it ends. The image clones a cursor holds
  keep no file open.
- Images in 2064-byte raw sectors are read through a buffer per clone (scarletbook's), so
  each call allocates it again; the straight-read figures show no cost.
- *Decision (#28, 2026-11-02): this is the whole of #28's step 1; no disc per worker thread and
  no further change.* Measured with 1, 4 and 8 readers at full speed, readers of one DST image
  reach the same aggregate as readers of different images, and the CPU per MB does not grow
  with the readers ([performance.md](performance.md#concurrency-and-memory)): the limit is the
  decoder's CPU, not the read path. *Why not hold a `Disc` clone per worker:* it would only
  save the clone of an `Image` per cursor (no I/O) and the handle lookup per call, neither of
  which shows in the figures, and would pin a file per worker past the `--max-open-images`
  bound. *Alternatives considered:* a lock-free handle fast path per source (no measured
  contention on the cache's lock, so more code for nothing); the library's `parallel`
  cursor window (more memory per open file, see the PCM and memory notes of #28).

## PCM conversion

The DSD-to-PCM conversion behind the WAV files is scarletbook-rs's `scarletbook-pcm` (since
#84), through `sacdfs::pcm`, which maps sacdfs's options to it and feeds it the engine's
frames. Its measured quality is in [pcm-quality.md](pcm-quality.md), the speed in
[performance.md](performance.md#converter-throughput). Each decision below says why, the
alternatives and the trade-off.

**Output.** sacdfs offers these outputs (owner decisions, with their reasons, in #11 and #5):
- 24-bit PCM at 176.4 kHz (DSD64 ÷ 16, the default), 88.2 kHz (÷ 32) or 352.8 kHz (÷ 8);
- a pass band to 40 kHz (the default, fully attenuated by 48 kHz), to 30 kHz (attenuated by 36 kHz), or a wide 80 kHz (176.4 and 352.8 kHz only, attenuated by the Nyquist frequency at 176.4 kHz);
- a gain, 0 dB by default, where 100 % DSD modulation is full scale.

**Why scarletbook-pcm (#84).** The owner's decision (2026-11-01) was to keep scarletbook-rs's clean converter, measure it against sacdfs's own, and switch when it is within the 15 % offload leeway and meets the targets above.
- **0.5.2 did not:**
  - it was 33 to 46 % slower with the default filter;
  - its wide filter folded 88.2-96 kHz back below Nyquist at 176.4 kHz;
  - its 30 kHz filter stopped from 38 kHz;
  - DSD silence did not convert to exact zero.
- **0.5.3 fixed all four** (scarletbook-rs#170 to #173), so sacdfs's own converter, about 1 600 lines, was deleted.
- *Alternative:* keep sacdfs's converter (ported from pysacd's), which has a flatter pass band (±0.00003 against ±0.004 dB) and a 4 dB lower noise floor. Neither difference is audible, and the code would be maintained twice, which the owner wants to avoid.
- *Trade-off:* the WAV files' samples changed (by less than -72 dB against the signal in the audio band), so their output version moved to 3 ([identity](#file-identity-and-nfs-export)): new mtimes and generations, so copies and library scanners pick up the new bytes.

**Stateless per frame (why: random reads).** Frame `f`'s PCM is computed from DSD frames `f - 1`, `f` and `f + 1` only, and equals, bit for bit, a conversion of the whole area in one pass. `pcm::convert_area` and scarletbook-pcm's own tests check this for every frame, 1 to 6 channels, every rate and cutoff, and by proptest on random windows and runs. Each output sample depends on at most 195 bytes per channel of each neighbour, far less than a frame.
- *Alternative:* a converter that carries filter state from frame to frame would need every earlier frame converted first, so a seek would cost a conversion from the start of the area.

**Aligned (why: DSF and WAV of a track line up).** PCM sample `n` is centred on DSD sample `d × n` of the same frame. The measured offset is below 0.0001 samples.

**Silence (why 0x69).** Before the first and after the last frame of an area, the converter reads DSD silence, the byte 0x69: a neighbour of `None` is the silence pattern. 0x69 converts to exact zero, so a track that starts at the area start fades in from true digital zero, and a silent channel is all zeros.
- *Alternatives:* zero bytes are -100 % modulation, a full-scale negative step; repeating the area's first or last frame would invent audio.

**Frames in place (why: the frame cache).** The engine keeps each decoded DSD frame in its own buffer and converts short runs of them: four at a time from plain DSD areas, one at a time from DST batches ([WAV reads](#reading-a-range-of-a-wav-file)). `Converter::convert_run` hands scarletbook-pcm the run as separate frames (`Frames::Separate`), and gets back:
- 24-bit little-endian bytes, written directly into the frame-cache entries (`PcmOut::S24le`);
- each frame's clip count, which is cached with the frame, so a later read reports the clips of exactly the frames it returns.

*Alternative:* copying the frames together and packing `i32` samples afterwards, which was 0.5.2's only API. That cost a copy and an extra pass per run (scarletbook-rs#172).

**Gain and clipping.** `--pcm-gain` (-60 to +24 dB) maps to scarletbook-pcm's `Gain` (-120 to +24 dB, the same factor on every platform). Samples beyond the 24-bit range are clamped and counted. 100 % positive modulation is exactly one step above the largest code, so a full-scale positive DC counts as clipping. Real discs stay far below this (SACD's reference level is 50 % modulation): no clips on the local discs.

**Deterministic everywhere.** scarletbook-pcm uses integer arithmetic and chooses its code at run time (AVX2 and FMA where the CPU has them), with the same bytes on every path and CPU. sacdfs pins the output's hash per configuration (`tests/pcm.rs`), so a change to the filters shows up there and needs a `WAV_VERSION` bump.

**API.** `pcm::Converter::new(channels, &PcmConfig)` is made once per worker thread and allocates nothing per frame after the first.
- `convert(prev, cur, next, out)` writes frame `cur`'s interleaved 24-bit little-endian PCM (`frame_bytes()` long) and returns the clip count. `None` neighbours are silence.
- `convert_run(prev, frames, next, out, clips)` converts consecutive frames: the same bytes as `convert` on each, plus each frame's clip count.
- `convert_f32` gives the same samples as floats, for tools and tests.
- `new` never fails. A configuration that `PcmConfig::validate` refuses (the wide filter at 88.2 kHz) is converted with `PcmConfig::effective` instead, 40 kHz, which `config()` reports. *Why:* a panic in a FUSE worker is an I/O error at best; options are checked once, with `validate`, when the mount starts.
- A short frame (which the decoder never makes) is padded with silence.

## Client-side start-up over NFS from hard disks (#71)

**Decision:** no code change; tuning guidance in the deployment guide
([client-side](deployment.md#client-side-images-on-hard-disks-over-nfs)), and a proposal to
state the concurrency target of #6 as *no read over 50 ms while playing*, with start-up
judged by the cold-seek target.

- **Why:** the measurement on an emulated hard disk
  ([performance](performance.md#client-side-where-the-slow-reads-come-from-71)) found every
  read over 20 ms in a player's start-up, when it fills its buffer after a cold seek; while
  playing, no read took over 0.5 ms. The slow start-up reads are the disk seeks of four cold
  starts at once, queued on one disk. A player is not yet playing then, so they delay its
  start but cannot cause a dropout.
- **Alternatives measured:** a larger NFS client read-ahead and other `rsize` values (worse),
  `nconnect` (no change: one disk), `--threads 4` (lower worst read, slower start-ups),
  `--cache-mb 1024` (no change). *Not tried:* read-ahead in a reader's first 1 MiB, which #78
  removed for library scanners; at start-up it would add reads to the same disk queue, which
  is what costs.
- **Trade-off:** four players started at the same instant from one hard disk still wait up to
  0.31 s for a full buffer, against the 50 ms cold-seek target. Without any disk delay, start-up
  reads still take 40 to 57 ms when four start together: the start-ups' round trips and
  decoding. Shortening that would mean fewer dependent reads of the image per cold seek,
  which is scarletbook's seeking (#28).
- **Owner direction:** performance and latency are key requirements.

## Performance basis

scarletbook decodes a stereo DST frame in about 0.17 ms on one core (about 78x real time per
core), and plain DSD areas need no decoding. The converter (scarletbook-pcm) runs at about
300x real time per core for the default 176.4 kHz stereo output, so on a DST area conversion adds about a
quarter of the decoding cost, and on a plain DSD area it is the only CPU cost. A straight
read through the mount is therefore limited by decoding on DST areas and by copying through
FUSE on plain DSD ones, and a seek by the number of reads of the image it makes. Measured
figures are in [performance.md](performance.md).

## Deployment on a NAS (the container)

The container image (`Containerfile`, [deployment.md](deployment.md)) runs sacdfs and the
servers that share its mount, Samba (SMB) and NFS-Ganesha (NFSv4), in one container. The
decisions, each with its reason:

- **Server-side is the primary deployment** (#17): sacdfs's reads of the images stay on the
  NAS, so a seek costs sacdfs's few milliseconds there plus about one network round trip,
  where a client-side mount pays a round trip for every read of the image. Any client
  system works, over SMB or NFS.
- **The servers run inside the container.** An appliance NAS shares only its own datasets,
  and cannot see a FUSE mount inside an app container (no mount propagation to the host on
  TrueNAS). Serving from inside depends on nothing in the host but its container runtime.
- **Build on UBI 10, run on Fedora.** Red Hat first: sacdfs is compiled with RHEL 10's Rust
  from UBI's AppStream. UBI's repositories have no Samba and no NFS server, RHEL's full
  repositories need a subscription (so the image couldn't be published or built in CI), and
  NFS-Ganesha isn't in RHEL at all. Fedora has all three. CentOS was not needed.
- **Generic x86-64 by default.** RHEL's Rust targets x86-64-v3 unless told otherwise, and many
  NAS CPUs (Atom, Celeron, Pentium Silver) have no AVX2, so the Containerfile sets
  `-C target-cpu=x86-64`; a build argument makes a v3 image.
- **NFS-Ganesha rather than the kernel's NFS server.** The kernel server needs its module
  loaded on the host (a container can't load one), a rootful and in practice privileged
  container, and shares kernel state with the host's own NFS service. NFS-Ganesha needs only
  `CAP_DAC_READ_SEARCH`, for `open_by_handle_at(2)`. sacdfs's decoding, not the server, sets
  the speed. Both servers rely on sacdfs's persistent file identity (#18) for file handles
  that survive a restart; CI checks this with NFS-Ganesha.
- **Privileges:** `/dev/fuse` and `CAP_SYS_ADMIN` for FUSE, `CAP_DAC_READ_SEARCH` for NFS
  only (`NFS=0` drops the need), and on AppArmor hosts `apparmor=unconfined`, since Docker's
  default profile forbids `mount(2)`. No `--privileged`.
- **NFSv4 only** (4.0 for macOS, 4.1 and 4.2 for Linux), TCP, port 2049: no rpcbind, no
  mountd, no lock manager, so one port to publish. **Graceless:** a read-only export has no
  locks or opens worth recovering, so a restarted server answers at once instead of
  waiting out a 90 s grace period.
- **`all_squash` to `nobody`, and `--allow-other`:** the mount is read-only and world-
  readable, so who a client claims to be doesn't matter; smbd and Ganesha read the mount
  as other users than the one that mounted it, which FUSE allows only with `allow_other`.
- **Windows-forbidden characters in titles** (`" * : < > ? \ |`) are sent over SMB as the
  private-use characters that macOS and the Linux kernel client map back (Samba's `catia`
  module, the SFM mapping), not as mangled 8.3 names. A full-width mapping (`：`) reads better
  on Windows, but breaks titles that already contain a full-width character: the corpus has
  one, and real Japanese titles often do. It is an option, not the default.
- **Samba does no extended-attribute or DOS-attribute lookups** (`ea support = no`,
  `store dos attributes = no`): sacdfs has neither, and each lookup would be one more FUSE
  round trip per file.
- **Servers stop before sacdfs, and start after it,** with the same NFS filesystem id
  every time. A server must not serve an unmounting or empty directory, and NFS-Ganesha
  holds the export root open until it exits, so stopping sacdfs first fails as busy. The
  order follows the restart sequence for the kernel server (stop NFS, then sacdfs; mount,
  then export with a fixed `fsid`), applied to NFS-Ganesha.
- **If one process stops, the container stops**, so the runtime's restart policy restarts
  all three together instead of leaving a server sharing an empty directory.

## Release binaries and image (#44)

`.github/workflows/release.yml` builds, tests and publishes a release on a `v*` tag, and does
everything but publish when run by hand or on a pull request that changes it (a dry run).
The checklist is in CONTRIBUTING.md, "Releasing". The decisions, each with its reason:

- **glibc 2.28, not static musl.** A static musl binary was the first choice (it runs on
  any Linux), and sacdfs builds so: fuser is used without libfuse, and no dependency
  compiles C. It passed the DSF gate (93/93 on the real discs), but measured (3 fresh-mount
  straight reads of a stereo area each, same compiler and flags, generic x86-64) it was much
  slower:

  | Straight read, stereo area, s | glibc | musl | |
  |---|---|---|---|
  | DST disc, DSF files | 5.47 | 7.79 | 1.42x (median 1.34x) |
  | DST disc, WAV files | 5.26 | 8.20 | 1.56x |
  | plain DSD disc, WAV files | 2.47 | 3.73 | 1.51x |

  A second round of 2 runs each, with the UBI 8 build added: WAV files of the DST disc 4.65 s
  (glibc build on UBI 10), 4.66 s (UBI 8) and 7.24 s (musl, 1.56x); of the plain DSD disc
  2.01, 2.01 and 3.31 s (1.65x). Building against the older glibc costs nothing: the binary
  runs on the system's glibc. Most likely musl's allocator and memory copies, under many
  decoder threads; not investigated further. Performance comes first for sacdfs, so the
  release builds against glibc instead, on **UBI 8** (RHEL 8's glibc 2.28) so that one binary
  covers RHEL 8 and later, Debian 10, Ubuntu 20.04 and Fedora 29 and later. CI fails the build
  if the binary needs a newer glibc symbol. Older bases (glibc 2.17, CentOS 7) are out of
  support and not Red Hat's current images. *Possible later:* musl with a faster allocator
  (mimalloc), measured against this.
- **Two builds: generic x86-64 and x86-64-v3,** `-C target-cpu` always explicit, since RHEL's
  Rust defaults to x86-64-v3. Generic runs everywhere and already converts with AVX2 where
  the CPU has it (#23); v3 compiles the rest of sacdfs and scarletbook for AVX2 as well. The
  v3 build is tested on the runner only if its CPU is x86-64-v3, and the release stops
  otherwise.
- **Rust from rustup** (the version the other workflows use), not RHEL's: RHEL 8's Rust is
  older than sacdfs's MSRV, and the tested toolchain should be the shipped one.
- **Each build is tested as built:** the whole suite, FUSE tests required, in the UBI 8
  container with the release profile and the build's flags; then the packaged binary,
  unpacked on the runner, mounts the corpus and every track is checked against the corpus
  manifest (DSF hashes, WAV headers).
- **Stripped** (`CARGO_PROFILE_RELEASE_STRIP=symbols`, so Cargo.toml's release profile, which
  the measurements use, is unchanged), **build paths remapped**, and the tarballs
  **reproducible** (sorted, fixed owner, the commit's time, `gzip -n`).
- **Integrity:** a `.sha256` per archive and `SHA256SUMS`; a GitHub build provenance
  attestation (Sigstore) for both archives and for the image, the archives' bundle also on
  the release as `.intoto.jsonl` for offline checks. The tag must equal `v` plus
  Cargo.toml's version (and Cargo.lock's), and the notes are the tag's CHANGELOG.md section
  (`.github/scripts/changelog-notes.py`); either missing stops the release before any build.
- **The image** is the Containerfile built for generic x86-64. The share test
  (`deploy/ci-share-test.sh`, the one `container.yml` runs on every pull request) runs on
  that image, which is then saved and loaded by the job that pushes it, so the pushed image
  is the tested one. That job alone has `packages: write`. `:latest` follows releases
  without a hyphen.
- **Permissions:** `contents: read` for the workflow; `contents: write` (the release) and
  `id-token`/`attestations: write` only in the publishing jobs (the release, the image,
  and `id-token` alone in crates.io's), which run only when
  `github.ref_type == 'tag'`. No caches (nothing a pull request writes can reach a release
  build), no persisted credentials, actions pinned by SHA.
- **crates.io (#155):** final versions only (a crates.io version can never be deleted or
  reused, only yanked; release candidates are tested from the GitHub release), after the
  GitHub release, through trusted publishing: the `crates-publish` job has `contents: read`
  and `id-token: write`, and no crates.io token is stored in the repository. A version already on crates.io is skipped, because trusted publishing
  cannot create a crate and the first version (0.4.0) is published by hand. The package
  holds only `src/`, Cargo.toml, Cargo.lock, README.md, COPYING and CHANGELOG.md (`include`).

## Tag layer (#81)

User tags are kept in a tags directory and merged into the TOC's tags when a track file is
made (`src/tags.rs`, `dsf::tagged`); `sacdfs tags import` fills it from extracts
(`src/tags_cli.rs`). *Why it matters:* storage reclamation (#82). A library check found that
deleting extracts would lose user genres (TCON) on 827 of 1,066 files, embedded art on 33 and
MusicBrainz IDs on 26.

- **A tags directory, one TOML file per disc, named by the disc's TOC identity.** *Owner
  decision (#81, 2026-11-01):* a tags directory with one file per disc, keyed by the disc's TOC
  identity; the images' dataset stays read-only. *Why:* edits survive renames and moves, and
  two copies of a disc share them; the images stay read-only; sacdfs does not depend on beets.
  *Alternatives:* a file next to each ISO (needs a writable image dataset), the beets
  database (a dependency). *Trade-off:* a user must look the key up (`sacdfs tags key`).
- **The key** is 128 bits of a SHA-256 of the master and area TOCs' numbers: catalogue
  numbers as stored, set size and position, date, disc flags, and each area's slot, channels,
  TOC and audio sectors and each track's sectors and frames. *Why not the path-based inode
  key:* that changes on a rename. *Why numbers and not the raw TOC sectors:* the library hands
  over the parsed TOC; these fields sit at fixed offsets of the specification, so a newer
  scarletbook gives the same key, and any copy of the same pressing does too. Text is left out
  so that a TOC text decoding fix can't move a key. *Trade-off:* two pressings with identical
  numbers would share a file; with sector-exact track positions that needs the same master.
- **Out of band only; no writable mount.** *Owner decision:* out-of-band only,
  through `sacdfs tags import` from extracts, beets-pysacd export, and a CLI/TOML for hand edits;
  the mount stays read-only, and a writable mount is not planned. *Why:* taggers rewrite whole
  multi-gigabyte files; accepting that over FUSE, NFS and SMB is a lot of risk for what the
  importer and beets already give.
- **Art only when named.** *Owner decision:* art is embedded only when a disc's tags file names an
  image, and with no tags file the output stays byte-identical. The picture is an `APIC` front cover
  appended to scarletbook's tag (as `Tags::Raw`), at most 960 KiB, since scarletbook caps a tag
  at 1 MiB. *Trade-off:* the tag is held in memory per listed track file, so a picture costs
  its size per track and format of the discs that embed one (33 of 1,066 files in the library
  check); sharing it between files is a follow-up if it matters.
- **Byte identity.** Without `--tags`, nothing is looked up. With it, a file whose tag comes
  out equal to the TOC's (no tags file, or nothing set for that track) keeps its bytes, mtime
  and generation. *Why not bump `DSF_VERSION`/`WAV_VERSION`:* those move every file; only the
  files a tags file changes get a new generation (the old one hashed with the tag's hash) and
  an mtime no older than the tags file's. The audio never changes, so the DSF gate and the
  golden audio hashes are untouched.
- **Merging:** items the library knows (title, genre, album ...) are set in its `TagSet`, so
  they are written as the TOC's are, in sacdfs's frame order; `TXXX` too. Other frames (`UFID`,
  `COMM`, other text frames) and the picture are appended after them. Levels: album, area
  album, track, area track; the later wins. An `album` from the tags file gets no multichannel
  suffix (#97 hook): the user named it.
- **Reading:** a tags file is read when a track file is made, that is when its area is first
  listed, and kept while its mtime and size are unchanged. A file that does not parse (unknown
  key or section) is ignored with a warning, so a typo never changes the served files.
- **Live reload (owner requirement: pruning scanners such as Roon must see tag edits).** On a
  directory SOURCE (a watching tree, #101), adding, editing or removing a tags file changes the
  affected files' tags, size, mtime and generation and moves the mtime of the image's folder,
  its area folders and every folder above, exactly as a new sidecar does (#123), within the
  refresh interval (2 s). The sweep (`Tree::sweep`, run when the root's attributes are asked
  for) looks at the tags files of the images: one `stat` of the tags directory, one per tags
  file known, and a listing of the directory only when its mtime moved (a file added or
  removed). A change is a bump under the image's path (`Tree::bump`), so the image and area
  folders, which now key their mtime on that path, move with their ancestors. An area listed
  again re-makes its track files; one whose tag changed replaces its node under the same
  number (`Tree::renew`), and a file whose tag is the same keeps its node, so open files and
  caches stay as they are (an open handle keeps the file it opened). The mtime of a bump is
  the tags file's when it moved forward (a remount shows the same), else the time now (a
  removal, or an older time put back), so that the folders always move.
  *Why the image's path, not the disc key:* scanners walk folders, and the image's path is
  what the folders above it are made of.
  *Why also across mounts:* a disc's key needs its TOC, read only when the image is opened
  (on its folder's first listing). A tags file written while sacdfs was stopped, or for an
  image not opened yet, would otherwise move nothing above the image, and a pruning scanner
  would never open it. So each opened image's key and its tags file's last state are kept in
  `tag-keys` in the seek index's cache directory, by a SHA-256 of the image's path (never the
  path), and checked at the mount, before anything is shown: every folder above an image with
  a tags file shows an mtime no older than it, and one changed or removed since moves.
  *Alternatives:* opening every image at the mount for its key costs a TOC read per image
  ("latency is important"); putting the tags directory's mtime in every folder's would make a
  scanner rescan the whole library for one edit. *Trade-off:* a stale entry (an image
  replaced under the same path) only moves a few mtimes once, and the image's own change
  moves them anyway; without a cache directory (`--no-index-cache`), an image not yet opened
  in this mount is found on its first listing only. A single-image SOURCE does not watch:
  there an edit shows after a remount.
- **Import** runs `verify` with the user's arguments and keeps only IDENTICAL and
  IDENTICAL-PAUSE rows: the matching is verify's, so a tag is never attached to a track whose
  audio differs. Per extract it takes the frames `Losses::between` calls lost against the
  untagged served file (so nothing the TOC already says is copied) and the front cover. Values
  shared by every track of an area become area-album values, shared by both areas album
  values. Existing tags files keep their values (hand edits win), and files are written
  through a rename. *Why reuse verify:* the owner asked for verify's matching, and it is the
  proof the extract may be deleted.

### Discs without text (#185)

Some real discs have little or no text in their TOC, so 0.4.0 served `01.wav` with an empty title
and no album name, and players filed them under "Various Artists". Phase 1 of #185 fills the gaps
with no network and no stored state; the lookups of its later phases write tags files instead.

- **Titles.** A track without a title gets the title `Track NN` and the file name
  `NN - Track NN.dsf`. Titles stay per area: a stereo area's title does not fill a multichannel
  track, since nothing says the two areas hold the same tracks in the same order.
- **Album and artist from the path.** `fallback.rs` holds the rule. *Owner decision (the phase
  split was delegated):* this part is always on and never uses the network. *Why so narrow:* a
  wrong artist is worse than a blank one, because a player then files the disc under someone else.
  So only a shape that can't be read another way counts (`Artist - YYYY - Album [...]` in the
  image's or the folder's name), or two names that agree (a `YYYY - Album [...]` folder, with an
  image name that starts with the folder above it, or reads `Artist - Album` with the same album).
  The folder above alone is not enough: it may be a genre or a format. Otherwise the album is the
  image's file name and the artist stays blank.
  *Why the second shape, beyond the issue's pattern:* the 21 areas with blank titles in the 0.4.0
  library survey are all laid out `Artist/YYYY - Album [...]/`, and their folders don't match the
  issue's pattern. *Alternatives:* any folder above as the artist (wrong on `SACD/2001 - Album/`);
  splitting `Artist - Album` image names without other evidence (wrong on `Album - Disc 1`);
  reading the year as a date tag (a folder's year may be a reissue's). *Trade-off:* a
  disc in `Artist/YYYY - Album/disc.iso` gets an album and no artist; a folder with several
  images gives them all one album name, as a set's discs share theirs.
- **Who wins.** The disc's own text, then a tags file over all of it, an empty value included:
  the fill-in is made before the overrides are applied (`dsf::tagged`). An album from the path
  takes the multichannel suffix (#97) as the disc's own does. The artist is set, as both TPE1 and
  TPE2, only if the disc has no artist text at all, so that a disc naming some performers isn't
  split by a guess on the others.
- **File names stay the disc's.** A tags file changes tags, not names, as before (#81): a track
  given a title there keeps the name `NN - Track NN`. *Why:* a name that follows the tags file
  would move a file when a tag is edited, and players would see a removal and an addition. Lookups
  keep to this ([Looking tags up](#looking-tags-up-185-phase-2)).
- **Path safety.** What comes from the path goes only into tags, never into a path of the mount;
  a name that isn't valid UTF-8 or holds control characters gives nothing to the patterns, and
  the plain name goes through `names::sanitize`.
- **Times, and why not the output-version rule as written.** *Choice:* a file with filled-in text
  counts as one output version past its format's (`ident::file_version`), which moves its mtime
  by one step and changes its generation; other files keep theirs, byte for byte. Folders move one
  step if they have an image below (`ident::newest_offset`, `ident::FILLED_STEP`), and that includes
a folder newer than its images, whose own time would otherwise hide the move; folders with only
sidecars below stay. *Why:* as with tags files, only those files'
  bytes change, and a bump of `DSF_VERSION`/`WAV_VERSION` would make every scanner re-read, and a
  player like Plex or Roon possibly re-analyse, every file of a large library for a few tracks. *Why
  all folders:* whether a disc has text takes its TOC, and the mount reads no TOC, only a small probe of each
  image (#175); a
  scanner that prunes unchanged folders (#123) would never reach the disc otherwise. *Measured:* before the folders newer than their images moved too, 24 of 259 changed files on the
  0.4.0 survey library had a folder above them that kept its time. *Trade-off:* a
  pruning scanner walks all its folders once after the upgrade, and a folder can show a time up to
  one step (7 hours) newer than anything below it, or ahead of the clock just after a change. Files keep their inode numbers, so a rename
  is only a new name for the same number. `golden_dsf_corpus` hashes paths and header sizes, so
  its value moved without a version bump, this once; the audio was compared with 0.4.0's on the
  pull request.
- **`verify` and `tags import`** compare an extract with the tag without the fill-in from the path
  (`TrackFile::stored_frames`). *Why:* text that happens to match the folder's name is still lost
  if the extract goes and the folder is later renamed, so `verify` reports it and `import` keeps it.
  The generated `Track NN` title does count as served. *Why:* it depends only on the track number,
  so nothing is lost with the extract, and an import storing it would look like a hand edit and
  keep a lookup's title out. *Alternative:* a check in `import` alone, which would leave `verify`
  reporting a loss that isn't one. *Trade-off:* a title set by hand to exactly `Track 03` on track 3
  isn't imported, and is served the same anyway. `Track 3`, or `Track 03` on another track, is kept.

### Looking tags up (#185, phase 2)

`sacdfs tags fetch IMAGE...` looks a disc up on MusicBrainz and writes to its tags file
(`src/lookup.rs`, `src/lookup/mb.rs`). It runs on demand: never in a mount or the FUSE path, and
with no worker (phase 4). *Why a command first:* it keeps the server process simple until the
fingerprinting phase's soak numbers exist. *Trade-off:* a user runs it, or schedules it.

- **The method is pysacd's, made stricter.** pysacd takes the best candidate, with a person
  checking its output as the fallback. Here the result is served to players with nobody looking, and
  a wrong match is worse than none, so:

  | Point | pysacd | sacdfs |
  |---|---|---|
  | A search hit | a candidate | must carry the number or names searched for (a search matches on analysed words) |
  | Tracks without a length | left out of the mean, so one length can confirm a disc | each must have a length, or its ISRC on the recording |
  | One wrong track | hidden in a mean (30 s over 12 tracks is 2.5 s) | a limit per track too |
  | ISRCs | hits can excuse durations | never; more disagreeing than agreeing refuses |
  | Limits | 3 s mean for all | tighter as the way of finding it weakens |
  | Several fit | the best | SACD media first, then the most ISRC hits, then the path's year; if text still differs, nothing |
  | A later, weaker method | n/a | can't overrule a stronger one's ambiguity |

  *Why each limit:* a number identifies the release, so durations guard against a wrong number
  (5 s mean, 15 s per track). A name says less (3 s, 6 s). Durations alone say least, and the fuzzy
  lookup returns near misses on purpose, so only the same master should fit (1 s, 2.5 s, 4 tracks at
  least). *Measured:* on 60 real discs with their text hidden, of 100 media whose titles agreed with
  the disc, 72 were within the first limits chosen (6 s, 3 s) and 90 within these; the worst track
  was 10 to 20 s off on 15 and over 30 s on 4, and many were off by a similar 2 to 5 s on every track,
  which is what a different split of pauses between tracks would give (the cause wasn't checked). Releases with other tracks were over 60 s off. Going from 6 s and 3 s to
  15 s and 5 s took discs with at least one matched area from 35 to 44 of 60, every one right by hand; by
  name, on 30 discs, 4 s and 2 s to 6 s and 3 s took them from 11 to 15, and wider gained none. *Not
  measured:* the limits for durations alone, since the run ended before that step, on the service's
  503s. *Trade-off:* a wrong release with the same number, the same track count and tracks within
  15 s would pass; none was seen. Real discs still refused: 16 of 60, 8 with no release found, 5 on ISRCs
  that disagree and 3 on durations.
- **ISRCs that disagree still refuse, also by number.** The coordinator decided this under the owner's
  delegation, after real discs. *Why:* a wrong tag written to a library costs more than a disc left
  unmatched, and the sample is small. *Measured:* without the rule, 49 of 60 discs matched against 44, and
  the 5 extra were right by hand. *Alternative:* no refusal at the number step, where the number and
  durations already agree. *Trade-off:* those discs need tags by hand, and the command says so; a name
  lookup found nothing for any of the 5.
- **Recorded case: why text must agree.** A live query by durations for one hybrid disc returned the
  disc, a digital release of it, and a box set holding it under another album title. Each has
  fitting durations and the same track titles. Taking "the best" would make the album name a coin
  toss, so that disc is refused. Another release listed two discs with the same durations, a stereo
  mix and a 5.1 mix, whose titles differ: refused, unless the disc's set number names one.
- **What several agreeing candidates leave out.** Text must be the same in all. IDs and the date
  must be too or they are dropped, so that pressings of one album still give titles. A name or
  durations never write a release ID or date: they find the album, not the pressing.
- **One disc, one album.** An ambiguous area stops the whole disc, as do two areas matching different
  albums. An area that doesn't fit the release its sibling matched is left alone and gets no second
  lookup by a weaker way.
- **Who wins.** A value anywhere in the tags file, in any spelling (`TIT2`, `title`) and empty ones
  included, because hand edits win (#81); then the disc's text, since a label's own text may be
  correct where a database romanises it; then the lookup. *Alternative:* the lookup replacing disc
  text. *Trade-off:* a disc with bad text keeps it unless it is edited by hand. Values go at the
  least specific level that says them, so a later edit at a more specific one wins as levels do.
- **A title from a lookup doesn't rename the file.** *Choice:* the name stays `NN - Track NN` and the
  title goes in the tag. *Why:* phase 1's rename already moved those files once, and players likely
  key play history on paths; a second rename, on top of one per tag edit afterwards, would likely
  lose it again (not tested with a player). Players show the tag; only file browsers show the
  name. It also keeps third-party text out of paths, and listing a folder needs no tags file.
  *Alternatives:* names that follow the tags file (a move per edit); renaming on lookups only (the
  same move, and a mount that must know who wrote a value). *Trade-off accepted:* file names stay
  generic on these discs, which shows in file browsers and players that list by name.
- **`Track NN` already in a tags file gives way.** `tags import` from 0.4.1 stored it from an extract
  of a mount. Fetch removes it where it has a title for that track. *Trade-off:* a title set by hand
  to exactly that text is replaced; such a choice is improbable, and the file says the same thing
  once a real title is wanted.
- **Path-derived text an older import stored is still kept.** An extract of a 0.4.1 mount can hold
  an album and artist from the path, and `import` keeps them on purpose (above), so a later fetch sees
  an edit and leaves them. *Why left:* that rule protects text from a renamed folder, and telling
  the two apart needs a path that may have changed.
- **A busy service.** *Why it needs care:* on real discs the service answered 503 "currently busy",
  with a `Retry-After` of 0, about once in 20 requests of an evening, so a large library meets it
  in every run. `Retry-After` is obeyed in full up to an hour (it was cut to 60 s, which asked early); a
  429 waits at least 60 s. A disc with no answer is followed by a wait of 1, 2, 4 and 8 minutes and
  is asked about again at the end of the run; 5 in a row end it, about a quarter of an hour of outage.
  A 403 or a request to wait over an hour ends the run at once. *Alternatives:* waiting without
  limit (an unattended run could sit for days); 3 in a row and no second try (about 90 s of outage lost the
  batch). *Trade-off:* a run in an outage takes longer to say so.
- **The client.** HTTPS only, no redirects, one fixed host, 8 MiB per response, 60 s per request,
  5 tries with growing waits on 429 and 5xx. Every try starts at least 1.1 s after the last, as in
  pysacd; the limit is per process. Search
  results list track counts, so a release that can't fit isn't read: a popular album's name search
  would otherwise cost 25 requests. Responses are hostile input: control characters, overlong text,
  placeholders and malformed ids are dropped, and an id reaches a URL only if it is a UUID. The
  `lookup_json` fuzz target covers this. Its tests answer from recorded responses, so CI never
  touches the network.
- **ureq with rustls and bundled roots.** *Why:* a release binary that needs no OpenSSL or CA store,
  including on the older systems the release supports (it isn't static, but links only glibc).
  *Alternatives:* running `curl` (a run-time dependency, and an argument and exit-code interface);
  native-tls (an OpenSSL dependency that the glibc-baseline build would then pin). *Trade-off:* about
  3 MB of binary and 15 crates for one optional command, and root certificates that age with the
  release; `deny.toml` now allows ISC (rustls's and ring's parts) and CDLA-Permissive-2.0
  (webpki-roots's data), both compatible with GPL-3.0-or-later. Nothing here runs in the mount.

## Verifying extracts (`sacdfs verify`, #80)

The owner's aim for sacdfs is reclaiming storage (#82): people delete the DSF and DFF files
they extracted from their images and keep one copy of each disc, the image. `sacdfs verify`
gives the per-file proof that this loses no audio, and lists what it would lose in tags. It
never deletes anything; the user acts on its report. The method is the one a one-off run on
the owner's NAS already used.

- **A subcommand, `sacdfs verify`, not a separate binary.** *Why:* one binary to install,
  ship and package: the release archives, the container image and the packaging all carry
  `sacdfs` only, and the check must use exactly the build that will serve the files. The
  first argument `verify` selects it before the mount's options are parsed, so `sacdfs
  --help` and the mount's command line are unchanged. *Trade-off:* a directory named `verify`
  in the current directory must be given as `./verify` to mount it (documented). A clap
  subcommand next to the mount's positional arguments would have changed the mount's help and
  usage lines for one rarely used command; a second binary (`sacdfs-verify`) would have
  needed the release workflow, archives and container to change.
- **In-process, through the mount's own code.** The files sacdfs would serve are made by the
  same `Tree` (with the same per-track checks that leave unplayable tracks out) and read
  with the same `track::Reader` as a mount reads them, then reduced by the same container
  reader as the extracts. *Why:* the proof is about the bytes a mount serves, not about a
  second decoding path that could disagree with it; and no FUSE mount is needed, so it runs
  wherever sacdfs is installed, in the container too, and can't hang a mount.
- **Canonical audio, compared by SHA-256.** Every file, extract or served, is reduced to the
  header's samples per channel, byte-interleaved across channels with the earliest sample in
  the most significant bit (the disc's and DFF's order; DSF's LSB-first blocks are
  bit-reversed and interleaved), so tags, padding, block size, bit order and container don't
  count. *Why:* tags differ by design between extractors, and an extract's padding and chunk
  layout say nothing about the audio. A digest (rather than a byte-wise comparison of two
  streams) lets each served track be decoded once however many extracts point at it, and
  goes in the report, where it identifies duplicate copies. SHA-256 is already a dependency.
- **Matching: the same folder, channels and length, then content.** An extract belongs to the
  images in its folder or the nearest folder above it that holds images (after `--map`).
  Only tracks with its channel count and sample count are candidates, the one its file name's
  number points at first, so normally exactly one track is decoded per extract. *Why:* the
  shape check costs nothing and avoids decoding every track of a disc; content decides, so a
  misnumbered file still matches. A file that matches nothing gets the nearest track by length
  in its reason, since a track-boundary difference (pauses included or not) is the usual
  cause.
- **Separate trees by prefix mapping (`--map FROM TO`).** *Why:* the two layouts the owner's
  library and common practice use are extracts next to the image and a mirror tree; a prefix
  map covers the second without guessing from names. Matching by disc text or digest across
  the whole library would cost a decode of every disc per unmatched file.
- **Statuses.** IDENTICAL, DIFFERENT (same shape, other audio), NO-MATCH, UNSUPPORTED (a
  valid file that can't be compared: DST-compressed DFF, which would need a DST decoder for
  the extract side, an SACD format concern that belongs to scarletbook), ERROR (truncated,
  broken, unreadable) and NOT-COMPARABLE for FLAC and WAV (#80: a CD-layer rip isn't in the
  image, and another converter's PCM never matches sacdfs's bit for bit). A truncated file is
  an ERROR, not DIFFERENT: it isn't a valid file of other audio.
- **What a deletion loses.** The extract's ID3 frames (v2.2 to v2.4) are compared with those
  of sacdfs's tag for the matched track, by frame and value; the ones sacdfs lacks are
  grouped as MusicBrainz IDs, art, ReplayGain and other frame names. DFF text chunks
  (DIIN/DITI, DIIN/DIAR) are compared with the title and artist. *Why:* the user's decision is
  "delete, or move these tags first" (to a tag layer, #81), so the report says which.
- **A track total is not a loss (#99).** `TRCK` of `n/N` against a served `n` (or `0n`) is
  the same information when `N` is the track count of the served track's area; a different
  number or a different total is still reported. *Why:* sacdfs knows the total from the TOC, so
  nothing is lost, and 241 of 1,066 IDENTICAL extracts on a real library were reported as
  losing it. Option 2 of the issue (serving `n/N`, a tag-byte change and output version bump)
  was not taken: verify's rule fixes the report without changing any served byte.
- **Images in subfolders (#98).** An extract belongs to the nearest folder above it that holds
  images; when there is none, to every folder of images up to two levels below its own folder
  (`Album/Album (SACD ISO)/disc.iso`), and all their tracks are candidates, as for several
  images in one folder. *Why:* a common download layout; 17 of a real library's extracts that
  another checker confirmed were NO-MATCH. Beside or above still wins, so an existing result
  never changes. The limit of two keeps a file at the top of a big library from trying the whole
  library; a match still needs the same audio, so a wider net cannot misreport. NO-MATCH now
  says the subfolders were searched and points to `--map`.
- **Pause-kept extracts (#93), `IDENTICAL-PAUSE`.** When no track has the extract's length but
  one within 60 s of it does, the shorter stream (extract or served track) is looked for inside
  the longer: a 1024-frame needle of non-idle audio from the short one is searched for in the
  first `difference + needle` bytes of the long one (the offset can't exceed the difference),
  each hit is confirmed by the SHA-256 of the long stream from there, and the audio outside
  is measured by region. *Why this way:* the long stream is read once more at most (the search
  stops early), no offset is guessed, and a hash decides, so a false hit cannot match. Silence
  is the exact DSD idle pattern (0x69 and its rotations), or no 4096-sample window with an
  imbalance of ones and zeros over 4 (two bits, -60.2 dB; one bit is -66.2 dB). *Why:* the
  owner's survey of a real library (844 extra regions) found 538 non-silent regions reading
  exactly the one- or two-bit values, noise-shaper residue and not content; exact idle patterns
  alone would clear only a third of the files. A short last window is judged by the same
  absolute imbalance. The cases: extract = track plus silent pauses (head, tail or both);
  extract a prefix or inner part of the served track, where sacdfs serves only silence beyond
  (older extractors drop the last 1/75 s frame); anything else extra is reported with its
  length and level (NO-MATCH, "deleting would lose it" or "the extract lacks audio sacdfs
  serves"). *Alternatives:* a new status vs. a reason on IDENTICAL: a status, so users and
  filters see why it matched, and the exit status and reclaimable total count it. Serving
  pauses (the issue's option 2) is not done here, and sacdfs keeps serving the TOC's length.
  *Trade-off:* a track that is entirely idle pattern has no needle, so is not compared this
  way; extras over 60 s are different recordings, not pauses (the longest seen is 24 s).
- **Parallel and bounded.** `--threads N` workers (plain threads, not a rayon pool, so that a
  worker waiting for another's decode of the same track can't be handed that same work by
  work stealing) check files at once, with an engine of N DST decoder threads and a small
  frame cache (64 MiB). Each reader holds a few MiB; a tag over 16 MiB is not read (reported
  as `unread`). The images of every folder needed are opened up front, on one thread.
- **Resumable at no cost.** Rows are written and flushed as they are made, to `FILE.partial`,
  so an interrupted run leaves what it finished; `--resume` keeps rows whose file has the same
  size and modification time (except ERROR rows) and checks the rest. At the end the report is
  written sorted to `FILE`, at once. *Trade-off:* a resumed row isn't re-checked if only the
  image changed; deleting the report starts afresh.
- **A partial result never looks whole (#196).** `verify` is what a user trusts before deleting
  extracts, and a panic in a worker used to end that worker unnoticed: exit status 0, and the
  file (with one thread, every later file) left out of the report. Now a panic is that file's
  ERROR row, and a run with fewer rows than files fails. `FILE` exists only after a finished
  run, since a report with rows missing looks like a report of fewer files. *Alternatives:* a
  last line saying "complete" in `FILE` (every reader of the TSV would have to check it, and the
  ones that don't are the problem); stopping the whole run at a panic (one bug would hide the
  other files' results). An earlier report is not deleted when a run starts, since a run that fails
  should not cost a report that can take half an hour, nor may it be
  mistaken for the new one: it moves to `FILE.previous` once the run has its first rows, and goes
  when the run finishes. *Alternative:* keeping `FILE` in place until the end, which was rejected
  because a stopped run would leave an old report under the name of the new one. *Trade-off:* a
  tool that watches `FILE` during a run finds nothing until the end, and a stopped run leaves
  two more files beside it.
- **Hostile input.** Extracts are untrusted like images: every size in a DSF or DFF header is
  checked against the file's length before use, chunk walks are bounded, and the readers are
  fuzzed in the unit tests by truncating and corrupting every header byte, and by the fuzz
  targets `verify_container` and `verify_id3` (#161). The DSF and DFF
  readers are container parsing written here from the published format descriptions; no SACD
  parsing or DST code is involved on the extract side.

## Testing

- **Unit tests:** DSF header and tag layout, offset-to-frame mapping (including the padded
  last block and every channel count 1 to 6), name sanitising; proptest of the offset
  mapping. Fuzzing the image layer's parsers is scarletbook's job.
- **Fuzzing (#161).** `fuzz/` has cargo-fuzz targets for what sacdfs adds on top of the
  library: tags files, sidecar paths, names, the tag and header writers, the tree, and the
  `verify` readers; fuzz/README.md lists them. They check more than "no panic": a name the
  kernel accepts, a tag with its true size, nothing read from outside SOURCE. *Why in CI as a
  job of its own, not required:* a fuzzer's result depends on the run, and a new crash in old
  code shouldn't block an unrelated pull request. *Why a crate of its own:* cargo-fuzz needs a
  nightly toolchain, which stays away from sacdfs's build and locked dependencies.
  *Alternative:* longer runs on every pull request, rejected by the CI speed rule.
  *Trade-off:* 60 s per target finds only the easy bugs, so a weekly run goes on for 20 minutes
  per target from the corpus the last one left.
- **The corpus.** Integration tests run against the synthetic CC0 conformance corpus of
  [sacd-spec](https://github.com/aclater/sacd-spec) (copied into `tests/corpus/` with its
  licence). `tests/corpus.rs` reads every case through the tree and readers without
  mounting, twice: with the default seeking, and with every read that does not continue its
  cursor starting a new one, provisionally wherever the library allows (`Tuning::exhaustive`),
  and no frame cache, so that seeking is exercised on the small corpus images too (hostile
  timelines included), and reads every frame of every file alone from a fresh reader. `tests/mount.rs` mounts the corpus with the binary, reads every file through
  the kernel and compares per-channel hashes with the corpus manifest, then remounts and
  compares scattered reads on fresh opens; it also checks background mode, read-only refusal
  and unmounting by SIGTERM and SIGINT.
- **Persistent seek index:** unit tests of the file format (round trip; another size, mtime,
  path or area shape misses; every single damaged byte and every truncation is rejected; an
  area over its bound is not written; proptest of random and damaged files, with the checksum
  recomputed after the damage so that the parser itself is fuzzed), of the directory (private
  mode, LRU trimming, symbolic links and oversized files not read). `tests/index_cache.rs`
  saves the seek hints of a straight read and seeks into every corpus case, then reads every
  frame alone and random ranges with them loaded, each frame also from a fresh tree so that
  only the loaded index is known, and requires the bytes of a straight read; the same with
  damaged entries (proptest of the saved indexes' bytes changed at random) and with a stale one
  (audio sectors moved under an entry with the image's path, size and mtime), which must cost
  only speed; and a changed size or mtime misses. `tests/mount_index.rs` mounts, seeks,
  unmounts, remounts and seeks again through the kernel: the same bytes, with no more calls to
  the image and no more bytes read (on the small corpus image a cold seek is already one call).
- **Against scarletbook's exact reading:** `tests/library_index.rs`: every frame and random
  ranges of every corpus case read through sacdfs equal reads through a complete library index
  and a straight library read; provisional library readings on an image made for it (late audio
  sectors written again near an area's start) differ from the exact ones and are all refuted;
  an ignored test, `real_discs`, does the same on local real discs (`scripts/real-discs.sh DIR`).
- **DSF header reads:** `tests/dsf_header.rs` reads every DSF file of the corpus as the
  kernel does for a tagger (128 KiB at 0, 128 KiB at 128 KiB, the tag's page): none of the
  reads starts read-ahead, and a player reading on starts it exactly at 256 KiB.
- **Open images:** unit tests of the handle cache (reuse, the bound, idle closing, a new
  handle after a change of size, of modification time or of the file, and none within the
  check interval). `tests/handles.rs` walks the whole corpus with at most 3 images open,
  reading every file whole and then pieces of all files interleaved (so that handles are
  closed and opened again mid-file), and requires the bytes of a walk with every image open
  and never more than 3 image descriptors in `/proc/self/fd`; and mounts the corpus with
  `--max-open-images 2`, reads the start, middle and end of every file through the kernel,
  and checks sacdfs's own descriptors.
- **Concurrent reads:** `tests/concurrent.rs` reads every file of every corpus disc on 4
  threads per disc at once (each with its own readers, in another order and piece size), all
  discs together, every read searching, no frame cache and at most 2 images open, and
  requires the bytes of a straight read.
- **WAV files:** `tests/wav.rs`, every check written against the converter's API only, so that
  it holds for any converter whose output for a frame depends only on that frame and its two
  neighbours. At every rate, every WAV file of every corpus case parses (RIFF, fmt, data,
  INFO, ID3), has the exact size, channel mask and sample count, carries the DSF file's tag,
  reads the same in odd chunk sizes, and holds exactly its positions of a reference stream of
  the whole area (scanned from the area's first sector and converted frame by frame with a
  fresh converter); the area's files concatenated equal the same positions of that stream
  (gapless). Random ranges (proptest) and every frame on its own, with the exhaustive search
  and no cache, equal the straight read. Listing and reading the header and tags of every file
  reads no audio sector and fills no cache; a sequential read makes about as many calls to the
  image as the DSF file's. `tests/mount.rs` reads every WAV file through the kernel and
  compares it with the library, and checks `--format` and `--pcm-*`. An ignored test,
  `real_discs`, runs the stream check on local real discs (`scripts/real-discs.sh DIR`).
- **The converter:** `tests/pcm.rs` and the unit tests of `src/pcm.rs` (quality against the
  targets, silence, determinism, pinned hashes), black-box on scarletbook-pcm; see
  [pcm-quality.md](pcm-quality.md).
- **Speaker layouts:** a test reads the header of every file of every corpus case and compares
  the mask and type with the manifest's channel count and configuration (written out in the
  test, not taken from sacdfs or scarletbook): stereo, 3/2 and 5.1, and six unspecified
  combinations.
- **File identity:** unit tests (`src/ident.rs`, `src/tree.rs`): the bit layout; a pinned key;
  forced collisions, which fall back the same way whatever the walk order; the same numbers on
  every tree, listed in any order; the same numbers for images, areas and files with and
  without `--flatten`; every number found from a fresh tree that knows only the number,
  opening only the images on its way, and numbers that name nothing not found; PCM options
  change the WAV files' generations only; a changed mtime changes every file's generation.
  `tests/export.rs` mounts through the kernel: the whole corpus has distinct numbers, the same
  after a remount, and with `--flatten` the same for what is still shown; directory handles
  (`name_to_handle_at`) kept across a restart of sacdfs open again (`open_by_handle_at`, which
  sends `.` and `..`) and list the same entries; file handles kept across a restart read on
  where they were, and a changed image's go stale. Opening file handles needs
  `CAP_DAC_READ_SEARCH` in the initial user namespace, so CI's `nfs` workflow runs these tests
  as root; directory handles also open in a container (`--cap-add DAC_READ_SEARCH`).
- **NFS export:** `tests/nfs-loopback.sh` (CI, in a privileged Fedora container with the
  runner's nfsd) exports a mount with the kernel NFS server, mounts it over loopback NFSv3,
  reads half of each of several files through open descriptors with `O_DIRECT` (so no read
  is answered from the client's cache), restarts sacdfs, and reads the rest through the same
  descriptors; then changes an image and requires `ESTALE` from its open file.
- **Container:** the `container` workflow builds the image, serves the corpus, mounts it over
  SMB and NFS with the kernel clients from a client container, and checks every track's
  per-channel hashes and every WAV header (`deploy/check-share.py`); a second run checks user
  access (guests refused), `--format dsf` and `--flatten`; in between, a client reads half of
  a file over each protocol, the container restarts, and it reads the rest through the same
  open file.
- **Real discs** (locally, not in CI; the discs can't be published). The same per-channel
  SHA-256 check over the `Stereo` and `Multichannel` directories of five real discs, against
  reference hashes made from `pysacd-extract` output: 93 of 93 tracks in 7 areas identical,
  with no warnings logged. Their WAV files: all 93 equal their positions of a reference
  stream of the whole area (`real_discs` in `tests/wav.rs`); FFmpeg (`ffprobe`) and Python's
  `wave` read all 93 at every rate, with the expected duration, rate, channels, layout
  (stereo, 5.0, 5.1), `pcm_s24le` and tags; libsndfile reads every WAV file of the corpus,
  its samples equal to the raw bytes; a sparse 4.76 GB six-channel RF64 file reads in FFmpeg
  and libsndfile.

### Leaks (#151)

The owner asked for leak tracing and profiling: sacdfs runs for days on a NAS. How to run
each tool is in [development.md](development.md).

- **On every PR: `tests/no_growth.rs`, a dhat-rs test.** It runs mount cycles in-process
  and requires the heap left after each unmount to stay flat.
  - *Why in-process:* dhat counts only its own process's allocations, so the filesystem
    runs on a fuser background session in the test.
  - *Why mount cycles, not hours:* the owner asked that CI not take too long. The test
    takes about 14 s on the CI runners, and the `tests` job grew from 2 min 23 s to
    2 min 51 s on main to 3 min 12 s on the PR's first run. That is under the 1-minute budget
    the owner set.
  - *Why 16 KiB of slack:* a deliberately leaked tree per cycle adds about 40 KiB a cycle,
    while the measured noise is 32 bytes.
  - *Alternatives:* a counting global allocator written in the test. Rejected because it
    needs `unsafe`, and dhat-rs is the tool #151 names.
- **Off PR CI:**
  - `scripts/soak.py`: hours of churn on a test host, plus 20 minutes weekly in
    `.github/workflows/leaks.yml`.
  - `scripts/leak-check.sh massif`: the heap profile of a soak segment.
  - `scripts/leak-check.sh memcheck`: the whole suite, and the `sacdfs` processes it starts,
    under valgrind memcheck. It also runs weekly.

  *Why off PR CI:* each takes from 20 minutes to hours. *Why the soak judges the trend rather
  than a fixed ceiling:* the frame cache and malloc's arenas fill during the warm-up. After
  it, what counts is the level staying within the cache cap plus noise, and the trend of the
  second half staying near zero.
- **Why memcheck, not LeakSanitizer.** There is no C code in the graph (sacdfs and
  scarletbook are pure Rust), so there is no C boundary to instrument. Rust's LeakSanitizer
  needs a nightly compiler, and clang's `-shared-libasan` route does not instrument Rust.
  memcheck works with the stable toolchain and follows child processes.
- **Found: every WAV file's header was kept whole in memory**, 256 KiB per listed WAV file,
  almost all of it the `JUNK` padding's zeros. massif put 522 MiB of a 40-minute soak's 635 MiB
  heap at `wav::header`. The fix landed through #150 at the same time, so this PR has none. On
  the five gate discs (93 WAV files), the resident memory after listing everything fell from
  32.3 MB to 8.8 MB, which is about 5 GB less for a 1,000-disc library after a full scan.
- **Found, filed and fixed separately: #153.** Nodes, and with them discs, of removed or
  renamed images stayed in the tree for good. With every added album under a new name, the
  soak grew from 135 to 394 MiB in 19 minutes. *Why not fixed here:* it needed a design
  change in the tree, in another lane; #163 fixed it. The soak's default churn (a new name
  for every added album) covers it from then on. `--churn-slots K` (names reused) checks
  everything else in isolation.
- **Why massif, not heaptrack.** heaptrack is not in UBI, and the Red Hat-first rule applies.
  massif, in UBI 10's valgrind, gives the same two answers: what is live at the end, and how
  each site grew. The container is `scripts/leak-check.Containerfile`.

### Oldest distributions (#58)

*Why:* the install docs ([binary.md](install/binary.md#binaries)) promise RHEL 8, Debian 10, Ubuntu 20.04 and Fedora 29, but CI tests only
a current runner (`fusermount3` 3.18, a current kernel), and the reviewed changelogs of other
FUSE projects show `fusermount3` behaviour changing across libfuse versions (3.0 renamed
the helpers and dropped `nonempty`; 3.15 stopped silently accepting unsupported options),
and kernels before 4.20 have neither `FOPEN_CACHE_DIR` nor `max_pages`. A glibc check alone
shows that the binary loads, not that it mounts.

*How:* the generic x86-64 release build (built on UBI 8 like `release.yml`, `-C
target-cpu=x86-64`), in a fresh VM of each distribution's last point release, from its
cloud image, as an ordinary user with the distribution's own `fuse3` package and its stock
kernel. The source is a real hybrid disc image, attached read-only. Per distribution:

- mount with `-f` and in the background, then with `--allow-other` in both modes (first
  refused without `user_allow_other` in `/etc/fuse.conf`, with fusermount3's message, then
  set);
- the listing (24 files: names and sizes) and SHA-256 of a stereo DSF file, its WAV file
  and a multichannel DSF file, equal to the same files read on a current system from the
  same image; through `--allow-other` mounts, read as root; a private mount refuses root;
- SIGTERM while a file is open (4 MiB read through it): the mount is detached at once,
  sacdfs serves the open file until it is closed, then exits; the mount point is usable;
- `fusermount3 -u`: sacdfs exits;
- the INIT negotiation from fuser's debug log (`RUST_LOG=fuser::session=debug`).

| Distribution | Kernel | `fusermount3` | glibc | INIT: enabled | Result |
|---|---|---|---|---|---|
| RHEL 8.10 | 4.18.0-553.el8_10 | 3.3.0 | 2.28 | `ASYNC_READ`, `EXPORT_SUPPORT`, `BIG_WRITES`, `MAX_PAGES` | 21/21 |
| Fedora 29 | 4.18.16-300.fc29 | 3.6.2 | 2.28 | `ASYNC_READ`, `EXPORT_SUPPORT`, `BIG_WRITES` | 21/21 |
| Debian 10 | 4.19.0-27-cloud | 3.4.1 | 2.28 | `ASYNC_READ`, `EXPORT_SUPPORT`, `BIG_WRITES` | 21/21 |
| Ubuntu 20.04.6 | 5.4.0-216 | 3.9.0 | 2.31 | `ASYNC_READ`, `EXPORT_SUPPORT`, `BIG_WRITES`, `MAX_PAGES` | 21/21 |

The kernels without `max_pages` (Fedora 29's and Debian 10's; RHEL 8 has it backported)
cap a request at 128 KiB instead of 1 MiB, and those without `FOPEN_CACHE_DIR` ignore the
flag; reads are the same bytes either way. `FUSE_EXPORT_SUPPORT` is offered by every one of
them. Read-ahead is 128 KiB on all four. No `fusermount3` refused an option sacdfs passes
(`ro`, `nodev`, `nosuid`, `noexec`, `fsname`, `subtype`, `max_read`, `allow_other`). The
claim stands as written; nothing needed fixing. Not covered: the x86-64-v3 build (the same
code, differently compiled) and the container on these kernels. The test is a VM harness
outside the repository (it needs the real disc and VM images), so it is rerun by hand
before a release that changes how sacdfs mounts.

## Prior art

ratarmount (MIT; lazy archive indexing, seek points), squashfuse (BSD), fuser's examples
(MIT), libfuse's passthrough_hp (big reads, multithreading), Vangoor et al., "To FUSE or
Not to FUSE: Performance of User-Space File Systems", FAST 2017. GPL/LGPL FUSE archive
filesystems (fuse-zip, archivemount, mount-zip, rar2fs). sacdfs is GPL-3.0-or-later, so
code from compatibly licensed projects may be adapted, with attribution (see
CONTRIBUTING.md); SACD parsing and DST decoding still come only from scarletbook.
