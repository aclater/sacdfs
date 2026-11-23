# sacdfs options

Every option of `sacdfs`. The first table lists those `sacdfs --help` shows, in its order (CI checks that
it matches `--help`); the [tuning options](#tuning) are left out of `--help`. What each one does in detail is in the [user guide](user-guide.md#options).

```
sacdfs [OPTIONS] SOURCE MOUNTPOINT
```

`SOURCE` is an SACD image file, or a directory searched recursively for `*.iso` files (any
case). `MOUNTPOINT` is an existing directory, normally empty, and not `SOURCE` or a directory
above it.

| Option | Meaning |
|---|---|
| `-f`, `--foreground` | Stay in the foreground and log to standard error. Without it sacdfs returns once the filesystem is mounted and keeps running in the background. |
| `--format LIST` | Which files to show: `dsf`, `wav`, or both, `dsf,wav` (the default). |
| `--pauses MODE` | `dropped` (the default): each file is the track's own range. `kept`: each file is the track with its pause (track 1 from the start of the area), as extractors that keep pauses write it; DSF and WAV both. |
| `--pcm-rate HZ` | Sample rate of the WAV files: `88200`, `176400` (the default) or `352800`. Always 24-bit. |
| `--tags DIR` | Tags files (one per disc, named by its disc key) whose tags override and add to the TOC's in the DSF and WAV files: genres, MusicBrainz IDs, corrected names, embedded art. Discs without one are unchanged ([user guide](user-guide.md#your-own-tags---tags)). |
| `--musicbrainz[=LOOKUPS]` | Look up, on MusicBrainz, the discs that lack a track title, the album or its artist, in the background, and write what matches to the `--tags` directory, which it needs and which must be writable. The same lookup as `sacdfs tags fetch`; off by default, and the only network use of a mount. At most 1 request per second, once per disc; no client request waits for it. `LOOKUPS` tries more when the barcode or catalogue number finds nothing: `name` also searches by album and artist, from the disc's text or the image's path (sends those names); `durations` also asks by the tracks' start times, for a disc of 4 tracks or more (sends the times); `name,durations` does both. As a `-o` option: `musicbrainz=name+durations` ([user guide](user-guide.md#in-the-background---musicbrainz)). |
| `--flatten` | Collapse each chain of folders that holds a single image into its outermost folder ([Layout](layout.md)). Top-level folders of `SOURCE` are never collapsed. |
| `--sidecars LIST` | Files passed through from `SOURCE`, byte for byte, next to the images: extensions, and folder names ending in `/` whose whole content passes. `default` (the default) is `jpg,jpeg,png,webp,m3u,m3u8,Artwork/,Scans/`; `default,cue` adds to it, `default,-webp` removes from it, `none` passes nothing ([Layout](layout.md)). |
| `--allow-other[=yes\|no]` | Let other users read the mount (`yes`, which a bare `--allow-other` means), or only the user who mounted, also when sacdfs runs as root (`no`). The default when sacdfs runs as root is `yes` (from 0.6.0; the mount is read-only). A user other than root needs `user_allow_other` in `/etc/fuse.conf` for `yes`. |
| `--index-cache DIR\|off` | Keep the persistent seek index in `DIR` (default: `$XDG_CACHE_HOME/sacdfs`, else `~/.cache/sacdfs`), or keep none with `off`, so that every mount starts with an empty one. The index remembers where frames are in each image, so that seeks after a remount skip most of their search. A folder that is really called `off` is written `./off`. |
| `--log-file FILE` | Append the log to `FILE` instead of standard error, at info level unless `-v` says more. Each start first writes a line such as `---- sacdfs 0.7.0 started 2026-11-18T21:30:05Z, pid 1234 ----`, so one session's lines can be told from the last one's. Without it, a background mount logs nowhere, except to the journal under systemd; use it (or `-f`) to see read errors and the `mounted` line ([troubleshooting](user-guide.md#troubleshooting)). |
| `-v`, `--verbose` | Log more: `-v` info, `-vv` debug, which includes the [read trace](user-guide.md#tracing-reads) (a line per open, read and close). `RUST_LOG` overrides it: `RUST_LOG=info,sacdfs::trace=debug` gives the read trace without the rest of the debug log. |
| `-h`, `--help` | Print help (`-h` for a summary). |
| `-V`, `--version` | Print the version. |

## Tuning

The defaults size themselves to the machine, so these are for a NAS with little memory, a
slow source or a particular player, and `sacdfs --help` does not list them. The container,
TrueNAS and Unraid settings map to them ([deployment guide](deployment.md)).

| Tuning option | Meaning |
|---|---|
| `--threads N` | DST decoder and PCM converter threads; `0` (the default) means one per CPU. Under a memory limit, the default is at most one per 64 MiB of it and at least 2 ([deployment guide](deployment.md#under-a-memory-limit)). |
| `--cache-mb N` | Size of the cache of decoded and converted frames, in MiB (default `32`, or `16` under a memory limit below 256 MiB). DSF and WAV reads share it. The default suits a NAS too; an idle mount gives the cache back after 60 s ([deployment guide](deployment.md#memory)). |
| `--listing-mb N` | Memory kept for what listed images hold (their TOCs and files), in MiB. Past it, the images not used lately are dropped, and read again when next used; a file open is never affected. `0` means no bound. Default: no bound, or 3/8 of the memory limit under one ([deployment guide](deployment.md#under-a-memory-limit)). |
| `--start-burst X` | How fast players read a track they start, in times real time (default `3.5`, `0` turns it off). Where sacdfs measures that it decodes a format and channel count slower than this, as on a 32-bit Atom NAS, it decodes a starting track's first seconds (`--start-burst-secs`) without waiting for the player's reads. A faster machine changes nothing ([user guide](user-guide.md#slow-cpus-and-players-starting-a-track)). |
| `--start-burst-secs SECS` | Seconds of audio decoded ahead when a track starts playing, 1 to 60 (default `8`), within a quarter of the cache. |
| `--pcm-cutoff KHZ` | Low-pass cutoff of the conversion to PCM: `40` (the default) or `30` kHz, or `wide` (about 80 kHz; with `--pcm-rate 176400` or `352800` only). |
| `--pcm-gain DB` | Gain of the conversion to PCM, from -60 to +24 dB (default `0`). Samples that would clip are clamped, and the log says so once per file. |
| `--no-multichannel-album-suffix` | Do not add ` (Multichannel)` to the album tag of the multichannel area's DSF and WAV files. By default they get it, so players and servers keep a disc's Stereo and Multichannel copies apart. |
| `--max-open-images N` | Most image files kept open at once, 1 to 1024 (default `64`), and at most half of the open files limit less 128. The least recently used is closed first, and any image unused for 5 minutes is closed. |
| `--request-timeout SECS` | A read, lookup or listing still waiting for the source after `SECS` seconds (default `30`) fails with an I/O error and a log line, instead of hanging its client; other images keep being served. `0` waits for good. |
| `--stop-grace SECS` | After SIGINT, SIGTERM or SIGHUP, sacdfs detaches the mount at once and keeps serving files clients still hold open for `SECS` seconds (default `10`), then exits even if they are open; their clients get I/O errors. It does not exit while the mount is still in the mount table, so no dead mount is left. `0` waits for the last file to close, however long (the behaviour before this option). Keep the service manager's stop timeout above it: [stopping](user-guide.md#stopping-with-files-open). |

## Deprecated spellings

These still work, on the command line, as `-o` mount options and in `SACDFS_OPTIONS`, and
log one line naming the new form. They are not listed in `--help`, and are kept through
1.0 at least. `sacdfs tags fetch` keeps its own `--lookup-name` and `--lookup-durations`.

| Deprecated | Use instead |
|---|---|
| `--lookup-name` | `--musicbrainz=name` |
| `--lookup-durations` | `--musicbrainz=durations` (both: `--musicbrainz=name,durations`) |
| `--no-allow-other` | `--allow-other=no` |
| `--no-index-cache` | `--index-cache off` |

`mount -t fuse.sacdfs` and fstab work too, with the options above as `-o` options (`allow_other`, `allow_other=no`, `musicbrainz=name+durations`, `index_cache=off`), and `Type=notify` units are supported: see [Mounting at boot](user-guide.md#mounting-at-boot-fstab-and-systemd).

Unmount with `umount MOUNTPOINT` as root, or as another user `fusermount3 -u MOUNTPOINT` (`fusermount -u` where only FUSE 2 is installed); SIGINT (Ctrl-C), SIGTERM and SIGHUP unmount too. If files are
still open, the mount is detached instead, and sacdfs stops when the last one is closed, or
after `--stop-grace` seconds if some stay open. What
each option does in detail, and the seek index cache, are in the
[user guide](user-guide.md#options).
