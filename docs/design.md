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

The pin is tag v0.4.1. 0.4.0 and 0.4.1 changed no decoded output (the corpus hashes and the
real-disc gates are unchanged) and needed no code change to build. Of what they add, sacdfs
uses the loudspeaker layouts ([Speaker layouts](#speaker-layouts)) and positional reads
(`ReadAtReader` over an `Arc<File>`, cloned per read, so that reads of one image run at once;
see [Concurrent reads of one image](#concurrent-reads-of-one-image)). Its seek indexes and
cold seeks (`SeekPolicy::Speculative`) were measured against sacdfs's own frame locator (#28)
and are not used for reading: see [Why not scarletbook's
seeking](#reading-a-range-of-a-dsf-file). The tests use its seek index as an exact reference. sacdfs builds scarletbook
without its `parallel` feature and decodes on its own worker pool, so the larger window of a
parallel library cursor (512 positions, about 48 MB) does not apply; **why:** memory stays
bounded by `--cache-mb` however many files are open.

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
- Inode numbers say what an entry is and are the same on every mount (see
  [File identity and NFS export](#file-identity-and-nfs-export)). Sizes are exact and known
  from the TOC alone.

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
  option). A collapsed folder has its image's inode number, so the image, its areas and its
  files have the same numbers with and without the option; only the folders that collapse
  away (or come back) differ.
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

## File identity and NFS export

Issue #18, for the server-side deployment (#17): an NFS client keeps file handles, which hold
the inode and generation numbers, across server restarts. Numbers handed out in walk order
changed on every mount, and clients got `ESTALE` on open files and cached directories after
every NAS reboot or app update.

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
  is already a dependency, and hashing every name walked costs little at mount (below). A
  unit test pins one number so that an accidental change of the scheme fails.
- *Why 48 bits of key:* 16 bits are needed for the entries; with 48 bits, a collision
  becomes likely only around 16 million paths, and is detected anyway.
- **Collisions** are detected at mount, where every key is computed from the names walked
  (`Ids::new`). The path that sorts first (domain, then bytes) keeps the key; every other one
  takes the first free key of the hash of its path with an attempt number appended (1, 2,
  ...), free meaning no path's first key and not yet handed out. This does not depend on the
  walk order, but a fallback key can change on a later mount if a new path's first key takes
  it. A second area in the same slot (only a hostile image has one) is not shown, rather than
  given a number that is not its own.
- **What changes the numbers:** renaming or moving a folder or image changes its number and
  everything below it. `--flatten` changes only the folders that collapse away. `--format`
  hides files without changing the others'. `--pcm-rate`, `--pcm-cutoff` and `--pcm-gain`
  change the WAV files' 4 flavour bits (so usually their numbers) and always their
  generations. Adding or removing an image changes no other number, except in the
  collision case above.

**Generation numbers.** A track file's generation is 32 bits (all the kernel keeps) of a hash
of its image's sector count and mtime, its format, size and an output version per format, and
for WAV the full PCM flavour; never 0. A handle to a file whose bytes may have changed then
fails with `ESTALE` instead of serving other audio. The output versions (`DSF_VERSION`,
`WAV_VERSION`) are bumped by any change that makes the same track's bytes differ.
Directories have generation 0: they are containers found by name, and a handle to one whose
content changed should still work (a missing area or file is then not found).
*Why not the image's file size:* the sector count is the image's size in sectors, already in
the TOC; together with the mtime it catches any rewrite, without another `stat` per lookup.

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

**Stopping a busy mount.** On SIGINT or SIGTERM, a mount that cannot be unmounted (files open,
or held by an NFS server) is detached (`MNT_DETACH`, or `fusermount3 -u -z`) with a warning;
sacdfs serves the files still open and exits when the last is closed. *Why:* before, as root, the
unmount failed with `EBUSY`, sacdfs logged it and kept serving, and a container or service
manager waiting for it to stop hung until it killed it, leaving a dead mount behind; for users
other than root, fuser already unmounted lazily, so this makes the two behave the same.

**Cost.** Mounting now hashes every source directory and image name once and keeps one map
entry per directory and image (tens of bytes each). The node table is a hash map by inode
number instead of a vector. Lookups of files compute their generation (one SHA-256 of about
70 bytes, well under a microsecond). Measured in [performance.md](performance.md).

## DSF files

A DSF file is the DSD chunk (28 bytes), the fmt chunk (52 bytes), the data chunk (12-byte
header, then audio), and an ID3v2.4 tag at the end, pointed to by the DSD chunk.

- Audio: the track's timeline positions `[start_frame, end_frame)` from the area TOC (the
  gaps between tracks are not included), each decoded to `C x 4704` bytes. DSF stores each
  channel in 4096-byte blocks, channel after channel, earliest sample in the least
  significant bit (bits per sample = 1). The last block of each channel is padded with
  zeros. Sample count per channel = frames x 4704 x 8.
- Channel type in fmt: from the area's loudspeaker layout (stereo: 2; 5-channel 3/2: 6, "5
  channels"; 5.1: 7), else from the channel count; see [Speaker layouts](#speaker-layouts).
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
  zero metadata pointer. Channel types by channel count, for an area without a defined
  layout: 1 mono, 2 stereo, 3 three channels, 4 quad, 5 five channels, 6 5.1.

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
  default, 30 kHz with `--pcm-cutoff 30`, following Daniel Weiss.
  `--pcm-cutoff wide` (about 80 kHz) is offered at 176.4 and 352.8 kHz only. **Why the option:**
  Philips listening tests (Nuijten and Reefman, AES paper 5399, 2001) found wideband filters at
  80 or 100 kHz undetected where 50 kHz was detected, and other sources (Reefman and Janssen,
  Stuart, Craven, JAES 2004) favour a wider channel. **Why not the default:** the owner keeps
  Weiss's 40 kHz, which keeps DSD's ultrasonic noise out of unknown playback chains; the
  listening tests don't answer that concern. **Why refused at 88.2 kHz:** that rate holds
  only up to 44.1 kHz, so sacdfs refuses `wide` with `--pcm-rate 88200` at startup, with a
  message, instead of mounting a filter that cannot be what was asked for. The wide filter is
  not longer, so it costs nothing in speed. `--pcm-gain` from -60 to
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
- **Channel mask and order:** from the area's loudspeaker layout, like the DSF channel type;
  see [Speaker layouts](#speaker-layouts). Stereo 0x3 (FL FR), 5-channel 3/2 0x37 (FL FR FC BL
  BR, which FFmpeg reads as "5.0") and 5.1 0x3F (FL FR FC LFE BL BR); without a defined layout,
  the mask of the channel count.
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


## Speaker layouts

The WAV channel mask (`dwChannelMask`) and the DSF channel type come from the area's
loudspeaker layout, which scarletbook (0.4.0 and later) gives as `Area::loudspeaker_layout`
(from the channel count and the low 3 bits of the area configuration byte, specification 3,
3.4.1) with the speaker of each channel in decoded-frame order (`Area::channel_speakers`):

| Layout | Speakers, in decoded order | WAV mask | DSF channel type |
|---|---|---|---|
| Stereo (2 channels, configuration 0) | FL FR | 0x3 | 2, stereo |
| 5-channel 3/2 (5, configuration 3) | FL FR C SL SR | 0x37 (FL FR FC BL BR) | 6, 5 channels |
| 5.1 (6, configuration 4) | FL FR C LFE SL SR | 0x3F (FL FR FC LFE BL BR) | 7, 5.1 channels |
| Anything else (`Unspecified`) | not defined | by channel count | by channel count |

- **Why from the disc, not the count:** the count only guesses which speakers a disc meant;
  the layout is what the disc states. sacdfs keeps no SACD format knowledge of its own (see
  [Engine](#engine)), so it reads the layout through scarletbook's typed API instead of
  decoding the configuration byte itself. Before 0.4.0 scarletbook gave only the raw byte, and
  sacdfs used the count alone (requested upstream as scarletbook-rs#75).
- **No reordering:** a WAV file's channels must follow the order of their mask bits, and the
  DSF types fix the order FL FR C (LFE) SL SR. scarletbook's decoded-frame order is that
  order for all three layouts (its order is informative, the order existing tools label
  extracted channels in), so the samples are written as decoded. A unit test checks that the
  speakers' mask bits rise in decoded order, so a change upstream would fail it.
- **Surround on the back bits (BL BR), not the side bits:** what 5.0 and 5.1 mean to most
  players and to FFmpeg (0x37 is its "5.0", 0x3F its "5.1"; the side bits give "5.0(side)"
  and "5.1(side)"), and what sacdfs wrote before. The DSF types have one surround pair, so
  they do not distinguish.
- **`Unspecified`** (any other combination: 1, 3 or 4 channels, or a stereo or 5-channel
  count with another configuration; seen only on synthetic test images so far) keeps the
  layout of the channel count, as before: 1 FC; 2 FL FR; 3 FL FR FC; 4 FL FR BL BR; 5 FL FR
  FC BL BR; 6 FL FR FC LFE BL BR (DSF types 1, 2, 3, 4, 6, 7). **Why:** the disc does not say
  which speakers it means, the count gives players a usable default, and a file is always
  shown. sacdfs logs it at Info once per disc, when the image is opened, naming the areas
  and their stored fields, so that a surprising mask can be traced to the disc.
- **Checked:** a test reads the header of every file of every corpus case and compares the
  mask and type with the manifest's channel count and configuration (written out in the
  test, not taken from sacdfs or scarletbook): stereo, 3/2 and 5.1, and six unspecified
  combinations. On the real discs, FFmpeg's `ffprobe` reads the stereo, 5-channel and 5.1
  areas as "stereo", "5.0" and "5.1" in both formats.

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
   *Pipeline:* one pool thread demultiplexes plain DSD frames 16 at a time, and each PCM
   frame is handed to the other pool threads as soon as its next neighbour is decoded. Once it
   meets a DST frame, the rest of the range is demultiplexed and decoded in one parallel batch
   first: decoding 16 DST frames at a time left most threads idle and made DST reads 1.8 to 1.9
   times DSF's instead of 1.3. **Why:** on a plain DSD area there is no DST
   decoding, so demultiplexing (serial) and conversion (parallel) took turns; overlapping them
   cut a straight read of a plain DSD stereo area from 3.28 s to 2.90 s (generic build, mean of
   3), before the longer read-ahead below.
6. **Sequential reads.** Each open file keeps, next to its demultiplexer, the last two DSD
   frames it decoded (*carry*): the previous neighbour and the first frame of the next read.
   A read that continues the file therefore decodes every DSD frame once. Read-ahead decodes
   and converts ahead on the pool and keeps the carry up to date.
   - *Window:* 2 s (150 frames), as for DSF; for a reader that has read at least 16 MiB at more
     than 4 times the file's byte rate (`cp`, a scanner), about 8 MiB of PCM, between 150 and
     600 frames (8 s of stereo at 176.4 kHz, 2 s of six channels at 352.8 kHz). **Why:** a fast
     reader of a plain DSD area used up a 2 s window before the next was ready; the longer
     window took the same read from 2.90 s to 2.29 s, within the 1.5x target of #6 (DSF:
     1.65 s). **Why only for fast readers:** with it for every reader, four real-time players
     and a tagger saw reads of up to 114 ms (target 50 ms), because players then convert in
     long bursts on the shared pool. Bounded in bytes, so that six-channel files at 352.8 kHz
     don't take 50 MB per reader.
   - *Not on the first audio read:* the first read of an open file that covers audio starts no
     read-ahead; the next one does. **Why:** a tagger walking a WAV file's chunks reads the
     header, then the data chunk's header 256 KiB later, which touches the first frame and
     looks like a continuing read; it made every tagged file convert seconds of audio (388 MB
     of image reads and 28 s of CPU for 186 files; now 0.16 s). A player or `cp` sends its
     next read at once.
7. **Converters.** One `Converter` per pool thread and channel count, made on first use and
   kept in a thread-local, keyed by engine as well so that two engines in one process (tests)
   never share one. Conversion always runs on the pool, never on a request thread.
8. **Runs of frames (#24).** Frames of the plain DSD pipeline are converted up to four at a
   time, consecutive ones in one job (`Converter::convert_run`, byte-identical to converting
   each alone): a straight WAV read of a plain DSD area took 15 % less time and 29 % less CPU.
   **Why only plain DSD:** its frames arrive 16 at a time from one demultiplexing thread, and
   one job per frame spent much of the conversion's cost on scheduling and on each frame's
   margins; a DST batch arrives all at once with enough frames for every thread, where runs
   of four made reads 5 % slower. **Why four:** two and eight gained less (11 and 12 %), and
   a short job (about 0.2 ms) keeps the 4 players and a tagger under 50 ms per read (#15).

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
sectors). A sequential scan records, at most once every 40 frames (about half a second), the
second frame popped after pushing a sector: every frame delivered by a push except the first
started in that sector; it skips points within 10 frames of a known one, which a stretch read
again from another start would otherwise pile up, but still checks them against the others.
A search for frame f starts from the nearest point numbered below f (scanning forward
directly when it is at most 48 frames back; the 40-frame spacing makes that true anywhere in
a stretch already read), else it searches, between the nearest points around f (the area's
first sector and its play time when there are none), for the last sector where a probe
yields a frame numbered below f, until 8 sectors are left, or about two of the area's
average frames if that is more (at most 32 sectors: scanning them costs no extra call to the
image, a probe does). It
never starts at a point numbered f itself: a hostile disc can store a second frame with an
earlier frame's timecode, which a scan from the area start discards because it has seen the
original, but which a demultiplexer started between the two takes for frame f (corpus cases
`dst-features-*`, where the frame stored for position 36 repeats timecode 35). Starting one frame earlier lets the demultiplexer see the
original first. The probes interpolate on frame numbers between the tightest of the known
bounds and the track starts of the area TOC that lie inside them (the TOC is only a hint:
every probe is checked, and TOC starts that are out of order are ignored), aimed half the
stop distance past the estimate, towards the farther bound, so that a good estimate closes
the interval from both sides. When an interpolated probe does not halve the interval, the
next one bisects, unless the probe found a frame within about twice the stop distance of f:
then the estimate was good and only its near bound moved, so the next interpolation, aimed
at the other side, usually closes the interval. At least every third probe bisects, so a
hostile disc cannot make the search linear. A read 375 frames or fewer ahead of the
open file's demultiplexer continues it instead. If two points ever contradict each other
(frame numbers that decrease as the LSN grows; a well-formed disc never does this, a hostile
one can), the area is marked irregular, its points are dropped and every later search
starts at the area's first sector: exactly the sequential scan, only slower. A cold seek
(a fresh mount, so an empty index) costs 3.0 to 5.7 calls to the image on average on real
discs (3.0 for plain DSD), including reading the frames it was looking for, a millisecond or
two from a local disk; on a network share each call is a round trip, and on a NAS with hard
disks often a disk seek too. `Disc::sector_reads` and `Disc::sector_bytes` count them.

*Sizes of the reads.* Each read of the image is sized to what it is for, from the area's
average frame size (its sectors over its play time). A probe reads about two average frames
and a sector per call (5 to 7 sectors for DST stereo, 11 for plain DSD stereo, 29 for
6-channel DSD; it was 16 sectors for all), which usually finds a whole frame in one call. The
first read of a cursor after a search covers the frames of a 128 KiB read of the DSF file
(the common size of a player's or the kernel's random read), two more frames and the stop
distance; later reads are 64 sectors as before. The cursor also starts with the sectors the
last probe at its starting point already read. *Why:* a cold read used to fetch about twice
the bytes it needed (293 KiB on average for a 128 KiB read; now 139 KiB). On a local disk
that hardly shows; over a network the bytes are transfer time on top of each round trip.

**Persistent seek index.** The points of each image's areas are kept across mounts in a
cache directory (`$XDG_CACHE_HOME/sacdfs`, `--index-cache DIR`, off with
`--no-index-cache`), so that a seek after a remount or a reboot starts from a point instead of
searching: one call to the image instead of four or five. *Why:* client-side mounts (images on
NFS or SMB, issue #20) pay a round trip for every call, plus a disk seek on a NAS whose cache
does not hold that part of the image; a remount used to start every seek cold.

- *Format* (`index_cache.rs`): one file per image, named by the first 128 bits of a SHA-256 of
  the image's canonical path, so that no path appears in the cache; it holds the full hash,
  the image's size and modification time (seconds and nanoseconds), each area's first and
  end LSN and channel count, and the points, delta-coded as LEB128 (about 2 to 3 bytes a
  point; a disc played through takes a few tens of KB, a library of a hundred discs a few
  MB). A format version and a truncated SHA-256 of the contents guard it. A file that does
  not match in every field, or fails any check while being read (points out of order or
  outside their area, more than 65 536 an area, a file over 4 MiB, a symbolic link), is
  ignored and replaced by the next save. *Why the key:* size and mtime change when an image
  is replaced or edited, and checking them costs nothing; hashing the content would read the
  whole image, which is what the cache exists to avoid.
- *Never trusted.* Loaded points are kept apart from the checked ones. They aim the search's
  probes (with the TOC's track starts, whichever is closer), and a cursor may start at one
  within 48 frames before its frame, but it checks the first frame it reads there exactly as
  a probe would (found within 80 sectors, and numbered below the frame wanted) before it
  returns anything. A start that passes is recorded as a checked point; one that fails makes
  the cursor fall back to an ordinary search, and drops every loaded point of the disc. So a
  stale, damaged or hostile cache costs only speed: the audio is always what a scan from the
  area start gives. *Why not check every point at load:* that would cost a probe per point,
  more than the search it saves.
- *Writing.* Lazily: every 30 seconds for discs whose points changed, and at unmount; a
  disc whose points did not change is only touched (its mtime set) so that it counts as
  used. Atomically: a new temporary file (`O_EXCL`, mode 0600, not following links), then a
  rename over the old one. Not synced: a crash can at worst lose or damage an entry, which
  then costs speed. The directory is created on the first write (mode 0700), and bounded to
  32 MiB in all: after a write, the least recently used files are removed until it fits.
  Points that would make the loaded and checked points of an area contradict each other are
  not saved.
- *Why not more:* precomputing the index of a whole library in the background
  (`--index-ahead`) would read every image completely, which on a network share is the cost
  this work avoids; it is a possible follow-up as a sparse, low-priority probe pass.

**Why not scarletbook's seeking** (#28). scarletbook 0.4 can seek within an area itself, two
ways, and sacdfs measured both against its locator before deciding. It keeps its locator for
cold and warm seeks, and its own persistent seek index.

- *A seek index* (`AreaIndex`, opt-in per area): the exact reading state every K sectors
  (default 64), so that a range starts at the last checkpoint before it with results exactly
  those of a scan from the area's first sector (specification 4, 4.2 item 7). But its
  checkpoints run without gaps from the area's first sector: it covers a seek only once the
  area has been read up to it, by playing from the start or by a full pass (about 9 s per GB
  over a network share). A seek after a remount into a part of the area read only by an
  earlier seek, which sacdfs answers with one call to the image from its saved points,
  finds no checkpoint there.
- *Provisional reading* (`SeekPolicy::Speculative`, the cold seek): a search of the frame
  records' timecodes, then a scan from a checkpoint boundary at least 32 sectors before the
  target with the range's first frame as the next acceptable frame number. Its positions are
  provisional: exact on well-formed discs, but on a hostile image they can differ, and a
  reader learns that only when a pass from the area's first sector reaches them (a
  `Refuted` verdict).

*Measured* (performance.md, [Seeking through scarletbook](performance.md#seeking-through-scarletbook-28)),
the frames of a 128 KiB DSF read at 200 positions per area of the five real discs, each on a
freshly opened disc, without FUSE:

| Calls to the image / KiB read per seek | cold | warm |
|---|---|---|
| sacdfs's locator (warm: after a remount, from the saved points of the same seek) | **4.31 / 136** | **1.01 / 103** |
| scarletbook provisional reading, default options | 8.84 / 373 | |
| the same, tuned (exact scan only within 64 sectors, checkpoints every 16) | 7.22 / 260 | |
| scarletbook, from a complete exact index (K = 64; K = 16) | | 2.68 / 172; 1.99 / 128 |

On a network share (NFS, every read to the server) a cold seek took 21 to 29 ms through
scarletbook against 7 to 15 ms with sacdfs's locator, and a warm one 1.5 to 1.8 ms against
0.6 to 1.2 ms.

- *Why the cold path costs more:* the library's search makes 3.0 to 5.3 reads of 8 sectors
  before it reads any frame; sacdfs's whole cold seek, frames included, takes 3.0 to 5.6
  calls, its probes sized to two of the area's average frames and aimed with the TOC's track
  starts. The library then starts 32 to 96 sectors before the target (a checkpoint boundary
  at least 32 sectors back) and reads in fixed calls of 32 sectors, where sacdfs's first read
  is one call sized to the frames wanted.
- *Why the warm path costs more:* a start from a checkpoint replays up to 32 sectors and reads
  up to K more before the range, again 32 sectors a call; sacdfs starts within 40 frames of
  the target in one call sized to the read. And the index must have been built first.
- *Why not provisional positions at all:* on corpus case `area-audio-past-end` provisional
  readings return other frames than the exact ones, and are refuted only once a pass reaches
  them (`tests/library_index.rs`). Served through FUSE, those bytes would sit in the kernel's
  page cache and in players' buffers until then; sacdfs's locator gives the exact frames
  there from the first read. Handling refutation (dropping cached frames,
  `notify_inval_inode`) would only repair what was already played.
- *What sacdfs takes from 0.4 instead:* the seek index as an oracle. `tests/library_index.rs`
  reads every frame and random ranges of every corpus case (hostile ones included, default
  and exhaustive search) located by sacdfs, and compares them with reads through a complete
  library index and with a straight library read; on the real discs, straight reads and 300
  random ranges per area. They are equal everywhere, so the locator's frame-number rule, which
  the specification's 4.2 note does not guarantee in general, is checked against an exact
  reader on every image the tests have.
- *Memory:* sacdfs keeps no library index or cursor, so the index's inline budget (1 MiB per
  index by default) and the `parallel` window (up to about 48 MB per cursor) do not apply.
- *What would change this* (requested upstream): cold-search probes sized by the caller or
  the area's frame size, aimed with the TOC's track starts; a start closer than a checkpoint
  boundary 32 sectors back; read calls sized by the caller (a first read covering the frames
  wanted); a warm start that does not need the area read from its first sector, for example
  confirmed provisional checkpoints that can be saved and checked on load. Until then sacdfs
  keeps its locator, the hardest code it has, and the tests above keep it honest.

Before 0.4, scarletbook's only random access was a range cursor that always starts at the
area's first audio sector (`TrackCursor::for_range`, `Disc::range_audio`), measured on a
50-minute DST stereo disc (page cache):

| | scarletbook range cursor | sacdfs locator |
|---|---|---|
| first 14 frames at 16 % / 42 % / 72 % of the area | 21-35 / 45-78 / 90-132 ms | 0.9 to 1.8 ms |
| image sectors read for that seek | every audio sector before the target | 5 to 6 calls of 16 to 64 sectors |
| straight read, one fresh range cursor per track (scan part only) | 0.58 s (7.2 times the area's sectors) | 0.08 s (each sector once) |

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
concurrently, slightly out of order). The first read of audio starts none (see the WAV
files), and neither does a read that ends within 256 KiB of where the first read of audio
started (#32). *Why:* that is the kernel's own read-ahead. On a first read at the start of
a file, the kernel asks for 128 KiB, then for 128 KiB more in the background, whatever the
reader asked for: a tagger reading a DSF file's first 4 KiB gets two "sequential" reads of
audio, and the second started 2 s of decoding. It made a DSF tag walk read 1.17 MB of the
image per file; now 221 KB. A player's next read goes past 256 KiB and starts read-ahead
there, one read later than before, which the measured straight reads and concurrency do not
show.
- *What a DSF header read still costs, and why it stays:* the 256 KiB the kernel reads hold
  the track's first frames (about 28 in stereo), and the first page itself holds 4004 bytes
  of the first frame, so a DSF tag walk reads audio sectors: 3.7 calls to the image per
  file on average, all at the track's start. The options of #32 were measured:
  - *Answer the header from a cache of track-start audio:* the kernel never asks for the
    header alone. With `max_readahead` at 4, 16 or 64 KiB or the default 128 KiB, it still
    asked for 128 + 128 KiB on a 4 KiB read at offset 0; only 0 changed it (4 KiB requests,
    11 times as many for the walk), which would also take read-ahead from every player and
    `cp`. A cache would therefore need about 28 decoded frames per track, about 256 KiB: some
    3 GB for a thousand discs, against the seek index's 32 MiB, and it would not help the
    first walk of a library anyway.
  - *Detect header-only reads and serve the audio part lazily:* the request is the same
    128 KiB whether a tagger asked for 4 KiB or a player for 128 KiB, so there is nothing to
    detect; and a page must be answered whole, since a short reply tells the kernel the file
    ends there. `direct_io` would pass the reader's own size, but turns off the page cache
    and the kernel's read-ahead for every reader.
  - *Accept the rest:* kept, with the read-ahead fix above, which removed four fifths of
    the bytes. The WAV files hold no audio in their first 256 KiB (the `JUNK` chunk), so
    their header and tags still read nothing. The next window (150 frames) is queued when the read
reaches the middle of the current one; it runs on a request thread and holds the open
file's demultiplexer, so a read that needs those frames waits for them instead of decoding
them twice.

**Kernel caching.** The filesystem is read-only, so entries and attributes are cached for
a long time, files are opened with `keep_cache`, and the kernel's page cache serves
repeated reads. Large reads (`max_read` 1 MiB) and kernel read-ahead are enabled.
*Implementation:* entry and attribute TTLs are 24 hours; the kernel read-ahead is asked for
1 MiB (the kernel grants at most its own default, 128 KiB on the test machine, as fuser's
`INIT` log shows) and up to 64 background requests are allowed. The filesystem is mounted `ro`,
`nodev`, `nosuid`, `noexec`.

## Concurrency

fuser dispatches requests on one thread. Reads are answered from a thread pool: the
request's `ReplyData` moves to a worker, which decodes or copies and replies, so slow
reads on one file don't block `lookup`/`readdir` or reads of other files. Each image has
at most one open handle, from a bounded cache of them, and the reads of one image run at the
same time: each takes its own reader of the shared file ([below](#concurrent-reads-of-one-image)).

*Implementation:* fuser 0.18 can run several dispatch threads; sacdfs uses 4, so that a
directory listing that opens many images does not hold up other requests. Reads go to a
plain pool of request threads (as many as decoder threads, at least 4 and at most 16), and
DST frames are decoded, and frames converted to PCM, on a separate rayon pool (`--threads`),
one `DstDecoder` and one `Converter` per channel count per thread.
The request pool is deliberately not a rayon pool: a rayon worker that waits for another
pool runs other jobs of its own pool meanwhile, and a second read of the same file would
then wait, on the same thread, for the demultiplexer lock the first one holds. Every read
runs under `catch_unwind`, so even an unexpected panic becomes an I/O error on that read.

**Open images** (`handles.rs`, #26). The handle an image's TOC was read through is kept,
and every audio read of the disc uses it; a closed one is opened again on the next read,
and refused if the image's size changed since the TOC was read. The handles live in one
least-recently-used cache for the process, bounded by `--max-open-images` (default 64);
a handle unused for 5 minutes is closed by a thread that looks every 30 seconds. A handle
is checked against its path (one `stat`) when it is used, at most once a second: if the
file's size, modification time, device or inode changed, the handle is closed and the
image opened again, as a first open would. A disc that is dropped closes its handle.
- *Why keep handles:* on a network share an open costs a round trip or two (0.6 to 0.9 ms
  over NFS, measured in #25), as much as the one read a seek with a warm seek index needs;
  before, a fresh mount opened every image twice (for its TOC, then for its first audio
  read). The TOC handle is kept because a seek right after a directory listing (a player
  resuming) is the common case.
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
  remount, as before.
- *Why the cache drops handles instead of closing them:* a handle is shared (`Arc`), so a
  read in progress on an evicted handle finishes on it, and the file is closed when that
  read ends. No lock is held while opening a file, and the cache's lock never waits for a
  read, so eviction cannot deadlock or stall a read. The keys are per disc and unique in
  the process (not the disc id, which callers choose).

**Concurrent reads of one image** (#28). A handle is an `Arc<File>` read with positional
reads (`pread`), through scarletbook 0.4's `ReadAtReader`. Each call to the image takes a clone
of the cached `Image` (no I/O, no lock: the same file, a position of its own) and reads
through it, so reads of one disc by several players, a tagger and read-ahead run at once.
Before, each handle was a `Mutex<Image<File>>`, and every read of a disc waited for the one in
progress.
- *Why:* on a local disk a read holds the lock only for a copy from the page cache, but on a
  network share each read is a round trip, and on a NAS with hard disks often a disk seek:
  a tagger's or a second player's read then waited behind the first player's for the whole
  round trip. Measured in [performance.md](performance.md#concurrent-reads-28).
- *Why a clone per call, not one per cursor or thread:* a clone holds the file open, and
  cursors live as long as their open file; per call, the bound on open images
  (`--max-open-images`) still holds between reads, and an evicted handle closes when the last
  read through it ends, as before.
- *Why `Image` clones, not `scarletbook::Disc::clone`:* sacdfs keeps its own TOC, frame
  locator and seek index per disc and reads sectors through an `Image`; a `Disc` adds a parsed
  TOC and shared seek indexes that sacdfs does not use (see [Locating a
  frame](#reading-a-range-of-a-dsf-file) for why), and a reopened file would need its TOC
  checked again. An `Image` clone is what `Disc::clone` does underneath.
- Images in 2064-byte raw sectors are read through a buffer per clone (scarletbook's), so
  each call allocates it again; the straight-read figures show no cost.

## PCM conversion (phase 1: the converter)

PCM output (WAV files next to the DSF files, #11) needs DSD-to-PCM conversion. Phase 1 adds
the converter, `sacdfs::pcm`, and its measurements; no WAV file is shown yet (that is phase
2, #7 and #8). The filter designs and the measurements are in
[pcm-quality.md](pcm-quality.md), the speed in [performance.md](performance.md#pcm-conversion).
Each decision below says why, the alternatives and the trade-off.

**Output.** 24-bit PCM at 176.4 kHz (DSD64 ÷ 16, the default), 88.2 kHz (÷ 32) or
352.8 kHz (÷ 8); a pass band to 40 kHz (default, fully attenuated by 48 kHz), 30 kHz
(attenuated by 36 kHz), or a wide 80 kHz (176.4 and 352.8 kHz only); a gain, 0 dB by
default, where 100 % DSD modulation is full scale. These are owner decisions, recorded with
their reasons in #11 and #5.

**Stateless per frame (why: random reads).** Frame `f`'s PCM is computed from DSD frames
`f - 1`, `f` and `f + 1` only, and equals, bit for bit, a conversion of the whole area in one
pass (`pcm::convert_area`, tested for every frame, 1 to 6 channels, every rate and cutoff,
and by proptest on random windows). Every stage is a linear-phase FIR, so each output sample
depends on a fixed window of DSD bits: at most about 2 200 DSD samples each way (about 270
bytes per channel of each neighbour, the 30 kHz and wide filters being the longest), far
less than a frame. *Alternative:* pysacd's converter carries filter state from frame to
frame, which needs every earlier frame converted first: a seek would cost a conversion from
the start of the area. *Trade-off:* each frame converts its neighbours' margins again, about
8 % more work than streaming.

**Aligned (why: DSF and WAV of a track line up).** PCM sample `n` is centred on DSD sample
`d × n` of the same frame, exactly: every stage has an odd number of taps, so a whole-sample
delay, and stage 1's 8 × 12 - 1 = 95 taps are placed so that their centre is the first bit of
a byte. *Alternative:* pysacd's even-length (96-tap) stage 1 has a half-sample delay, which
it rounds to whole output samples; measured against pysacd, its output is 0.26 samples (at
88.2 kHz) away from the DSD timeline (see pcm-quality.md).

**Silence (why 0x69).** Before the first and after the last frame of an area the filter
reads DSD silence, the byte 0x69 (01101001), the Scarlet Book silence pattern that
scarletbook and sacdfs already use for missing frames. Stage 1's eight polyphase sums are
made equal, which puts exact zeros at every multiple of 352.8 kHz; any byte-periodic
balanced pattern, 0x69 included, then converts to exact digital zero (and after the integer
tables are rounded, the last table's 0x69 entry is adjusted so the sum is exactly 0).
*Alternatives:* zero bytes are -100 % modulation, a full-scale negative step; repeating the
area's first or last frame would invent audio. A track that starts at the area start fades
in from true zero.

**The port (why: proven code, same author).** The structure is ported from pysacd's
`csrc/dsp.c` (GPL-2.0-or-later, the owner's own; commit 2113789, named in the source): the
Kaiser-windowed design, the byte lookup tables of the ÷ 8 stage and the cascade of ÷ 2
stages. What changed: stateless per frame; odd lengths; equal polyphase sums; new tap
counts for the 40/30/80 kHz bands at every rate (pysacd's 176.4 kHz path passes to 60 kHz);
half-band stages where the band allows, which halves their cost; integer stage 1 and f64
accumulation (below). FFmpeg's and dsd2pcm's DSD code were not needed.

**Arithmetic (why: nothing above the 24-bit step, and the same bits everywhere).** Stage 1
adds integer table entries (fixed point, 2^-28): exact. The later stages accumulate in f64.
Measured on random DSD against a plain f64 evaluation of the same filters, f32 accumulation
in stage 3 alone added 0.34 of a 24-bit step RMS, and an f32 stage 2 another 0.15: more
than the 24-bit quantisation itself (0.29). In f64 the difference is below 0.00001 of a
step. Every output is a fixed sequence of operations (Rust never fuses a multiply and an
add, nor reorders float sums), so the SSE2, AVX2 and AVX-512 builds give identical bytes
(a test pins the output's hash for every configuration). *Trade-off:* f64 halves the SIMD
width of the FIR stages; the bar is still met with margin (performance.md).

**Coefficients computed at start-up (why).** A converter designs its filters when it is made:
27 to 42 µs (Kaiser windows, a Bessel series, the stage-1 tables), once per worker thread.
*Alternative:* tables generated by a script and committed, which would need a generator and a
check that the two agree; with start-up design the parameters (taps, attenuation, edges)
are what the code says. The risk, a platform maths library giving different `sin` results,
is covered by the pinned-hash test, which passes on the local RHEL glibc and on CI's
Ubuntu glibc.

**SIMD (why portable code).** The FIR kernels are written so that the compiler vectorises
them (blocks of independent outputs, windows cut to exact lengths so there are no bounds
checks, the generated code checked: packed `addpd`/`mulpd` on generic x86-64, `vaddpd` on
`ymm` registers on x86-64-v3). Stage 1 reads its bytes as words and takes them apart with
shifts (half the loads).

**Runtime CPU dispatch (#23, why).** Generic x86-64 builds, which distributions and
`cargo install` users get, would otherwise run the converter with SSE2 only; the wide filter
then sat near the 100x bar of #6. So stage 1 and the three FIR kernels are compiled three
times, for the baseline, x86-64-v3 (AVX2) and x86-64-v4 (AVX-512), by the `multiversion`
crate (MIT OR Apache-2.0), and the copy is chosen from the CPU when the first converter is
made (`pcm::Simd`). *Why `multiversion`:* calling `#[target_feature]` code needs `unsafe`,
which sacdfs denies outside its daemonising code; the crate's macro keeps that call behind its
own CPU check, so no copy can run on a CPU without its features, and sacdfs keeps
`unsafe_code = "deny"`. *Alternatives:* a local `unsafe` block with `std::arch` (more control,
breaks the policy for no measured gain); shipping x86-64-v3 builds only (excludes older CPUs);
a 112 dB wide filter (about 5 % faster, lower quality).
- *Granularity:* the kernels are dispatched, not the whole channel. Forcing all of a channel's
  code inline into one dispatched function made the baseline path about 10 % slower than
  before; dispatching each kernel (a few calls per channel and frame) leaves the baseline as it
  was.
- *Wider blocks on wider vectors:* each output is a chain of dependent adds, so a block must
  hold enough independent outputs to keep the adders busy: 16 on the baseline, 32 on v3, 64
  on v4 (8 registers of accumulators each). The block size never changes an output's
  operations.
- *v3 by default, even with AVX-512:* measured on a Zen 5 CPU, the v4 copy was slower than v3
  on every filter but one (performance.md), and much slower with both threads of a core
  converting, as the decoder pool does under load. It is built and tested, and
  `SACDFS_SIMD=v4` selects it, so that other CPUs can be measured; the default changes only
  with a measurement. AVX-512 target features need Rust 1.89, so a build with an older
  compiler (the MSRV is 1.85) has no v4 copy.
- *Identical output:* every output is the same sequence of operations on every path (Rust does
  not fuse multiplies and adds, even with FMA enabled, nor reorder float sums). The pinned
  hashes are checked on all three paths in one process (`Converter::with_simd`), and CI runs
  the converter and WAV tests once per `SACDFS_SIMD` value.
- *Building:* RHEL 10's Rust targets x86-64-v3 by default; there every copy has AVX2, and
  `-C target-cpu=x86-64` gives a true baseline (how the figures were measured).

**Interleaved input.** The converter takes frames as scarletbook gives them (`C × 4704`
interleaved, MSB first) and copies one channel at a time into a window buffer with its
neighbours' margins (about 5 KB); the kernels then run on contiguous data. *Alternative:*
reading the interleaved bytes in stage 1 directly; it would lose the word loads, and the
copy costs about 7 % of the conversion.

**Gain and clipping.** The gain is a multiply folded into the final rounding (f64, ties to
even, by the 1.5 × 2^52 trick, which needs no libm call). Samples beyond the 24-bit range are
clamped and counted (`convert` returns the count). 100 % positive modulation is exactly one
step above the largest code, so a full-scale positive DC counts as clipping; a full-scale
step overshoots by about 9 % (the filter's ringing). Real discs stay far below this (SACD's
reference level is 50 % modulation): no clips on the local discs.

**API for phase 2.** `pcm::Converter::new(channels, &PcmConfig)` holds its buffers (about
100 KiB), is `Send`, allocates nothing per frame, and is made once per worker thread.
`convert(prev, cur, next, out)` writes frame `cur`'s interleaved 24-bit little-endian PCM
(`frame_bytes()` long) and returns the clip count; `None` neighbours are silence.
`convert_f32` gives the samples before rounding (for tests, and for FLAC later).
`convert_run(prev, frames, next, out, clips)` converts consecutive frames in one window, the
same bytes as `convert` on each, and each frame's clip count (#24).
`new` never fails: a configuration that `PcmConfig::validate` refuses (the wide filter at
88.2 kHz) is converted with `PcmConfig::effective` instead, 40 kHz, which `config()`
reports. *Why:* a panic in a FUSE worker is an I/O error at best; options are checked once,
with `validate`, when the mount starts.

## Performance basis

scarletbook 0.3.0 decodes a stereo DST frame in about 0.17 ms on one core with its lockstep
decoder (about 80x real time per core; 0.2.0 took about 0.215 ms) and scales to about 13.7x
at 16 threads; plain DSD areas need no decoding.
A straight read through the mount should therefore be limited by the copy and FUSE
overheads, not by decoding. Measured figures are in [performance.md](performance.md).

## Deployment on a NAS (the container)

The container image (`Containerfile`, [deployment.md](deployment.md)) runs sacdfs and the
servers that share its mount, Samba (SMB) and NFS-Ganesha (NFSv4), in one container. The
decisions, each with its reason:

- **The servers run inside the container.** An appliance NAS shares only its own datasets,
  and cannot see a FUSE mount inside an app container (no mount propagation to the host on
  TrueNAS). Serving from inside depends on nothing in the host but its container runtime.
- **Build on UBI 10, run on Fedora.** Red Hat first: sacdfs is compiled with RHEL 10's Rust
  from UBI's AppStream. UBI's repositories have no Samba and no NFS server, RHEL's full
  repositories need a subscription (so the image couldn't be published or built in CI), and
  NFS-Ganesha isn't in RHEL at all. Fedora has all three. CentOS was not needed.
- **NFS-Ganesha rather than the kernel's NFS server.** The kernel server needs its module
  loaded on the host (a container can't load one), a rootful and in practice privileged
  container, and shares kernel state with the host's own NFS service. NFS-Ganesha needs only
  `CAP_DAC_READ_SEARCH`, for `open_by_handle_at(2)`. sacdfs's decoding (about 500 MB/s for
  DSF), not the server, sets the speed. Both servers rely on sacdfs's persistent file identity
  (#18) for file handles that survive a restart; CI checks this with NFS-Ganesha.
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
  order follows #18's restart sequence for the kernel server (stop NFS, then sacdfs; mount,
  then export with a fixed `fsid`), applied to NFS-Ganesha.
- **If one process stops, the container stops**, so the runtime's restart policy restarts
  all three together instead of leaving a server sharing an empty directory.

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
- *Persistent seek index:* unit tests of the file format (round trip; another size, mtime,
  path or area shape misses; every single damaged byte and every truncation is rejected;
  proptest of random and damaged files, with the checksum recomputed after the damage so that
  the parser itself is fuzzed), of the directory (private mode, LRU trimming, symbolic links
  and oversized files not read). `tests/index_cache.rs` saves the index of a straight read of
  every corpus case, then reads every frame alone and random ranges with it loaded, each
  frame also from a fresh tree so that only the loaded points are known, and requires the
  bytes of a straight read; the same with hostile entries (proptest of points in order but
  anywhere, and every point too late), which must fail their checks and still give the same
  bytes; and a changed size or mtime misses. `tests/mount_index.rs` mounts, seeks, unmounts,
  remounts and seeks again through the kernel: the same bytes, with fewer calls to the image
  and fewer bytes read.
- *Against scarletbook's seek index:* `tests/library_index.rs` (see [Why not scarletbook's
  seeking](#reading-a-range-of-a-dsf-file)): every frame and random ranges of every corpus
  case located by sacdfs equal reads through a complete library index and a straight library
  read; a provisional library reading of a hostile case is refuted and differs, where the
  locator is exact; an ignored test, `real_discs`, does the same on local real discs
  (`SACDFS_REAL_IMAGES`).
- *DSF header reads:* `tests/dsf_header.rs` reads every DSF file of the corpus as the
  kernel does for a tagger (128 KiB at 0, 128 KiB at 128 KiB, the tag's page): none of the
  reads starts read-ahead, and a player reading on starts it exactly at 256 KiB.
- *Open images:* unit tests of the handle cache (reuse, the bound, idle closing, a new
  handle after a change of size, of modification time or of the file, and none within the
  check interval). `tests/handles.rs` walks the whole corpus with at most 3 images open,
  reading every file whole and then pieces of all files interleaved (so that handles are
  closed and opened again mid-file), and requires the bytes of a walk with every image open
  and never more than 3 image descriptors in `/proc/self/fd`; and mounts the corpus with
  `--max-open-images 2`, reads the start, middle and end of every file through the kernel,
  and checks sacdfs's own descriptors.
- *Concurrent reads:* `tests/concurrent.rs` reads every file of every corpus disc on 4 threads
  per disc at once (each with its own readers, in another order and piece size), all discs
  together, every read searching, no frame cache and at most 2 images open, and requires the
  bytes of a straight read.
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
- *Container:* the `container` workflow builds the image, serves the corpus, mounts it over
  SMB and NFS with the kernel clients from a client container, and checks every track's
  per-channel hashes and every WAV header (`deploy/check-share.py`); a second run checks user
  access (guests refused), `--format dsf` and `--flatten`.

- *File identity:* unit tests (`src/ident.rs`, `src/tree.rs`): the bit layout; a pinned key;
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
  `tests/nfs-loopback.sh` (CI, in a privileged Fedora container with the runner's nfsd)
  exports a mount with the kernel NFS server, mounts it over loopback NFSv3, reads half of
  each of several files through open descriptors with `O_DIRECT` (so no read is answered from
  the client's cache), restarts sacdfs, and reads the rest through the same descriptors; then
  changes an image and requires `ESTALE` from its open file. The kernel NFS server could not
  be run on the development machine (no root, rootless containers, no nfsd module), so this
  test runs only in CI.

## Prior art

ratarmount (MIT; lazy archive indexing, seek points), squashfuse (BSD), fuser's examples
(MIT), libfuse's passthrough_hp (big reads, multithreading), Vangoor et al., "To FUSE or
Not to FUSE: Performance of User-Space File Systems", FAST 2017. GPL/LGPL FUSE archive
filesystems (fuse-zip, archivemount, mount-zip, rar2fs). sacdfs is GPL-3.0-or-later, so
code from compatibly licensed projects may be adapted, with attribution (see
CONTRIBUTING.md); SACD parsing and DST decoding still come only from scarletbook.
