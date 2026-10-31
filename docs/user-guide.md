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
`MOUNTPOINT` is an existing, empty directory. The [README](../README.md#options) lists every
option; this is what they do.

**Running.**
- Without `-f` (`--foreground`), sacdfs returns once the filesystem is mounted and keeps
  running in the background, logging nowhere; with it, it stays in the foreground and logs to
  standard error. `-v` logs what sacdfs found (image counts, images and tracks not shown and
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
  number of open files: about 170 to 340 MiB in all with the default, under four players and
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

**The seek index cache** (`--index-cache DIR`, `--no-index-cache`). sacdfs learns where the
frames are in each image as it reads, and keeps what it learnt in `$XDG_CACHE_HOME/sacdfs`
(else `~/.cache/sacdfs`), so that a seek after a remount or a reboot needs about one read of
the image instead of four or five. That matters most when the images are on a network share,
where each read is a round trip.
- One small file per image (a few tens of KB for a disc played through), named by a hash of
  the image's path, never the path itself; 32 MiB in all, the least recently used files
  removed first. Written every 30 seconds if something changed, and at unmount. It never
  leaves the machine.
- An image whose size or modification time changes misses its entry. sacdfs never trusts the
  cache: every saved position is checked when it is used, so a stale or damaged cache can only
  make a seek slower, never wrong. Deleting the directory is always safe.
- `--no-index-cache` starts every mount with an empty index (used for measurements).

**Unmounting.** `fusermount3 -u MOUNTPOINT`, or SIGINT (Ctrl-C) or SIGTERM to sacdfs. If files
are still open, or an NFS server still exports the mount, the mount is detached instead, as
`umount -l` would: it disappears at once, the open files still read, and sacdfs stops when the
last one is closed.

**Restarting on the same mount point.** A sacdfs started while the previous one is still on
the mount point (it is stopping) waits up to 10 s for that mount to go, instead of mounting
over it, then fails with "already a FUSE mount". A stopping sacdfs only ever unmounts its own
mount, never one started on the same path since.

**CPU code paths.** The WAV converter picks its code when it starts: AVX2 (x86-64-v3) where the
CPU has it, SSE2 otherwise, with the same output bit for bit. `SACDFS_SIMD=baseline`, `v3` or
`v4` forces a path, for tests and measurements; the AVX-512 path (`v4`) is not the default
because it measured slower.

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

Both files of a track carry the same **ID3v2.4 tag**, made from the disc's text, as far as the
disc has it: title (TIT2), performer (TPE1, else the album artist), album artist (TPE2),
album (TALB), track n/N (TRCK), disc n/N for multi-disc sets (TPOS), composer (TCOM), date
(TDRC), ISRC (TSRC), genre (TCON), publisher (TPUB) and copyright (TCOP). Album fields fall
back to the disc's text. The WAV files hold it in an `id3 ` chunk (read by FFmpeg, TagLib,
mutagen, foobar2000 and mp3tag), plus a `LIST`/`INFO` chunk (title, artist, album, track
number, date, genre) for software that reads only that. Both come after the audio.

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
  A tag walk of 93 DSF files took 0.07 s from a local disk.
- Neither starts read-ahead: sacdfs decodes ahead only once a reader reads on into the audio.

**Library scanners.** A program that reads every audio file of a library (fingerprinting,
loudness or ReplayGain scans, waveform previews) converts every WAV file it reads, which costs
CPU and time for no new information. Point such programs at a mount with `--format dsf`, or
mount twice: one mount for them, one with WAV files for players.

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
- Adding or removing images does not change the numbers of the others. The design is in
  [design.md](design.md#file-identity-and-nfs-export).

## Times, sizes and free space

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

## Images that change, and damaged images

- **New or removed images** appear or disappear only after a remount: the directory tree is
  read once, at mount time.
- **An image file that changes while mounted** (another size, modification time or file) is
  opened again, at most a second after the change. An image whose size changed gives I/O
  errors until the next mount, since its directory still shows the old tracks.
- **A damaged image** shows up as missing directories or files, filled audio (missing frames
  as 0x69, invalid DST frames as 0x55, with a warning in the log) or an I/O error on one
  file. It never stops the mount: every image is treated as untrusted input.

## Troubleshooting

Run in the foreground with logging to see what sacdfs does: `sacdfs -f -v SOURCE MOUNTPOINT`
(`-vv` for debug detail).

- **`fusermount3: command not found`, or `failed to open /dev/fuse`:** install FUSE 3
  (package `fuse3`). In a container, pass `--device /dev/fuse --cap-add SYS_ADMIN`.
- **`fusermount3: option allow_other only allowed if 'user_allow_other' is set in
  /etc/fuse.conf`:** add that line to `/etc/fuse.conf`, or drop `--allow-other`.
- **`Transport endpoint is not connected`:** sacdfs stopped (or was killed) without
  unmounting. `fusermount3 -u MOUNTPOINT`, then mount again.
- **`Illegal instruction` at start:** the x86-64-v3 build on a CPU without AVX2. Use the
  generic build; it converts with AVX2 where the CPU has it anyway.
- **A directory or disc is missing:** files that are not SACD images, and folders with no
  `*.iso` file below them, are left out; a damaged image may be too. `-v` logs why. Images
  added after mounting need a remount.
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
