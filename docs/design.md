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

- SOURCE is one image file or a directory, scanned recursively for `*.iso` files (any
  case). A file that is not an SACD image (no master TOC) is left out; its directory still
  appears if it holds other images.
- *Implementation:* at mount time only the names of the source tree are walked (no file is
  opened), and directories with no `*.iso` anywhere below them are left out. A source
  directory's images are opened, and their master and area TOCs read, when that directory
  is first looked up or listed (in parallel, a few small reads per image); an image that
  fails is left out then. An area's files and tags are made when the area directory is
  first looked into. Symbolic links to files are followed, links to directories are not.
  A directory whose `*.iso` files all turn out not to be SACD images shows up empty.
- *Implementation:* an image directory whose name collides with a source subdirectory gets
  ` (2)`. A track that fails scarletbook's `check_track` (empty, too many channels, or past
  the area's capacity) is left out. Names are cut to fit 255 bytes.
- `Stereo` and `Multichannel` appear only for areas the disc has.
- File names come from the track text (title), with `/`, NUL and control characters
  replaced; a track without a title is `NN.dsf`. Duplicates get ` (2)`, ` (3)`.
- Every inode number is stable for a mount's lifetime. Sizes are exact and known from the
  TOC alone.

**Hybrid discs.** sacdfs shows only the SACD audio areas of an image. The CD (Red Book)
layer of a hybrid disc is not in SACD images at all, because SACD rippers read only the
high-density layer; CD rips are ordinary files outside sacdfs. The disc's hybrid flag is not
shown either: no consumer needs it, and it would add a tag no player uses. Images without an
SACD master TOC are skipped, as above.

### Flattening (`--flatten`)

Libraries often keep an image in a rip folder inside a release folder, so a disc sits
several folders below its artist. With `--flatten` (off by default, so existing mounts keep
their paths), a chain of source directories whose only content is one image is shown as
its outermost directory, which then holds the image's `Stereo` and `Multichannel` folders:

```
Artist/Year - Album/Artist - Album (rip)/Album/disc.iso
  -> Artist/Year - Album/Stereo/01 - Title.dsf      (was .../Album/disc/Stereo/...)
```

- "Only content" means after the usual hiding: other files, and folders with no `*.iso`
  below them, don't count. A folder that holds an image and another folder with images, or
  two images, is kept; a chain inside it can still collapse.
- The top-level directories of SOURCE (usually artists) are never collapsed, so an artist
  with one album keeps both folders. A single image file as SOURCE is unaffected.
- Collisions get ` (2)` as before (the collapsed folder keeps its own name, so it can
  collide only where an image of the same stem sits next to it, exactly as without the
  option). Inode numbers stay stable for the mount's lifetime.
- *Implementation:* the decision is made from the names walked at mount time
  (`SourceDir::single_image`, a walk down the chain already in memory), when the parent
  directory is listed. No image is opened for it, so mounting costs the same with and
  without the option, and listing a folder of collapsed albums opens nothing: the
  collapsed folder (`DirKind::Flat`) opens its image when it is first looked into, exactly
  when the image's own folder would have been, and is then listed as that folder. The
  price: an `*.iso` that turns out not to be an SACD image still counts as the chain's
  image, so its collapsed folder shows up empty, as a folder of such files does without
  the option.
- *Why these rules* (issue #13): always flattening would change paths for existing users;
  collapsing the top level too would drop the album name of a single-album artist; naming
  folders from the TOC's text is a separate option if ever wanted, since disc titles are
  often less descriptive than the owner's folder names. Deciding from the TOCs (for
  example, not counting files that are not SACD images) would mean opening every image at
  mount, which is the cost the lazy tree exists to avoid.

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
- *Implementation:* album fields fall back from the album text to the disc text (artist,
  title, publisher, copyright). TCON is the track's genre, else the first album genre, and
  only names of the general genre table (not "not used"/"not defined"). TDRC is `YYYY`,
  `YYYY-MM` or `YYYY-MM-DD` as far as the stored date is set. NUL characters are dropped
  and empty values leave their frame out; a track with no text at all has no tag and a
  zero metadata pointer. Channel types: 1 mono, 2 stereo, 3 three channels, 4 quad, 5
  five channels, 6 5.1.

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
starting from the nearest known point before it, else by binary search over the
area's sectors (each probe reads a handful of sectors), aimed with the track start LSNs of
the area TOC. Sequential reads continue the open file's demultiplexer state, so a straight
read never searches.

*Implementation of the locator.* An index point (n, L) means: a fresh demultiplexer started
at L first yields a frame numbered at most n, and frame n starts at or after L, so starting at
L never misses frame n or any later frame. Points come from two places. A *probe* of the
search starts a demultiplexer at L and records the first frame it yields (reading at most 80
sectors). A sequential scan records, at most once a second of audio, the second frame popped
after pushing a sector: every frame delivered by a push except the first started in that
sector. A search for frame f starts from the nearest point numbered below f (scanning
forward directly when it is at most 48 frames back), else it searches, between the nearest
points around f (the area's first sector and its play time when there are none), for the
last sector where a probe yields a frame numbered below f, until 8 sectors are left. It
never starts at a point numbered f itself: a hostile disc can store a second frame with an
earlier frame's timecode, which a scan from the area start discards because it has seen the
original, but which a demultiplexer started between the two takes for frame f (corpus cases
`dst-features-*`, where the frame stored for position 36 repeats timecode 35). Starting one frame earlier lets the demultiplexer see the
original first. The probes interpolate on frame numbers between the tightest of the known
bounds and the track starts of the area TOC that lie inside them (the TOC is only a hint:
every probe is checked, and TOC starts that are out of order are ignored), aimed half the
8-sector stop distance past the estimate, towards the farther bound, so that a good estimate
closes the interval from both sides; when an interpolated probe does not halve the
interval, the next one bisects. A read 375 frames or fewer ahead of the
open file's demultiplexer continues it instead. If two points ever contradict each other
(frame numbers that decrease as the LSN grows; a well-formed disc never does this, a hostile
one can), the area is marked irregular, its points are dropped and every later search
starts at the area's first sector: exactly the sequential scan, only slower. A cold seek
(a fresh mount, so an empty index) costs 4.6 to 6.1 calls to the image on average on real
discs, including reading the frames it was looking for, a millisecond or two from a local
disk; on a network share each call is a round trip. `Disc::sector_reads` counts them.

**Why not scarletbook's frame ranges.** scarletbook 0.3.0 can decode any frame range of an
area (`Disc::range_audio`, `range_audio_cursor`, `continue_range_audio`,
`TrackCursor::for_range`), with exactly the frames, fills and timeline of a scan from the
area start, because that is what it does: a range cursor always starts at the area's first
audio sector and demultiplexes forward to the range, and it exposes no LSNs. Measured on a
50-minute DST stereo disc (page cache; see [performance.md](performance.md)):

| | scarletbook range cursor | sacdfs locator |
|---|---|---|
| first 14 frames at 16 % / 42 % / 72 % of the area | 21-35 / 45-78 / 90-132 ms | 0.9 to 1.8 ms |
| image sectors read for that seek | every audio sector before the target | 5 to 6 calls of 16 to 64 sectors |
| straight read, one fresh range cursor per track (scan part only) | 0.58 s (7.2 times the area's sectors) | 0.08 s (each sector once) |

A range seek grows with its position in the area and on a network share would read up to the
whole area (more than 1 GB) before its first byte, far past the 100 ms target. It also cannot
seed or replace the learned index, since it reports no sector positions. For straight reads
a range cursor per open file would rescan everything before its track, and sacdfs's own
cursor already continues across reads; a hybrid would add code for no gain. So sacdfs keeps
its locator, which is what made the duplicate-timecode case above visible, and keeps checking
it against the corpus, where the range API's results are the reference. A range cursor that
could start from a caller-supplied (frame, LSN) point would let sacdfs drop its own cursor;
that is a possible request to scarletbook, not something sacdfs needs.

*Lockstep DST decoding.* scarletbook 0.3.0 also added `DstDecoder::decode_many`, which
decodes two to four frames together on one thread. Grouping the pool's frames that way (always,
or only when there are enough frames for every thread) made no measurable difference to a
straight read through the mount (within the run-to-run noise of about 5 %): a FUSE read needs
only a few dozen frames at a time, so longer jobs mostly delay the reply. sacdfs keeps one
frame per job.

**Read-ahead.** When reads on a file are sequential, sacdfs decodes the next window of
frames (default 2 s of audio) on the worker pool while the current read is answered, so
a player reading in real time, or `cp`, never waits for the decoder after the first read.
*Implementation:* a read counts as sequential when it starts at most 1 MiB after, or 8 MiB
before, the furthest byte read so far (the kernel sends its read-ahead requests
concurrently, slightly out of order). The next window (150 frames) is queued when the read
reaches the middle of the current one; it runs on a request thread and holds the open
file's demultiplexer, so a read that needs those frames waits for them instead of decoding
them twice.

**Kernel caching.** The filesystem is read-only, so entries and attributes are cached for
a long time, files are opened with `keep_cache`, and the kernel's page cache serves
repeated reads. Large reads (`max_read` 1 MiB) and kernel read-ahead are enabled.
*Implementation:* entry and attribute TTLs are 24 hours; the kernel read-ahead is set to
1 MiB and up to 64 background requests are allowed. The filesystem is mounted `ro`,
`nodev`, `nosuid`, `noexec`.

## Concurrency

fuser dispatches requests on one thread. Reads are answered from a thread pool: the
request's `ReplyData` moves to a worker, which decodes or copies and replies, so slow
reads on one file don't block `lookup`/`readdir` or reads of other files. Each image has
one open handle; sector reads are serialised per image (a `Mutex<Image>`), decoding is
not.

*Implementation:* fuser 0.18 can run several dispatch threads; sacdfs uses 4, so that a
directory listing that opens many images does not hold up other requests. Reads go to a
plain pool of request threads (as many as decoder threads, at least 4 and at most 16), and
DST frames are decoded on a separate rayon pool (`--threads`), one `DstDecoder` per thread.
The request pool is deliberately not a rayon pool: a rayon worker that waits for another
pool runs other jobs of its own pool meanwhile, and a second read of the same file would
then wait, on the same thread, for the demultiplexer lock the first one holds. The image
file is opened on its first audio read, and refused if its size changed since the TOC was
read. Every read runs under `catch_unwind`, so even an unexpected panic becomes an I/O
error on that read.

## Performance basis

scarletbook 0.3.0 decodes a stereo DST frame in about 0.17 ms on one core with its lockstep
decoder (about 80x real time per core; 0.2.0 took about 0.215 ms) and scales to about 13.7x
at 16 threads; plain DSD areas need no decoding.
A straight read through the mount should therefore be limited by the copy and FUSE
overheads, not by decoding. Measured figures are in [performance.md](performance.md).

## Testing

- Unit tests: DSF header and tag layout, offset-to-frame mapping (including the padded
  last block and every channel count 1 to 6), name sanitising.
- Integration tests against the synthetic CC0 conformance corpus of
  [sacd-spec](https://github.com/aclater/sacd-spec) (copied into `tests/corpus/` with its
  licence): mount, list, read every file, compare per-channel hashes with the corpus
  manifest; random-offset reads equal the same slice of a straight read.
- Fuzzing the image layer is scarletbook's job; sacdfs fuzzes its own offset mapping with
  proptest.
- *Implementation:* `tests/corpus.rs` reads every case through the tree and readers without
  mounting, twice: with the default search, and with a search that runs on every read down
  to one sector and no frame cache, so that the locator is exercised on the small corpus
  images too (hostile timelines included), and reads every frame of every file alone from a
  fresh reader. `tests/mount.rs` mounts the corpus with the
  binary, reads every file through the kernel, then remounts and compares scattered reads
  on fresh opens; it also checks background mode, read-only refusal and unmounting by
  SIGTERM and SIGINT. Locally, the same per-channel hashes were compared on real discs
  (see [performance.md](performance.md)).

## Prior art

ratarmount (MIT; lazy archive indexing, seek points), squashfuse (BSD), fuser's examples
(MIT), libfuse's passthrough_hp (big reads, multithreading), Vangoor et al., "To FUSE or
Not to FUSE: Performance of User-Space File Systems", FAST 2017. GPL/LGPL FUSE archive
filesystems (fuse-zip, archivemount, mount-zip, rar2fs). sacdfs is GPL-3.0-or-later, so
code from compatibly licensed projects may be adapted, with attribution (see
CONTRIBUTING.md); SACD parsing and DST decoding still come only from scarletbook.
