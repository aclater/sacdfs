# sacdfs design

sacdfs is a read-only FUSE filesystem that shows the audio of Super Audio CD disc images
as ordinary DSF files, and as WAV files (24-bit PCM) next to them, decoded and converted on
demand. Point a player, a tagger or a file manager at
the mount point and every track of every disc is there, with tags, without extracting
anything first.

## Goals

1. **Correct audio.** A track read through the mount is bit-identical to the same track
   extracted with a reference extractor (pysacd's `pysacd-extract`, or scarletbook's
   `bench decode --sink raw`), checked by per-channel SHA-256 of the DSD stream. A WAV
   file's audio is exactly what the converter makes of the track's frames, each with its
   neighbours in the area, whichever reads asked for it.
2. **Fast.** Reading a track straight through runs close to the decoder's own speed:
   within 1.5x of `bench decode` wall time at the same thread count. A seek (a read at an
   arbitrary offset) returns its first bytes in under 100 ms on a desktop machine.
3. **Cheap to mount.** Mounting a library of hundreds of discs reads only master and area
   TOCs, lazily, when a directory is first listed. Nothing is decoded until a file is read.
4. **Flat memory.** A bounded cache of decoded frames, whatever the library size or the
   number of open files.
5. **Safe on hostile images.** Every image is untrusted input. A bad image shows up as a
   missing directory or an I/O error on one file, never as a crash of the mount.

Not goals: writing, DSDIFF (DFF) output, FLAC output (deferred, #10), DoP, the 48 kHz PCM
family, PS3 network sources, macOS/FreeBSD (Linux first; fuser supports both later). WAV
output was added after 0.1 (see [WAV files](#wav-files)); the plan and every decision behind
it are in issue #11.

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
      01 - <title>.wav
      02 - <title>.dsf
      02 - <title>.wav
    Multichannel/
      01 - <title>.dsf
      01 - <title>.wav
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
- DSF and WAV files side by side in the same folder (owner decision, #11): `--format dsf`,
  `wav` or `dsf,wav` (the default) selects which appear. **Why:** one mount and one tree, no
  duplicate folders to browse or configure; players and taggers handle two files with the same
  name and different extensions. Listing, `stat` and tag reads cost nothing for the extra
  files (below), and `--format` is there for library scanners that would otherwise convert
  every WAV file.
- *Implementation:* a track's files are made one after the other, DSF first, through the same
  de-duplicator, so a track's two names always differ only in the extension, ` (2)` suffixes
  included.
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

## WAV files

A WAV file is a RIFF/WAVE file with a `WAVE_FORMAT_EXTENSIBLE` fmt chunk (24-bit PCM, the
channel mask below, the PCM subformat), padding, the data chunk, and then the tags:

```text
RIFF <size> WAVE   fmt <40>   JUNK <zeros>   data <frames x S x C x 3>   LIST <INFO ...>   id3 <ID3v2.4>
```

- **Rate and depth** (owner decisions, #11): `--pcm-rate` 176 400 Hz by default, or 88 200 or
  352 800; always 24-bit. **Why 176.4 kHz:** it is DSD64 divided by exactly 16, so the
  conversion needs no fractional resampling (cheapest and cleanest), and it holds the 40 kHz
  band with room for the filter's transition. **Why 24-bit only:** it needs no dither, so the
  output stays deterministic and seekable (a frame's PCM never depends on what was converted
  before it). 44.1 kHz/16-bit would need both.
- **Filter and level** (owner decisions, #11; the converter is #5): band-limited to 40 kHz by
  default, 30 kHz with `--pcm-cutoff 30`, following Daniel Weiss; `--pcm-gain` from -60 to
  +24 dB, 0 by default, because 100 % DSD modulation is PCM full scale, so 0 dB never clips.
  A sample that would clip with a gain is clamped, and the log says once per file how many
  samples were clipped. *The range* guards against typos: +24 dB clips almost any disc, and
  below -60 dB the music is gone.
- **Samples per frame** S: an SACD frame is 1/75 s, so S = 1176, 2352 or 4704 at the three
  rates. Data byte `d` lies in frame `d / (S x C x 3)`; frames are stored one after the other,
  each as the converter writes it (interleaved samples, little-endian). The data length is
  `frames x S x C x 3`, so every size is known from the TOC alone.
- **RF64:** when the file does not fit RIFF's 32-bit sizes (over 4 GiB), it starts `RF64`,
  with a `ds64` chunk after `WAVE` holding the 64-bit RIFF size, data size and sample count,
  and both 32-bit sizes set to 0xFFFFFFFF. **Why only then:** plain RIFF is what every program
  reads; RF64 (EBU Tech 3306) is read by FFmpeg, sox, libsndfile and most current software but
  not by everything (Python's `wave`, for one). Stereo at 176.4 kHz reaches 4 GiB after about
  67 minutes, six channels after about 22, so long multichannel tracks need it. The decision is
  made per file, from its size. *Implementation:* no `JUNK` placeholder is written, since the
  size is known before the file is made.
- **Channel mask and order:** 1 FC; 2 FL FR; 3 FL FR FC; 4 FL FR BL BR; 5 FL FR FC BL BR; 6 FL
  FR FC LFE BL BR. This is the order of the DSF files' channel types, and also the order of the
  mask bits, so the samples need no reordering. **Provisional:** it comes from the channel
  count alone. scarletbook 0.3.0 exposes the area's loudspeaker configuration only as the raw
  `area_config` byte, and sacdfs keeps no SACD format knowledge of its own (see
  [Engine](#engine)), so it does not decode that byte; a typed channel assignment is requested
  from scarletbook (scarletbook-rs#75), and the code carries a `TODO(scarletbook#75)`.
- **Tags:** the same ID3v2.4 tag as the DSF file, in an `id3 ` chunk (FFmpeg, TagLib, mutagen,
  foobar2000 and mp3tag read it), and a `LIST`/`INFO` chunk for software that reads only that:
  INAM (title), IART (performer), IPRD (album), ITRK (the track number alone), ICRD (date),
  IGNR (genre), from the same values as the ID3 frames, UTF-8 and NUL-terminated. A chunk with
  nothing to hold is left out. **Why after the audio:** so that the header, and with it the
  offset of every audio byte, does not depend on the text, as in the DSF files. FFmpeg and
  libsndfile read chunks after the data.
- **Padding, so that tag reads never convert.** The `JUNK` chunk makes the audio start at
  256 KiB or a little later (less than 4 KiB more) and end on a 4 KiB page boundary. **Why:**
  the kernel reads a FUSE file in whole pages, and the first read of a file asks for up to twice
  the request: 16 KiB for a small read, 256 KiB for the 128 KiB that Python 3.14's buffered
  files read (mutagen, beets). Without padding, the first page holds audio (it would start at
  byte 68), so a tagger reading the header made sacdfs locate, decode and convert the track's
  first frames; and the page with the end of the audio held the start of the tags. Measured on
  the five local discs, with a fresh mount: reading the first 200 bytes of each WAV file made
  1.1 MB of image reads per file before, 0 after; reading the tags, about 240 KB per file
  before, 0 after. The cost is 256 KiB of zeros per file, about 0.05 % of a typical track. The
  padding takes up the ds64 chunk in RF64 files, so the audio offset is the same for both.
  *What padding cannot avoid:* a tagger that walks the chunks (mutagen, TagLib) reads the 8-byte
  header of the data chunk, which RIFF puts right before the audio, and the kernel then reads
  that page and about 16 KiB after it. So such a tagger converts the track's first one or two
  frames: measured, about 3 ms per file with the stub converter, and the image reads of one
  cold seek (about 1.1 MB per file, the same as reading a DSF file's header).
  *Not for DSF:* a DSF file's chunk layout is fixed (the audio starts at byte 92), so reading a
  DSF file's header still decodes its first frames. Reading its tag decodes almost nothing.
- Header and tag bytes are built when the area directory is listed, like the DSF files', so
  `stat` and `ls -l` never read the image, and reading the first 256 KiB or the tags of a WAV
  file decodes and converts nothing.

## Reading a range of a WAV file

1. Header and tag bytes come from the per-file buffers.
2. Audio bytes map to frames: data byte `d` → frame `d / (S x C x 3)`. A read of 128 KiB of
   stereo at 176.4 kHz covers about 10 frames.
3. **Neighbours.** The converter (#5) is stateless per frame: the PCM of timeline position `t`
   is computed from DSD positions `t - 1`, `t` and `t + 1` (its filters span less than half a
   frame). At a track's first and last frames the neighbours are the area's positions next to
   it, from the previous or next track or the pause between them, so tracks join exactly as
   they would in one stream of the whole area (gapless). Only at the area's edges is a
   neighbour silence: before position 0, and at or after the *area end*, the later of the
   area's play time and the end of its last track. *Why the later:* the play time is the area's
   play range, but a TOC whose last track runs past it is still shown, and inside a track every
   frame has its real neighbours.
4. **Cache.** Converted frames go into the same byte-bounded LRU as the decoded DSD frames,
   keyed by (disc, area, position, kind), so `--cache-mb` is one budget for both. **Why one
   budget:** a mount that serves only WAV files spends all of it on PCM, and memory stays
   bounded whatever the number of open files. The DSD frames a conversion reads are not
   cached (the PCM frame is what is served again).
5. **Random reads.** A read of k frames that are not cached decodes k + 2 DSD frames (or
   fewer at the area's edges) and converts the k frames in parallel on the decoder pool.
6. **Sequential reads.** Each open file keeps, next to its demultiplexer, the last two DSD
   frames it decoded (*carry*): the previous neighbour and the first frame of the next read.
   A read that continues the file therefore decodes every DSD frame once. Read-ahead (2 s of
   audio, as for DSF) decodes and converts ahead on the pool and keeps the carry up to date.
7. **Converters.** One `Converter` per pool thread and channel count, made on first use and
   kept in a thread-local, keyed by engine as well so that two engines in one process (tests)
   never share one. Conversion always runs on the pool, never on a request thread.

*Why convert per frame, with neighbours, rather than as a stream:* any byte of any file can be
produced from three DSD frames, with no state carried from earlier reads, so a seek costs the
same as for DSF plus two frames, and a frame's PCM is the same whichever read or file asked for
it. That is what makes the results cacheable and testable byte for byte.

## Reading a range of a DSF file

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
Only reads that cover audio start or extend it: before the WAV work, a read of the header at
offset 0 counted as the start of a sequential read, so every tagger opening a file made it
decode (and, for WAV, convert) its first 2 s in the background.
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
DST frames are decoded, and frames converted to PCM, on a separate rayon pool (`--threads`),
one `DstDecoder` and one `Converter` per channel count per thread.
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
- *WAV files:* `tests/wav.rs`, every check written against the converter's API only, so that it
  holds for any converter whose output for a frame depends only on that frame and its two
  neighbours. At every rate, every WAV file of every corpus case parses (RIFF, fmt, data,
  INFO, ID3), has the exact size, channel mask and sample count, carries the DSF file's tag, reads
  the same in odd chunk sizes, and holds exactly its positions of a reference stream of the whole
  area (scanned from the area's first sector and converted frame by frame with a fresh
  converter); the area's files concatenated equal the same positions of that stream (gapless).
  Random ranges (proptest) and every frame on its own, with the exhaustive search and no cache,
  equal the straight read. Listing and reading the header and tags of every file reads no audio
  sector and fills no cache; a sequential read makes about as many calls to the image as the DSF
  file's. `tests/mount.rs` reads every WAV file through the kernel and compares it with the
  library, and checks `--format` and `--pcm-*`. An ignored test, `real_discs`, runs the stream
  check on local real discs (`SACDFS_REAL_IMAGES`).

## Prior art

ratarmount (MIT; lazy archive indexing, seek points), squashfuse (BSD), fuser's examples
(MIT), libfuse's passthrough_hp (big reads, multithreading), Vangoor et al., "To FUSE or
Not to FUSE: Performance of User-Space File Systems", FAST 2017. GPL/LGPL FUSE archive
filesystems (fuse-zip, archivemount, mount-zip, rar2fs). sacdfs is GPL-3.0-or-later, so
code from compatibly licensed projects may be adapted, with attribution (see
CONTRIBUTING.md); SACD parsing and DST decoding still come only from scarletbook.
