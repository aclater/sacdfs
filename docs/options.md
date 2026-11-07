# sacdfs options

Every option of `sacdfs`, in the order `sacdfs --help` gives them (CI checks that this table
matches `--help`). What each one does in detail is in the [user guide](user-guide.md#options).

```
sacdfs [OPTIONS] SOURCE MOUNTPOINT
```

`SOURCE` is an SACD image file, or a directory searched recursively for `*.iso` files (any
case). `MOUNTPOINT` is an existing directory, normally empty, and not `SOURCE` or a directory
above it.

| Option | Meaning |
|---|---|
| `-f`, `--foreground` | Stay in the foreground and log to standard error. Without it sacdfs returns once the filesystem is mounted and keeps running in the background. |
| `--threads N` | DST decoder and PCM converter threads; `0` (the default) means one per CPU. Under a memory limit, the default is at most one per 64 MiB of it and at least 2 ([deployment guide](deployment.md#under-a-memory-limit)). |
| `--cache-mb N` | Size of the cache of decoded and converted frames, in MiB (default `32`, or `8` under a memory limit up to 512 MiB). DSF and WAV reads share it. The default suits a NAS too; an idle mount gives the cache back after 60 s ([deployment guide](deployment.md#memory)). |
| `--listing-mb N` | Memory kept for what listed images hold (their TOCs and files), in MiB. Past it, the images not used lately are dropped, and read again when next used; a file open is never affected. `0` means no bound. Default: no bound, or 3/8 of the memory limit under one ([deployment guide](deployment.md#under-a-memory-limit)). |
| `--format LIST` | Which files to show: `dsf`, `wav`, or both, `dsf,wav` (the default). |
| `--pauses MODE` | `dropped` (the default): each file is the track's own range. `kept`: each file is the track with its pause (track 1 from the start of the area), as extractors that keep pauses write it; DSF and WAV both. |
| `--pcm-rate HZ` | Sample rate of the WAV files: `88200`, `176400` (the default) or `352800`. Always 24-bit. |
| `--pcm-cutoff KHZ` | Low-pass cutoff of the conversion to PCM: `40` (the default) or `30` kHz, or `wide` (about 80 kHz; with `--pcm-rate 176400` or `352800` only). |
| `--pcm-gain DB` | Gain of the conversion to PCM, from -60 to +24 dB (default `0`). Samples that would clip are clamped, and the log says so once per file. |
| `--no-multichannel-album-suffix` | Do not add ` (Multichannel)` to the album tag of the multichannel area's DSF and WAV files. By default they get it, so players and servers keep a disc's Stereo and Multichannel copies apart. |
| `--tags DIR` | Tags files (one per disc, named by its disc key) whose tags override and add to the TOC's in the DSF and WAV files: genres, MusicBrainz IDs, corrected names, embedded art. Discs without one are unchanged ([user guide](user-guide.md#your-own-tags---tags)). |
| `--flatten` | Collapse each chain of folders that holds a single image into its outermost folder ([Layout](layout.md)). Top-level folders of `SOURCE` are never collapsed. |
| `--sidecars LIST` | Files passed through from `SOURCE`, byte for byte, next to the images: extensions, and folder names ending in `/` whose whole content passes. `default` (the default) is `jpg,jpeg,png,webp,m3u,m3u8,Artwork/,Scans/`; `default,cue` adds to it, `default,-webp` removes from it, `none` passes nothing ([Layout](layout.md)). |
| `--allow-other` | Let other users read the mount. Needs `user_allow_other` in `/etc/fuse.conf`. |
| `--index-cache DIR` | Keep the persistent seek index in `DIR` (default: `$XDG_CACHE_HOME/sacdfs`, else `~/.cache/sacdfs`). It remembers where frames are in each image, so that seeks after a remount skip most of their search. |
| `--no-index-cache` | Keep no persistent seek index: every mount starts with an empty one. |
| `--max-open-images N` | Most image files kept open at once, 1 to 1024 (default `64`). The least recently used is closed first, and any image unused for 5 minutes is closed. |
| `--request-timeout SECS` | A read, lookup or listing still waiting for the source after `SECS` seconds (default `30`) fails with an I/O error and a log line, instead of hanging its client; other images keep being served. `0` waits for good. |
| `--log-file FILE` | Append the log to `FILE` instead of standard error, at info level unless `-v` says more. Without it, a background mount logs nowhere, except to the journal under systemd; use it (or `-f`) to see read errors and the `mounted` line ([troubleshooting](user-guide.md#troubleshooting)). |
| `-v`, `--verbose` | Log more: `-v` info, `-vv` debug. `RUST_LOG` overrides it. |
| `-h`, `--help` | Print help (`-h` for a summary). |
| `-V`, `--version` | Print the version. |

`mount -t fuse.sacdfs` and fstab work too, with the options above as `-o` options, and `Type=notify` units are supported: see [Mounting at boot](user-guide.md#mounting-at-boot-fstab-and-systemd).

Unmount with `fusermount3 -u MOUNTPOINT` (`fusermount -u` where only FUSE 2 is installed); SIGINT (Ctrl-C), SIGTERM and SIGHUP unmount too. If files are
still open, the mount is detached instead, and sacdfs stops when the last one is closed. What
each option does in detail, and the seek index cache, are in the
[user guide](user-guide.md#options).
