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

- **The pin is tag v0.5.2** (0.1.0 shipped with v0.5.0, the latest release when its release
  preparation started, #45). *Why:* a release ships against the newest scarletbook, taken
  while it passes the correctness gates, and then frozen so that the performance round
  measures the code that ships; each move of the pin is a change of its own, gated like a
  release (DSF 93/93, the real-disc and corpus comparisons with scarletbook's exact seek
  index). 0.5.2 was taken for 0.2.0 because its seeking (#28), DSF output and DSD-to-PCM
  conversion are what sacdfs may offload to it next; the move itself changes no output.
- **Seeking is scarletbook's** (since 0.2.0, #28): its seek index per area, provisional (cold)
  readings and seek hints, through range cursors; sacdfs's own frame locator was removed.
  *Why:* it measured within the owner's 15 % leeway and no less exact
  ([Seeking](#seeking-scarletbooks-28)). Not used: cancellation (`CancelToken`) and progress
  (`Progress`); cancellation could later back FUSE `INTERRUPT` (#45). The tests use
  scarletbook's exact seek index as a reference.
- **Built without scarletbook's `parallel` feature;** sacdfs decodes on its own worker pool.
  *Why:* memory stays bounded by `--cache-mb` however many files are open, and a parallel
  library cursor would hold a window of its own (512 positions, about 48 MB).

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
- A file that is not an SACD image (no master TOC), or that fails to open, is left out; a
  directory whose `*.iso` files all turn out not to be SACD images shows up empty.
- An image directory whose name collides with a source subdirectory gets ` (2)`. A track
  that fails scarletbook's `check_track` (empty, too many channels, or past the area's
  capacity) is left out. Names are cut to fit 255 bytes.
- `Stereo` and `Multichannel` appear only for areas the disc has.
- File names come from the track text (title), with `/`, NUL and control characters
  replaced; a track without a title is `NN.dsf`. Duplicates get ` (2)`, ` (3)`.
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
  into, exactly when the image's own folder would have been. The price: an `*.iso` that
  turns out not to be an SACD image still counts as the chain's image, so its collapsed
  folder shows up empty, as a folder of such files does without the option.
- *Why these rules* (#13): always flattening would change paths for existing users;
  collapsing the top level too would drop the album name of a single-album artist; naming
  folders from the TOC's text is a separate option if ever wanted, since disc titles are
  often less descriptive than the owner's folder names. Deciding from the TOCs (for
  example, not counting files that are not SACD images) would mean opening every image at
  mount, which is the cost the lazy tree exists to avoid.

### Sidecar files (#79, #75)

The owner's goal for sacdfs (#82, 2026-11-01): "a value of using sacdfs is reclaiming storage
space - so that's what i want to architect for": people delete the DSF files they extracted
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
- **Changes while mounted.** The tree is read once, as for images. Opening a sidecar whose
  source size or mtime differs from what was listed fails with `ESTALE`. *Why:* the kernel
  holds the listed size for a day (`TTL`), so new bytes would be cut off or padded; a clear
  error until the next mount is better than wrong bytes. The next mount shows the new file
  under the same number with a new generation.
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
- **Not done:** embedding art in the DSF and WAV tags (a separate question, #79), rewriting
  playlist entries when `--flatten` or `--format` change paths (kept byte for byte, #75), and
  copying folder art into the area folders (the art sits one level above the track files with
  `--flatten`, two without, as in SOURCE).

## File identity and NFS export

For the server-side deployment (#17, #18): an NFS client keeps file handles, which hold the
inode and generation numbers, across server restarts. Numbers handed out in walk order would
change on every mount, and clients would get `ESTALE` on open files and cached directories
after every NAS reboot or app update.

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

**Generation numbers.** A track file's generation is 32 bits (all the kernel keeps) of a hash
of its image's sector count and mtime, its format, size and an output version per format, and
for WAV the full PCM flavour; never 0. A handle to a file whose bytes may have changed then
fails with `ESTALE` instead of serving other audio. The output versions (`DSF_VERSION`,
`WAV_VERSION`) are bumped by any change that makes the same track's bytes differ.
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

Directories keep their own mtime.

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
  With the fix, 2 of about 5,000 fixed-binary restarts lost the new mount, both with a file
  open and the old mount detached from outside, and neither sacdfs logged an unmount: not
  explained yet, and not an unmount by sacdfs. This was the failure seen in #51.
- On SIGINT, SIGTERM or SIGHUP, sacdfs unmounts (or detaches, if busy) only if the mount on top at
  its path still has its own device number, read from `/proc/self/mountinfo` right after
  mounting; otherwise it logs why and leaves the path alone. *Why:* the device number is
  unique while the mount exists, which is the only time it is compared; the mount table asks
  the file system nothing, so the check cannot wait on a stopping session.
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
  *Decision (owner, #28):* adopt the library's writer, accept the conformant TCOP: "i dont care
  about roons play history or churn". *Trade-off:* players and taggers that keyed on the old
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
6. **Pipeline.** On a plain DSD area, one pool thread demultiplexes frames 16 at a time, and
   each PCM frame is handed to the other pool threads as soon as its next neighbour is
   demultiplexed. *Why:* there is no DST decoding there, so demultiplexing (serial) and
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
  222 KB. A player's next read goes past 256 KiB and starts read-ahead there.

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

Two kinds of probe are answered so that the kernel stops asking (#61):

- **Missing names** get a negative entry (inode 0, cached for the same 24 hours) instead of
  `ENOENT`. Players, file managers and SMB clients look for `cover.jpg`, `folder.jpg`,
  `desktop.ini` and the like in every folder, again and again; the kernel now answers the
  repeats itself. *Why it is safe:* a directory's listing is made once per mount and never
  changes, so a name missing from it stays missing. Lookups of `.` and `..` (the NFS export
  code's) still fail with `ENOENT` when the inode is unknown, so a handle to something gone
  still gets `ESTALE`. Measured with fuser's request log on a 24-track area: three `stat`s of
  each of four missing names sent 3 `LOOKUP`s each before, 1 after.
- **Extended attributes:** `getxattr` and `listxattr` answer `ENOSYS`. The kernel then
  remembers that the filesystem has none and answers every later call itself, with
  `EOPNOTSUPP`, the answer of a filesystem without xattrs. *Why not `ENODATA` and an empty list,
  as before:* those are answers about one file, so the kernel asked again for every file:
  `ls -l` sends a `LISTXATTR` per entry. Measured on the same area (48 files): two `ls -l` and a
  `getfattr -d` sent 49, 49 and 48 `LISTXATTR` before; 1 `LISTXATTR` and 1 `GETXATTR` in the
  first `ls -l`, then none. Samba (`ea support = no` in the container) and NFS-Ganesha (no
  xattr support configured) read the same files through the container test.

**Filesystem size (#62).** `statfs` reports the size of the filesystem holding SOURCE, all of
it used (0 free, 0 available), the number of entries made so far as inodes, and 0 free
inodes. `df` showed a 0-byte filesystem before, and SMB clients were told 0 bytes total and
free, which some treat as a broken share. *Why the source's size, not the sum of the files'
sizes:* the tree is made lazily, so the sum is known only for the folders listed so far; it
would grow while a client browses and opening every image at mount to know it costs a TOC read
per image. The source's size is one `statvfs`, is the same however much has been browsed, and
says truthfully where the bytes live. *Why 0 free:* the mount is read-only; a full
filesystem is what copy dialogs understand as "nothing can be written here". The size is read
at mount (the source was just walked, so this does not wait) and again at most once a minute,
on a request thread, so a stalled source never holds up a `statfs` reply: it gets the last
size read. `statvfs` comes from `nix` (MIT, already a dependency of fuser, with the same
`fs` feature), not `libc`: sacdfs keeps `unsafe` to its daemonising code.

## Concurrency

fuser dispatches requests on 4 threads, so that a directory listing that opens many images
does not hold up other requests. Reads are answered from a pool of request threads (as many
as decoder threads, at least 4 and at most 16): the request's `ReplyData` moves to a worker,
which decodes or copies and replies, so slow reads on one file don't block
`lookup`/`readdir` or reads of other files. DST frames are decoded, and frames converted to
PCM, on a separate rayon pool (`--threads`), one `DstDecoder` and one `Converter` per channel
count per thread.

- *Why the request pool is not a rayon pool:* a rayon worker that waits for another pool runs
  other jobs of its own pool meanwhile, and a second read of the same file would then wait,
  on the same thread, for the demultiplexer lock the first one holds.
- Every read runs under `catch_unwind`, so even an unexpected panic becomes an I/O error on
  that read.

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

## PCM conversion

The DSD-to-PCM converter, `sacdfs::pcm`, behind the WAV files. The filter designs and their
measurements are in [pcm-quality.md](pcm-quality.md), the speed in
[performance.md](performance.md#converter-throughput). Each decision below says why, the
alternatives and the trade-off.

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
stages. Stage 1's technique, lookup tables of filter partial sums indexed by whole DSD bytes,
is credited to Sebastian Gesemann's dsd2pcm (BSD-3-Clause), which FFmpeg and MPD also use; no
code was taken from dsd2pcm or FFmpeg. What changed from pysacd: stateless per frame; odd
lengths; equal polyphase sums; new tap counts for the 40/30/80 kHz bands at every rate
(pysacd's 176.4 kHz path passes to 60 kHz); half-band stages where the band allows, which
halves their cost; integer stage 1 and f64 accumulation (below).

**Arithmetic (why: nothing above the 24-bit step, and the same bits everywhere).** Stage 1
adds integer table entries (fixed point, 2^-28): exact. The later stages accumulate in f64.
Measured on random DSD against a plain f64 evaluation of the same filters, f32 accumulation
in stage 3 alone added 0.34 of a 24-bit step RMS, and an f32 stage 2 another 0.15: more
than the 24-bit quantisation itself (0.29). In f64 the difference is below 0.00001 of a
step. Every output is a fixed sequence of operations (Rust never fuses a multiply and an
add, nor reorders float sums), so the SSE2, AVX2 and AVX-512 code gives identical bytes
(a test pins the output's hash for every configuration). *Trade-off:* f64 halves the SIMD
width of the FIR stages; the bar is still met with margin.

**Coefficients computed at start-up (why).** A converter designs its filters when it is made:
27 to 55 µs (Kaiser windows, a Bessel series, the stage-1 tables), once per worker thread.
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
`cargo install` users get, would otherwise run the converter with SSE2 only, and the wide
filter sat near the 100x bar of #6. So stage 1 and the three FIR kernels are compiled three
times, for the baseline, x86-64-v3 (AVX2) and x86-64-v4 (AVX-512), by the `multiversion`
crate (MIT OR Apache-2.0), and the copy is chosen from the CPU when the first converter is
made (`pcm::Simd`; `SACDFS_SIMD=baseline|v3|v4` overrides it). *Why `multiversion`:* calling
`#[target_feature]` code needs `unsafe`, which sacdfs denies outside its daemonising code; the
crate's macro keeps that call behind its own CPU check, so no copy can run on a CPU without
its features, and sacdfs keeps `unsafe_code = "deny"`. *Alternatives:* a local `unsafe` block
with `std::arch` (more control, breaks the policy for no measured gain); shipping x86-64-v3
builds only (excludes older CPUs); a 112 dB wide filter (about 5 % faster, lower quality).

- *Granularity:* the kernels are dispatched, not the whole channel. Forcing all of a channel's
  code inline into one dispatched function made the baseline path about 10 % slower;
  dispatching each kernel (a few calls per channel and frame) leaves it as fast as a build
  without dispatch.
- *Wider blocks on wider vectors:* each output is a chain of dependent adds, so a block must
  hold enough independent outputs to keep the adders busy: 16 on the baseline, 32 on v3, 64
  on v4 (8 registers of accumulators each), which gained up to 4 % over 16. The block size
  never changes an output's operations.
- *v3 by default, even with AVX-512:* measured on a Zen 5 CPU, the v4 copy was slower than v3
  on every filter but one, and much slower with both threads of a core converting, as the
  decoder pool does under load. It is built and tested, and `SACDFS_SIMD=v4` selects it, so
  that other CPUs can be measured; the default changes only with a measurement. AVX-512
  target features need Rust 1.89, so a build with an older compiler (the MSRV is 1.85) has no
  v4 copy.
- *Identical output:* the pinned hashes are checked on all three paths in one process
  (`Converter::with_simd`), and CI runs the converter and WAV tests once per `SACDFS_SIMD`
  value.
- *Building:* RHEL's Rust targets x86-64-v3 by default; there every copy has AVX2, and
  `-C target-cpu=x86-64` gives a true baseline.

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

**API.** `pcm::Converter::new(channels, &PcmConfig)` holds its buffers (about 100 KiB), is
`Send`, allocates nothing per frame, and is made once per worker thread.
`convert(prev, cur, next, out)` writes frame `cur`'s interleaved 24-bit little-endian PCM
(`frame_bytes()` long) and returns the clip count; `None` neighbours are silence.
`convert_f32` gives the samples before rounding (for tests, and for FLAC later).
`convert_run(prev, frames, next, out, clips)` converts consecutive frames in one window, the
same bytes as `convert` on each, and each frame's clip count.
`new` never fails: a configuration that `PcmConfig::validate` refuses (the wide filter at
88.2 kHz) is converted with `PcmConfig::effective` instead, 40 kHz, which `config()`
reports. *Why:* a panic in a FUSE worker is an I/O error at best; options are checked once,
with `validate`, when the mount starts.

## Performance basis

scarletbook decodes a stereo DST frame in about 0.17 ms on one core (about 78x real time per
core), and plain DSD areas need no decoding. The converter runs at about 300x real time per
core for the default 176.4 kHz stereo output, so on a DST area conversion adds about a
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
  `id-token`/`attestations: write` only in the two publishing jobs, which run only when
  `github.ref_type == 'tag'`. No caches (nothing a pull request writes can reach a release
  build), no persisted credentials, actions pinned by SHA.
- **Not crates.io.** scarletbook is a git dependency and crates.io refuses those
  (`publish = false`). Publishing waits for scarletbook on crates.io (scarletbook-rs#39).

## Testing

- **Unit tests:** DSF header and tag layout, offset-to-frame mapping (including the padded
  last block and every channel count 1 to 6), name sanitising; proptest of the offset
  mapping. Fuzzing the image layer is scarletbook's job.
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
  an ignored test, `real_discs`, does the same on local real discs (`SACDFS_REAL_IMAGES`).
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
  `real_discs`, runs the stream check on local real discs (`SACDFS_REAL_IMAGES`).
- **The converter:** `tests/pcm.rs` and the unit tests of `src/pcm.rs` (quality, determinism,
  pinned hashes on every code path); see [pcm-quality.md](pcm-quality.md).
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

### Oldest distributions (#58)

*Why:* the README promises RHEL 8, Debian 10, Ubuntu 20.04 and Fedora 29, but CI tests only
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
