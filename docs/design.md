# sacdfs design

sacdfs is a read-only FUSE filesystem that shows the audio of Super Audio CD disc images
as ordinary DSF files, decoded on demand. Point a player, a tagger or a file manager at
the mount point and every track of every disc is there, with tags, without extracting
anything first.

## Goals

1. **Correct audio.** A track read through the mount is bit-identical to the same track
   extracted with a reference extractor (pysacd's `pysacd-extract`, or scarletbook's
   `bench decode --sink raw`), checked by per-channel SHA-256 of the DSD stream.
2. **Fast.** Reading a track straight through runs close to the decoder's own speed:
   within 1.5x of `bench decode` wall time at the same thread count. A seek (a read at an
   arbitrary offset) returns its first bytes in under 100 ms on a desktop machine.
3. **Cheap to mount.** Mounting a library of hundreds of discs reads only master and area
   TOCs, lazily, when a directory is first listed. Nothing is decoded until a file is read.
4. **Flat memory.** A bounded cache of decoded frames, whatever the library size or the
   number of open files.
5. **Safe on hostile images.** Every image is untrusted input. A bad image shows up as a
   missing directory or an I/O error on one file, never as a crash of the mount.

Not goals for 0.1: writing, DSDIFF (DFF) output, PCM output (WAV/FLAC, which needs DSD to
PCM conversion), PS3 network sources, macOS/FreeBSD (Linux first; fuser supports both later).

## Engine

All SACD parsing and DST decoding comes from
[scarletbook](https://github.com/aclater/scarletbook-rs), the clean-room Rust library
(MIT OR Apache-2.0), used through its public API as a pinned git dependency until it is
published on crates.io. sacdfs adds no SACD format knowledge of its own beyond what that
API exposes: sector reading (`Image`), frame reassembly (`Demuxer`), TOCs and text
(`Toc`), and decoding (`DstDecoder`, `audio::decode_frame`).

## Layout

```
MOUNT/
  <relative dir of the image>/<image file stem>/
    Stereo/
      01 - <title>.dsf
      02 - <title>.dsf
    Multichannel/
      01 - <title>.dsf
```

- SOURCE is one image file or a directory, scanned recursively for `*.iso` files. A file
  that is not an SACD image (no master TOC) is left out; its directory still appears if
  it holds other images.
- `Stereo` and `Multichannel` appear only for areas the disc has.
- File names come from the track text (title), with `/`, NUL and control characters
  replaced; a track without a title is `NN.dsf`. Duplicates get ` (2)`, ` (3)`.
- Every inode number is stable for a mount's lifetime. Sizes are exact and known from the
  TOC alone.

## DSF files

A DSF file is the DSD chunk (28 bytes), the fmt chunk (52 bytes), the data chunk (12-byte
header, then audio), and an ID3v2.4 tag at the end, pointed to by the DSD chunk.

- Audio: the track's timeline positions `[start_frame, end_frame)` from the area TOC (the
  gaps between tracks are not included), each decoded to `C x 4704` bytes. DSF stores each
  channel in 4096-byte blocks, channel after channel, earliest sample in the least
  significant bit (bits per sample = 1). The last block of each channel is padded with
  zeros. Sample count per channel = frames x 4704 x 8.
- Channel type in fmt: from the channel count (2: stereo; 5: 5 channels; 6: 5.1).
- A position with no frame, or with an invalid DST frame, is filled as scarletbook fills
  it (0x69 / 0x55); a warning is logged once per file.
- Tag (built when the directory is listed, so the file size is known): TIT2 title, TPE1
  track performer (else album artist), TPE2 album artist, TALB album title, TRCK n/N,
  TPOS disc sequence/set size (when the set size > 1), TCOM composer, TDRC date, TSRC ISRC,
  TCON genre name, TPUB publisher, TCOP copyright. Text is UTF-8 (ID3v2.4 encoding 3).

## Reading a range of a file

A read of `[offset, offset + len)`:

1. Header and tag bytes come from a buffer built once per file.
2. Audio bytes map to frames: data byte `d` lies in block group `g = d / (4096 x C)`,
   channel `c = (d % (4096 x C)) / 4096`, channel byte `b = g x 4096 + d % 4096`, frame
   `b / 4704`. A read of 128 KiB of stereo covers about 14 frames.
3. The frames come from the decoded-frame cache, or are decoded: locate the first frame,
   demultiplex forward with `Demuxer`, and decode the DST frames in parallel (one
   `DstDecoder` per worker thread).
4. Each frame is converted from interleaved MSB-first to per-channel LSB-first once, when
   it enters the cache, so serving a read is copying.

**Locating a frame.** An area's audio sectors hold frames in increasing frame number, and
a fresh `Demuxer` fed sectors from any LSN yields correctly numbered frames from the
first frame that starts there. sacdfs keeps, per area, a sorted index of (frame number,
LSN of a sector where reading can start) learned from every read, and locates a frame by
starting from the nearest known point at or before it, else by binary search over the
area's sectors (each probe reads a handful of sectors). Sequential reads continue the
open file's demultiplexer state, so a straight read never searches.

**Read-ahead.** When reads on a file are sequential, sacdfs decodes the next window of
frames (default 2 s of audio) on the worker pool while the current read is answered, so
a player reading in real time, or `cp`, never waits for the decoder after the first read.

**Kernel caching.** The filesystem is read-only, so entries and attributes are cached for
a long time, files are opened with `keep_cache`, and the kernel's page cache serves
repeated reads. Large reads (`max_read` 1 MiB) and kernel read-ahead are enabled.

## Concurrency

fuser dispatches requests on one thread. Reads are answered from a thread pool: the
request's `ReplyData` moves to a worker, which decodes or copies and replies, so slow
reads on one file don't block `lookup`/`readdir` or reads of other files. Each image has
one open handle; sector reads are serialised per image (a `Mutex<Image>`), decoding is
not.

## Performance basis

scarletbook 0.2.0 decodes a stereo DST frame in about 0.215 ms on one core (about 65x real
time per core) and scales to about 13.7x at 16 threads; plain DSD areas need no decoding.
A straight read through the mount should therefore be limited by the copy and FUSE
overheads, not by decoding. Measured figures will go in `docs/performance.md`.

## Testing

- Unit tests: DSF header and tag layout, offset-to-frame mapping (including the padded
  last block and every channel count 1 to 6), name sanitising.
- Integration tests against the synthetic CC0 conformance corpus of
  [sacd-spec](https://github.com/aclater/sacd-spec) (copied into `tests/corpus/` with its
  licence): mount, list, read every file, compare per-channel hashes with the corpus
  manifest; random-offset reads equal the same slice of a straight read.
- Fuzzing the image layer is scarletbook's job; sacdfs fuzzes its own offset mapping with
  proptest.

## Prior art

ratarmount (MIT; lazy archive indexing, seek points), squashfuse (BSD), fuser's examples
(MIT), libfuse's passthrough_hp (big reads, multithreading), Vangoor et al., "To FUSE or
Not to FUSE: Performance of User-Space File Systems", FAST 2017. GPL/LGPL FUSE archive
filesystems (fuse-zip, archivemount, mount-zip, rar2fs) may inform ideas only; no code
is copied from them into this permissively licensed project.
