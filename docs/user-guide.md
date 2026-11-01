# sacdfs user guide

How to use sacdfs: its options, the files it shows, how taggers and players are served, and
what to do when something goes wrong. Installing is in the [README](../README.md#install),
running it on a NAS in [deployment.md](deployment.md).

## Options

```
sacdfs [OPTIONS] SOURCE MOUNTPOINT
```

`SOURCE` is one image file, or a directory that is searched recursively for `*.iso` files
(any case); symbolic links to files are followed, links to directories are not.
`MOUNTPOINT` is an existing directory. The [README](../README.md#options) lists every
option; this is what they do.

**Where it can mount.** sacdfs reads the images through their paths, so none of them may be
inside its own mount: reading one would send sacdfs a request it waits for itself, and the
mount would hang beyond what even `kill -9` can stop. So sacdfs refuses, before mounting:
- a `SOURCE` that is `MOUNTPOINT` or inside it (`sacdfs X X`, `sacdfs X/sub X`), symbolic
  links resolved;
- a `MOUNTPOINT` inside `SOURCE` that holds images (an empty one is fine:
  `sacdfs ~/Music ~/Music/sacd`);
- an `--index-cache` directory inside `MOUNTPOINT`. If the default cache directory is inside
  it (a mount point at your home directory), sacdfs mounts without the cache and warns.

A `MOUNTPOINT` that is not empty is allowed, as FUSE 3 allows it, with a warning: its files are
hidden while sacdfs is mounted.

The mount is listed as `sacdfs` (in `df`, `mount` and `/proc/mounts`), not under the source
path: any character in the path, a comma included, is then safe. `-v` logs the source.

**Running.**
- Without `-f` (`--foreground`), sacdfs returns once the filesystem is mounted and keeps
  running in the background, logging nowhere unless `--log-file FILE` is given (or under
  systemd, where standard error is the journal and is kept); with it, it stays in the
  foreground and logs to standard error. `-v` logs what sacdfs found (image counts, images and tracks not shown and
  why, areas without a defined speaker layout, and at exit how many reads of the images it
  made); `-vv` logs debug detail. `RUST_LOG` (for example `RUST_LOG=sacdfs=debug`) overrides
  both.
- `--allow-other` lets users other than the one who mounted read the files, which a media
  server running as its own user, or an NFS or SMB server sharing the mount, needs. FUSE
  allows it only with `user_allow_other` in `/etc/fuse.conf` (or when mounting as root).

**Speed and memory.**
- `--threads N` sets how many threads decode DST frames and convert frames to PCM; `0` (the
  default) is one per CPU. On a machine that has other work to do, fewer threads keep sacdfs
  from crowding it out while a file is read quickly (`cp`); a player reading in real time
  needs a small part of one core.
- `--cache-mb N` (default 256) bounds the cache of decoded DSD frames and converted PCM
  frames, which DSF and WAV reads share. It is sacdfs's main use of memory, whatever the
  number of open files: about 170 to 360 MiB in all with the default, under four players and
  a tagger. A larger cache helps only when the same parts of files are read again and the
  kernel's page cache has dropped them.
- `--max-open-images N` (default 64, 1 to 1024): an image stays open after its directory is
  listed and between reads, so that a seek does not have to open it again (a round trip or two
  on a network share). The least recently used image is closed when the limit is reached, and
  any image unused for 5 minutes is closed.

**What is shown.**
- `--format dsf`, `wav` or `dsf,wav` (the default) chooses which files appear.
- `--pcm-rate`, `--pcm-cutoff` and `--pcm-gain` choose how the WAV files are made
  ([Formats](#wav-files)). `--pcm-cutoff wide` needs `--pcm-rate 176400` or `352800`; with
  `88200` sacdfs refuses to start, since 88.2 kHz cannot hold an 80 kHz band.
- `--flatten` collapses folders that hold nothing but one image (see the
  [README](../README.md#layout)).
- `--sidecars LIST` chooses which other files of `SOURCE` (folder art, scans, playlists) are
  passed through next to the images ([Sidecar files](#sidecar-files-art-scans-playlists)).

**The seek index cache** (`--index-cache DIR`, `--no-index-cache`). As it reads, sacdfs (through
scarletbook) records where frames are in each image, and keeps those seek hints in
`$XDG_CACHE_HOME/sacdfs` (else `~/.cache/sacdfs`), so that a seek after a remount or a reboot
near a place read before needs one read of the image instead of three or four. That matters
most when the images are on a network share, where each read is a round trip.
- One small file per image (a few KB per area), named by a hash of the image's path, never the
  path itself; 32 MiB in all, the least recently used files removed first. Written every 30
  seconds if something changed, and at unmount. It never leaves the machine. sacdfs 0.2 writes
  a new format and ignores 0.1's files (and 0.1 ignores 0.2's): each costs only a cold seek.
- An image whose size or modification time changes misses its entry. A loaded entry is
  checked as it is used, so a stale or damaged cache can only make a seek slower, never wrong.
  Deleting the directory is always safe.
- `--no-index-cache` starts every mount with an empty index (used for measurements).

**Unmounting.** `fusermount3 -u MOUNTPOINT`, or SIGINT (Ctrl-C), SIGTERM or SIGHUP to sacdfs.
If files are still open, or an NFS server still exports the mount, the mount is detached
instead, as `umount -l` would: it disappears at once, the open files still read, and sacdfs
stops when the last one is closed.

**Signals.** SIGINT, SIGTERM and SIGHUP all unmount and stop sacdfs; SIGHUP does not reload
anything (there is nothing to reload). A foreground sacdfs whose terminal closes gets SIGHUP and
unmounts; a background one runs in its own session, which a closing terminal doesn't reach.
A signal that arrives while sacdfs is starting is handled too: before it mounts, it stops
without mounting; once it is mounting, it unmounts as soon as the mount exists. SIGKILL can't
be handled, and leaves a dead mount (below).

**Restarting on the same mount point.** A sacdfs started while the previous one is still on
the mount point (it is stopping) waits up to 10 s for that mount to go, instead of mounting
over it, then fails with "still mounted there after 10 s". A stopping sacdfs only ever unmounts
its own mount, never one started on the same path since.
- If the previous sacdfs was killed or crashed, its mount is dead ("Transport endpoint is not
  connected"). A new sacdfs of the same user detaches a dead sacdfs mount at once and mounts,
  so a service manager's restart works.
- A dead mount of another file system, or any FUSE mount of another user, is left alone:
  sacdfs fails at once and says what is there.

**CPU code paths.** The WAV converter (scarletbook-pcm) picks its code when it starts: AVX2 and
FMA where the CPU has them, SSE2 otherwise, with the same output bit for bit.

## Formats

### DSF files

Standard DSF files: DSD64 (2.8224 MHz), 1 bit, 4096-byte blocks per channel, with an
ID3v2.4 tag at the end.
- The audio is the track's frames as the disc's table of contents gives them, bit-identical
  to what the reference extractors (pysacd's `pysacd-extract`, scarletbook's `bench decode`)
  write. The pauses between tracks are not included.
- The channel type follows the disc's speaker layout: stereo, 5 channels (3/2) or 5.1.
- A frame missing from a damaged image is filled with DSD silence (0x69), an invalid DST
  frame with 0x55, as the reference extractors do, with a warning in the log.

### WAV files

The same tracks converted to 24-bit PCM:
- **Rate:** `--pcm-rate` 176 400 Hz (the default; DSD64 divided by exactly 16), 88 200 or
  352 800 Hz. Always 24-bit, so no dither is needed and every read of a file gives the same
  bytes.
- **Filter:** band-limited to 40 kHz by default, following Daniel Weiss's practice, which
  keeps DSD's ultrasonic noise out of the playback chain; 30 kHz with `--pcm-cutoff 30`; or
  about 80 kHz with `--pcm-cutoff wide`, which lets more of the ultrasonic band, and of DSD's
  noise, through. Measured quality: [pcm-quality.md](pcm-quality.md).
- **Level:** 0 dB by default, where 100 % DSD modulation is PCM full scale, so the default
  never clips. `--pcm-gain` from -60 to +24 dB; samples that would clip are clamped, and the
  log says once per file how many.
- **Gapless:** each track's first and last frames are converted together with the
  neighbouring tracks' audio, so the WAV files of an area played one after the other are one
  continuous stream.
- **Container:** RIFF/WAVE with `WAVE_FORMAT_EXTENSIBLE`. A file over 4 GiB (stereo at
  176.4 kHz after about 67 minutes, six channels after about 22) is RF64 instead, which
  FFmpeg, sox, libsndfile and most current software read, but not everything (Python's
  `wave`, for one).
- **Channels:** the order and mask come from the disc's speaker layout: stereo FL FR,
  5-channel (3/2) FL FR FC BL BR (5.0) and 5.1 FL FR FC LFE BL BR. An area with another
  combination of channel count and configuration (rare: not defined by the format) gets the
  layout of its channel count (1 FC; 3 FL FR FC; 4 FL FR BL BR; 5 and 6 as above), and sacdfs
  logs this once per disc at `-v`.
- The audio starts after 256 KiB of padding (a `JUNK` chunk) and ends on a 4 KiB boundary,
  so that reading the header or the tags never converts anything.

## Tagging behaviour

Both files of a track carry the same **ID3v2.4 tag**, made from the disc's text by scarletbook,
as far as the disc has it: title (TIT2), performer (TPE1), album artist (TPE2), album (TALB),
track n/N (TRCK), disc n/N for multi-disc sets (TPOS), songwriter (TEXT), composer (TCOM),
arranger (TIPL), messages (COMM), date (TDRC), ISRC (TSRC), genres (TCON), publisher (TPUB)
and copyright (TCOP, the year first, as ID3v2.4 asks). Album fields fall back to the disc's
text. The WAV files hold it in an `id3 ` chunk (read by FFmpeg, TagLib, mutagen, foobar2000 and
mp3tag), plus a `LIST`/`INFO` chunk (title, artist, album, track number, date, genre) for
software that reads only that. Both come after the audio. sacdfs 0.2 changed the TCOP of discs
with a copyright text, so those files show a new modification time once.

**Multichannel copies have their own album name.** The album tag of the files in a disc's
`Multichannel` folder (DSF and WAV) ends in ` (Multichannel)`, so "Album" is the stereo copy
and "Album (Multichannel)" the multichannel one. *Why:* in client tests, Navidrome, LMS, MPD
and MinimServer all showed a disc's two copies as one album, either merged (each track up to
four times) or as two albums with the same name. Stereo files are unchanged, and so is a
disc with no album text. Only tags change: the audio is the same, and `sacdfs verify`
compares audio only, so extracts of multichannel tracks still verify. `--no-multichannel-album-suffix`
(`SACDFS_MC_ALBUM_SUFFIX=0` in the container) leaves the album as the disc has it. Switching
it does not change the files' modification times, so restart the mount and have your player
rescan. (When the tag layer, #81, can name a disc's album, that name will win and get no
suffix, unless the tags file asks for one.)

The filesystem is read-only, so tags cannot be edited in place; a tagger's changes belong in
its own database, or in copies of the files.

**What reading tags costs:**
- `ls -l`, `stat` and directory listings never read audio: sizes are exact and known from
  the table of contents.
- A WAV file's header and tags read no audio at all. A tagger that walks every chunk header
  (mutagen, TagLib) also reads the data chunk's header, right before the audio, and converts
  the track's first frame or two: a few milliseconds.
- A DSF file's audio starts at byte 92, and the kernel reads at least 128 KiB from the start
  of a file whatever was asked for, so reading a DSF file's header decodes the track's first
  quarter second or so (about 220 KB of the image). Reading its tag decodes almost nothing.
  A tag walk of 93 DSF files took 0.09 s from a local disk.
- Neither starts read-ahead: sacdfs decodes ahead only once a reader reads on into the audio.

**Library scanners.** A program that reads every audio file of a library (fingerprinting,
loudness or ReplayGain scans, waveform previews) converts every WAV file it reads, which costs
CPU and time for no new information. Point such programs at a mount with `--format dsf`, or
mount twice: one mount for them, one with WAV files for players.

**MPD, moOde, Volumio and Jellyfin.** Use `--format dsf` for any mount that MPD (which moOde
and Volumio run) or Jellyfin scans. Jellyfin's ffprobe reads 5 MB of each stereo WAV and 12 to
14 MB of each 5.1 WAV (a first scan 27 to 58 % slower); for MPD: MPD plays DSF natively. Its first database scan probes about 5 MB of every WAV
file through FFmpeg, so every WAV file is converted for that probe: in a test of 93 tracks the
first scan took about 9 s with `dsf,wav` and 0.6 s with `dsf` (about 15 times as long), and
a library of 1,000 discs would cost about an hour of CPU. The same happens after any MPD
database rebuild. Incremental `mpc update` reads nothing. If a player needs WAV files, mount
twice as above. Jellyfin's LUFS normalization and Plex's nightly deep analysis read every
file in full; turn them off, or use `--cache-mb 16`.

## Sidecar files: art, scans, playlists

A folder that holds images often holds what goes with them: `folder.jpg` or `cover.jpg`,
scans of the booklet in an `Artwork/` or `Scans/` folder, playlists. sacdfs shows these
**sidecar files** where they are in `SOURCE`, read-only and byte for byte: the mount shows the
source file's size, modification, access and change times, and reads its bytes as they are.
sacdfs never parses them. So Roon, Plex, foobar2000 and file managers find the folder art
next to the albums, and a playlist can sit next to its images (Roon imports a playlist only
from inside the same storage location). It is one of the things that let you delete the
DSF files you extracted and keep only the images.

**Which files.** `--sidecars LIST`, read from left to right:
- an extension (`jpg` or `.jpg`): files with it, in any case (`Cover.JPG`);
- a folder name ending in `/` (`Artwork/`): a folder of that name, any case, is shown with
  everything in it, any file type and subfolders, so a booklet scan as PDF or TIFF passes too;
- `default`: `jpg,jpeg,png,webp,m3u,m3u8,Artwork/,Scans/` (the default of the option);
- `-item` removes an item, `none` empties the list. So `--sidecars default,cue,Covers/` adds to
  the default, `--sidecars default,-webp` removes from it, `--sidecars jpg,png` replaces it,
  and `--sidecars none` shows only the audio, as sacdfs 0.1.0 did.

`.iso` cannot be an item: images are shown as their audio. Audio files (DSF, DFF, FLAC) are
not in the default on purpose: extracted copies next to the images would show up twice in a
player, once as the extract and once as sacdfs's file.

**Where they appear.**
- Next to the image's folder: `SOURCE/Artist/Album/folder.jpg` is
  `MOUNTPOINT/Artist/Album/folder.jpg`, beside `MOUNTPOINT/Artist/Album/disc/`.
- **Folder art is also inside each area folder** (#95): the images among the sidecars
  (`jpg`, `jpeg`, `png`, `webp`; not playlists, scans folders or other files) also show in
  `Stereo/` and `Multichannel/` of every image in the folder, after the tracks, so that
  Lyrion Music Server, MPD, MinimServer and Plex, which look for folder art only in the
  audio file's own folder, find it. It is the same file with a second name: the same inode
  number, so a server that walks both sees one image. No audio file changes, only the art is
  added to the listing. It follows `--sidecars`: an extension you remove (`default,-webp`)
  is not shown in either place, and `none` shows no art at all. A name that a track file
  already has gets ` (2)`.
- With `--flatten`, the sidecar files of every folder of a collapsed chain show in the folder
  that is kept, next to `Stereo` and `Multichannel`, outermost folder first. A name that is
  already taken (two `folder.jpg` in the chain, or a file named `Stereo`) gets ` (2)`, as
  repeated track names do.
- Only folders that hold images (directly or below) show sidecar files, plus the sidecar
  folders inside them; a folder of art with no image anywhere below is not shown. Hidden
  files and folders (a name starting with `.`, such as macOS's `._folder.jpg`) never are, nor
  an empty sidecar folder, nor anything inside the mount point when it is inside `SOURCE`.
- Listings are in the same order as before, with the sidecar files after the images.

**Playlists** are passed as they are, never rewritten: write their entries as relative paths
that resolve inside the mount, as sacdfs names its files (`Album/disc/Stereo/01 - Title.dsf`
for a playlist in `Artist/`). `--flatten` and `--format` change those paths, so a playlist is
written for one layout.

**Symbolic links.** A sidecar that is a symbolic link is shown as the file it points to, if
that is a regular file inside `SOURCE`, the link resolved by name (relative, or absolute
under `SOURCE`), and not inside the mount point. A link that points outside `SOURCE`, or
into the mount, is left out with a warning in the log; a link to another link, a link
through a linked folder, and folder links are not followed at all. Every sidecar is opened one
folder at a time without following links, so nothing changed in `SOURCE` after the mount can
lead sacdfs outside it.

**Changes.** New and removed sidecar files appear without a remount (#101). When a folder is
listed or looked into, and its folder in `SOURCE` has a different modification time than at the
last reading (checked at most every 2 seconds), sacdfs reads that folder again: new images,
sidecar files and sidecar folders show, removed ones go. Entries that did not change keep
their inode numbers. A file removed while a program has it open still reads for that program;
new opens fail with "no such file". A sidecar file that was replaced shows its new size and
time under the same inode number. While a sidecar is open for the first time after being
replaced but before its folder was read again, the open fails with "stale file handle"
(its size, already shown, would not match its bytes), and works once the folder is read
again. Programs that cache listings see a change after the 2 seconds.

**Cost.** Mounting reads only names, as before. The first listing of a folder looks at its
sidecar files (one `stat` each, in parallel, as images are opened); nothing else reads them
until they are opened. A folder without sidecar files costs nothing more.

## Sharing the mount over NFS

The kernel NFS server can export a sacdfs mount, so that NFS clients see the files. (On a NAS,
the [container](deployment.md) does this for you, with SMB as well.) Inode numbers come from
what each entry is (its source path, or the image, area, track and format), not from the order
sacdfs finds things, so they are the same on every mount, and clients keep working, open files
included, after sacdfs restarts. `/etc/exports` needs an `fsid` (a FUSE mount has no device
number of its own), and sacdfs `--allow-other` when it does not run as root:

```
/srv/sacd  192.0.2.0/24(ro,fsid=4242,no_subtree_check)
```

- To restart sacdfs under an export, stop the NFS server first, then start it again after
  the new mount is up (`systemctl stop nfs-server`, restart sacdfs, `systemctl start
  nfs-server`). Its clients wait, as for a server reboot. While the server runs, its open
  files keep the old mount busy, and with the export gone a client could be told "stale file
  handle".
- NFSv3 clients carry on reading as if nothing happened (this is what the tests check).
  NFSv4 clients also reclaim their open state after a server restart, within the server's
  grace period.
- A file whose image changed (size or modification time) is refused with "stale file
  handle" instead of showing other audio, as is a WAV file after a change of `--pcm-rate`,
  `--pcm-cutoff` or `--pcm-gain`. Directories stay valid.
- With `--flatten` turned on or off, images, areas and files keep their numbers (a collapsed
  folder is the image's folder); only the folders that disappear or reappear change.
- A sidecar file (folder art, a playlist) is numbered by its path in `SOURCE`, with or
  without `--flatten`, and its handle works across restarts while the file stays the same;
  once its size or modification time changes, its handle is stale.
- Adding or removing images does not change the numbers of the others. The design is in
  [design.md](design.md#file-identity-and-nfs-export).

## Times, sizes and free space

- **Sidecar files** show their source file's size and times as they are.
- **Modification times.** A DSF or WAV file shows its image's modification time, moved
  forward whenever its bytes would differ: a WAV file with a non-default `--pcm-cutoff` or
  `--pcm-gain` shows a later time (by up to 7 hours, a different time for every cutoff and
  0.01 dB of gain), and a sacdfs release that changes the bytes of a format moves that
  format's times forward again. rsync, Plex, Roon and other tools that compare size and time
  therefore see a change of options or of output as a change of the file, and copy or
  re-analyse it. The same options always show the same times, across remounts and restarts.
  `--pcm-rate` changes the files' size instead. With the default options, the times are the
  images' own.
  With non-default options, a file whose image changed in the last few hours can show a
  time slightly in the future (up to 7 hours ahead); tools such as `tar` may warn about it.
- **`df` and free space.** The mount reports the size of the filesystem that holds SOURCE,
  all of it used: `df` shows that size with 0 available, and SMB clients (Explorer, Finder)
  see a full drive of that size. Nothing can be written to the mount anyway.
- **Extended attributes.** There are none: `getfattr` and `listxattr` say "Operation not
  supported" (the kernel answers this itself after the first request).

## Before you delete extracts

If you extracted DSF or DFF files from your images before (with `sacd_extract`, pysacd or
another tool), sacdfs can replace them: it serves every track from the image on demand.
`sacdfs verify` tells you, file by file, whether that is safe. It reads the extracts and the
images and writes a report; **it never deletes, moves or changes a file.**

```
sacdfs verify [OPTIONS] SOURCE [EXTRACTS]...
```

`SOURCE` is the image file or directory of images, as for mounting. `EXTRACTS` are files or
directories searched recursively for `.dsf`, `.dff`, `.flac` and `.wav` files; without them,
`SOURCE` itself is searched, for extracts kept next to their images. Nothing needs to be
mounted: the files sacdfs would serve are read in-process, through the same code as a mount.

| Verify option | Meaning |
|---|---|
| `--map FROM TO` | Extracts under `FROM` belong to the images under `TO`, a directory in `SOURCE`; for extracts kept in a tree of their own that mirrors the images' folders. Repeat it for more trees; the longest `FROM` that applies wins. |
| `-o`, `--output FILE` | Write the report to `FILE` instead of standard output. |
| `--resume` | With `-o`: keep the rows of an earlier report at `FILE` for files whose size and modification time haven't changed, except ERROR rows, and check only the rest. An interrupted run leaves every row it finished in `FILE`, so `--resume` picks up where it stopped. |
| `--threads N` | Files checked at once, and DST decoder threads; `0` (the default) means one per CPU. |
| `--cache-mb N` | Size of the cache of decoded frames, in MiB (default `64`). |
| `-q`, `--quiet` | No progress line on standard error. |
| `--log-file FILE` | Append the log to `FILE` (mode 0600), at info level unless `-v` says more. |
| `-v`, `--verbose` | Log more; `RUST_LOG` overrides it. |

**How a file is matched.** An extract belongs to the images in its folder, or in the nearest
folder above it that holds images (after `--map`). Among the tracks sacdfs shows for those
images, the candidates are those with the extract's channel count and samples per channel;
the track whose number the file name starts with is tried first. The audio is compared by the
SHA-256 of the raw DSD: the samples the header counts, interleaved, earliest sample first,
whatever the container (DSF or DFF), bit order, block size, padding or tags. Only candidates
are decoded, so a library is read about once.

**The statuses.**

| Status | Meaning | Delete? |
|---|---|---|
| `IDENTICAL` | sacdfs serves the same audio (the `image`, `area` and `track` columns say which track). | Safe for the audio; see `loses` for tags. |
| `DIFFERENT` | A track of the same length and channels exists, but its audio differs. | Keep it. |
| `NO-MATCH` | No track of those images has the file's channel count and length, or the file isn't at SACD's 2.8224 MHz, or no image is in its folder or above. The reason names the nearest track by length. | Keep it. |
| `UNSUPPORTED` | A file that can't be compared, such as a DST-compressed DFF; the reason says why. Decompress it (to a plain DSD DFF or DSF) and check again. | Keep it. |
| `ERROR` | The file is truncated, broken or unreadable, or sacdfs could not read its track. | Keep it until you know why. |
| `NOT-COMPARABLE` | FLAC or WAV. A CD-layer rip isn't in the SACD image at all, and a PCM conversion of the SACD layer made by another converter never matches sacdfs's WAV bit for bit. | Your call: sacdfs's WAV replaces it only if you prefer it. |

**What a deletion would lose.** The `loses` column lists what the extract's tags hold that
sacdfs's own tag for the track doesn't: `musicbrainz` (MusicBrainz IDs), `art` (embedded
pictures), `replaygain`, and `other=` with the frame names (a changed title shows up as
`TIT2`, a genre as `TCON`). sacdfs's tags come from the disc's text only, so anything a
tagger added is listed here. The same information written another way doesn't count: an
ID3v2.3 date (`TYER` and `TDAT`, day and month in either order) that sacdfs's `TDRC` holds, a disc number of `1/1` (sacdfs
leaves it out for a single disc), a track number with the total (`3/12`) where sacdfs has `3` and 12 is the area's track count, a `PERFORMER` that is sacdfs's artist, empty frames. The
summary counts the IDENTICAL files that lose each kind.

**The report** is tab-separated, one row per file, with a header row: `status`, `path`,
`bytes`, `mtime`, `image`, `area`, `track`, `channels`, `samples`, `sha256` (of the
compared audio), `loses` and `reason`. Tabs, newlines and backslashes in fields are
escaped (`\t`, `\n`, `\\`), as are bytes of a path that aren't UTF-8 (`\xNN`). The summary
goes to standard error: counts and bytes by status, the reclaimable bytes (the IDENTICAL
files), and the tags they carry.

**Exit status:** 0 when every DSF and DFF file is IDENTICAL, 2 when any is not, 1 when the
check couldn't run.

Examples:

```sh
# Extracts next to the images; then list what is safe to delete.
sacdfs verify -o report.tsv ~/music/sacd-images
awk -F'\t' '$1 == "IDENTICAL" && $11 == "" {print $2}' report.tsv

# Extracts in ~/music/dsf/<artist>/<album>/, images in ~/music/sacd-images/<artist>/<album>/.
sacdfs verify -o report.tsv --map ~/music/dsf ~/music/sacd-images ~/music/sacd-images ~/music/dsf
```

Notes:
- A directory named `verify` given as `SOURCE` to mount needs a path (`./verify`):
  `sacdfs verify ...` is always this command.
- Only `.dsf`, `.dff`, `.flac` and `.wav` files are looked at. Folder art, playlists and other
  files are not audio: they are counted in the summary ("not audio, not checked"), never
  listed in the report, and never touched. sacdfs passes its sidecar files through itself
  ([Sidecar files](#sidecar-files-art-scans-playlists)), so those next to the images stay
  where they are.
- A sacdfs mount can't be given as `EXTRACTS` (its files are what verify compares with), and
  mounts inside `EXTRACTS` are skipped.
- A file sacdfs's tree leaves out (a track it can't play, a damaged image) has no candidate:
  NO-MATCH or ERROR, never IDENTICAL.

## Images that change, and damaged images

- **New or removed images and sidecar files** appear or disappear within a few seconds, with
  no remount: a folder is read again when its modification time in `SOURCE` changes
  ([Sidecar files](#sidecar-files-art-scans-playlists)). A subfolder that held no image
  when its parent was last read is watched too, so an album copied in file by file shows
  once its image is there. Not covered: a folder collapsed by `--flatten` that gains a
  second image (it stays one image's folder until a remount), and an image file replaced
  under the same name.
- **An image file that changes while mounted** (another size, modification time or file) is
  opened again, at most a second after the change. An image whose size changed gives I/O
  errors until the next mount, since its directory still shows the old tracks.
- **A damaged image** shows up as missing directories or files, filled audio (missing frames
  as 0x69, invalid DST frames as 0x55, with a warning in the log) or an I/O error on one
  file. It never stops the mount: every image is treated as untrusted input.

## Mounting at boot: fstab and systemd

**fstab.** `mount -t fuse.sacdfs` runs `mount.fuse3`, which runs `sacdfs SOURCE MOUNTPOINT -o OPTIONS`.
Put `sacdfs` where root finds it (`/usr/local/bin` or `/usr/bin`) and `fuse3` installed:

```
# /etc/fstab
/srv/sacd  /mnt/sacd  fuse.sacdfs  ro,allow_other,cache_mb=64,format=dsf,_netdev,nofail  0 0
```

`-o` takes sacdfs's own options as `name` or `name=value` with the dashes written as underscores
(`allow_other`, `cache_mb=64`, `pcm_rate=88200`, `flatten`, `index_cache=/var/cache/sacdfs`,
`log_file=/var/log/sacdfs.log`, `verbose=2`). A comma separates `-o` options, so a list value
uses `+`: `format=dsf+wav`, `sidecars=default+cue`. The options of `mount(8)` and fstab (`ro`,
`rw`, `noauto`, `nofail`, `_netdev`, `nosuid`, `nodev`, `x-systemd.*` and similar) are accepted
and ignored by sacdfs itself; sacdfs is always read-only. Any other option is an error, so a
typo does not pass silently. `allow_other` needs `user_allow_other` in `/etc/fuse.conf` for
non-root users. Without `-f`, sacdfs goes to the background once mounted, which `mount` expects.

**systemd unit.** sacdfs sends `READY=1` when `NOTIFY_SOCKET` is set, so `Type=notify` works.
Run it in the foreground (`-f`) so that the service's main process is the one that serves the
mount; its log goes to the journal.

```ini
# /etc/systemd/system/sacdfs.service
[Unit]
Description=SACD images as DSF and WAV files
After=network-online.target
Wants=network-online.target

[Service]
Type=notify
ExecStart=/usr/local/bin/sacdfs -f --cache-mb 64 --format dsf /srv/sacd /mnt/sacd
ExecStop=/usr/bin/fusermount3 -u /mnt/sacd
Restart=on-failure

[Install]
WantedBy=multi-user.target
```

`systemctl enable --now sacdfs`, `journalctl -u sacdfs` for the log. Services that read the
mount should say `After=sacdfs.service` and `Requires=sacdfs.service`. A container that
bind-mounts the mount point must bind its parent directory instead
([deployment guide](deployment.md#media-servers-in-containers-on-a-client-host)). This
recipe was written from the mount helper's and systemd's documentation and the unit tests; it
was not run in a test VM.

## Troubleshooting

Run in the foreground with logging to see what sacdfs does: `sacdfs -f -v SOURCE MOUNTPOINT`
(`-vv` for debug detail).

A background mount (the default) logs nowhere, so its read errors and damaged-image warnings
are invisible. Start it with `--log-file /var/log/sacdfs.log` to keep the log (it records
the `mounted` line and warnings; add `-v` for more), or run it under systemd, where the log
goes to the journal: `journalctl -u sacdfs`.

- **`fusermount3: command not found`, or `failed to open /dev/fuse`:** install FUSE 3
  (package `fuse3`). In a container, pass `--device /dev/fuse --cap-add SYS_ADMIN`.
- **`fusermount3: option allow_other only allowed if 'user_allow_other' is set in
  /etc/fuse.conf`:** add that line to `/etc/fuse.conf`, or drop `--allow-other`.
- **`Transport endpoint is not connected`:** sacdfs was killed (or crashed) without
  unmounting. Starting sacdfs again on the same mount point, as the same user, clears it; or
  `fusermount3 -u MOUNTPOINT` (`umount -l MOUNTPOINT` as root).
- **"... is mounted there and its server has exited":** a dead mount that isn't sacdfs's, or
  another user's. Unmount it as the message says, then start sacdfs again.
- **"... is still mounted there after 10 s":** a live mount is on the mount point, usually a
  sacdfs still running (`pgrep -a sacdfs`). Stop it, or mount somewhere else.
- **"SOURCE ... is the mount point", "... is inside the mount point", "... holds images",
  "... choose another --index-cache":** see [Where it can mount](#options); choose another
  mount point.
- **"... is not empty: its files are hidden while sacdfs is mounted":** only a warning; use an
  empty directory to avoid it.
- **`Illegal instruction` at start:** the x86-64-v3 build on a CPU without AVX2. Use the
  generic build; it converts with AVX2 where the CPU has it anyway.
- **A directory or disc is missing:** files that are not SACD images, and folders with no
  `*.iso` file below them, are left out; a damaged image may be too. `-v` logs why. Images
  added after mounting appear without a remount.
- **A track is missing:** a track the disc cannot play (empty, or running past the end of the
  image) is left out; `-v` logs the reason.
- **`--pcm-cutoff wide needs --pcm-rate 176400 or 352800`:** 88.2 kHz cannot hold the wide
  filter's band; choose a higher rate or another cutoff.
- **"Stale file handle" over NFS:** the image changed, or the WAV options did; reopen the file.
  After restarting sacdfs under an export, restart the NFS server too (above).
- **A tagger or scanner is slow on WAV files:** it is reading the audio, which is converted on
  demand. Use `--format dsf` for it ([Tagging behaviour](#tagging-behaviour)).
- **Seeks are slow with images on a network share:** each read of an image is a round trip.
  Keep the seek index cache on (the default), and consider running sacdfs on the server
  instead ([deployment.md](deployment.md)).
- **Characters in file names look wrong over SMB:** see
  [File names over SMB](deployment.md#file-names-over-smb).

Bugs are reported as in [CONTRIBUTING.md](../CONTRIBUTING.md#reporting-a-bug), with the
`-f -vv` log.
