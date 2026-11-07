# Changelog

All notable changes to sacdfs are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project follows
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Players on the same host read the mount directly, with no SMB hop (#186).** The container's mount
  can be shared through a host directory bound over `/srv` with `rshared`, and a player container
  binds that directory with `ro,rslave`. Validated on TrueNAS SCALE 25.10 (Docker, Navidrome) and on
  rootful Podman (Jellyfin): the player sees the tree again by itself after a restart, an update or a
  crash of sacdfs, without a restart of its own; a file open during a restart fails once the runtime
  stops sacdfs. Rootless Podman can't share the mount this way. See
  [deployment](docs/deployment.md#players-on-the-same-host-no-smb-hop).
- **The container makes `/srv/sacd` itself** when a host directory is bound over `/srv`, so that
  directory can start empty. A dead mount left there by a killed sacdfs doesn't stop the start: sacdfs
  detaches it, as before. CI checks both on every pull request (`deploy/ci-same-host-test.sh`).
- **`sacdfs tags fetch IMAGE...`: tags from MusicBrainz, on demand** (#185, phase 2). It finds the
  disc's release by barcode or catalogue number, confirms it by track count, durations and ISRCs,
  and writes titles, artists, album, date and MusicBrainz IDs to the disc's file in the tags
  directory. `--lookup-name` also searches by album and artist, and `--lookup-durations` by the
  tracks' start times. `--dry-run` shows what would be written. No match, or an ambiguous one,
  writes nothing and says why, and the exit status is 2. Values in a tags file win, and the disc's
  own text stays. Nothing goes on the network unless you run this command: a mount never does. It
  sends the numbers, names or times to MusicBrainz, at no more than 1 request per second
  (docs/user-guide.md, "From MusicBrainz"). Run on 9 real discs by barcode or catalogue number: 7
  matched the right release (checked by hand), and 2 are not on MusicBrainz and were refused.

### Changed

- **A memory limit no longer gets sacdfs killed while listing (#237, #231).** Under a cgroup limit, what
  listed images hold is kept to a budget (`--listing-mb`, `SACDFS_LISTING_MB`, default 3/8 of the
  limit): images not used lately are dropped and read again when next used, with the same names, inode
  numbers, sizes, times and bytes. 5,000 and 20,000 images now list at 256 MiB, where 0.4.0 and 0.4.1-rc.2
  were killed. A second pass over a library past its budget takes longer, as TOCs are read again.
- **Defaults follow the limit.** Under a limit up to 512 MiB the cache defaults to 8 MiB, as a 6-channel WAV player then keeps real time
  during whole-file scans, at the cost of scans 2.4 to 2.6 times slower (#230), and decoder
  threads to at most one per 64 MiB of the limit (at least 2). The container no longer sets
  `SACDFS_CACHE_MB` by itself. With no limit, nothing changes.
- **A warning at startup** when the library won't fit in memory as listed, naming the size that would.
- **A memory watch** under a limit: past 75% of the limit listings shrink, past 85% the cache and idle
  decoders go, past 93% new listings and opens fail with `ENOMEM`, while files already open go on.
- **NFS-Ganesha's cache follows the limit, in the container.** Its cache of entries is kept to what an
  eighth of the limit holds (16,384 entries at 256 MiB). At its default, a client going through 5,000
  images took it to about 180 MiB of a 256 MiB limit, which sacdfs can't free, and new requests kept failing.
- **`--tags` with covers under a limit.** Under a limit with a tags directory, blocks from 128 KiB up keep
  their own mappings, as the heap otherwise held over 100 MiB of freed covers. Listings are also kept to
  their budget as each directory is listed.
- **`--tags`, with or without a limit:** only the last 16 covers read stay in memory, not every disc's for
  as long as the mount lasts. A disc listed again reads its cover again, so a cover image edited during a
  mount can show then, with no new generation for its files.
- File nodes are smaller, by about 4.4 KiB per image at about 25 files per image, for every library.
- **The share says it is read-only, and has the size of its images (#217).** Over SMB, files and
  folders now carry the read-only attribute (`map readonly = yes`; gvfs's `can-write` is FALSE for
  files, the Linux kernel client shows no write bit). `df` and clients now see the total size of the
  images found at mount instead of the capacity of the disk they're on, still with 0 free, and
  the source's size only when there are no images. Untested on Windows, macOS, Roon and Plex; to go back
  on the attribute, `SMB_EXTRA_GLOBAL=map readonly = no`.
- **scarletbook and scarletbook-pcm 0.5.8 -> 0.5.9**. 0.5.9 makes
  DST decoding faster (about 1 to 14 % in scarletbook's own figures, on Zen 2 only), changes the DSF
  partial-row code, and adds nothing sacdfs can use to delete code of its own: the new
  `with_dst_lanes` is a setting to measure with, and the per-track DSF writer recipe is documentation
  (the writer is planned for 0.6.0, scarletbook-rs#202). Audio bytes are unchanged: every DSF and WAV file
  served from the 136-image corpus (914 files) and from the five bench discs (186 files) has the same
  SHA-256 as with the 0.4.1-rc.3 build (0.5.8), and the bench-disc hashes equal 0.4.0's. The
  library's notes warn that a DSF read of part of a row now asks for all of that row's positions, so on
  damaged media a read that used to succeed can return an I/O error. The extra work the library reports for DST reads under 4 KiB was not measured
  through the mount.
- **A tags file's title still doesn't rename a file** (#185, phase 2, now decided for lookups): a
  track given a title by `sacdfs tags fetch` stays `NN - Track NN.dsf`, with the title in its tag.
- **`sacdfs tags import` leaves out a generated `Track NN` title, and `sacdfs verify` doesn't report
  it as lost** (#185). An extract copied from a 0.4.1 mount has that text, and an import stored it
  as if edited by hand, which would have kept a lookup's title out. A title of any other form,
  `Track 3` for instance, is still kept. `sacdfs tags fetch` replaces a `Track NN` that an earlier
  import stored.
- **The binary is about 3 MB larger** (7.2 to 10.2 MB in a release build on x86-64): it now holds an
  HTTPS client with its own root certificates (ureq, rustls), used only by `sacdfs tags fetch`.
- **`sacdfs tags fetch`'s duration limits come from real discs:** by number, 15 s on a track and 5 s on
  average; by name, 6 s and 3 s. The limits for `--lookup-durations` stay as judged. It also no longer
  takes a SACD layer for a CD layer.
- **`sacdfs tags fetch` on a busy service.** It obeys `Retry-After` in full, up to an hour, waits
  longer between discs that get no answer, tries those again at the end of the run, and ends at once
  on a 403 or a request to wait longer than an hour.
- **The soak's memory verdict no longer fails a run for one step ([#247](https://github.com/aclater/sacdfs/issues/247)).**
  Memory in healthy runs moves in single steps of 3 to 6.5 MiB, up or down, and the line through the
  second half of a 2-hour run read one as a climb of 8 to 9 MiB/h (limit 4.4). The trend is now the rate
  left after the largest single step between six window medians is set aside, so a step followed by a
  plateau passes and a steady climb still fails (10 MiB/h fails; a 2-hour run catches about 5 MiB/h and
  up, a longer run less). Replayed over 21 recorded runs and 4 two-hour cuts of long ones (0.4.0, 0.4.1-rc.2, rc.3), the runs that failed on a step or
  on a three-sample end level now pass and the others keep their verdict. `--trend-floor-mb` default 4 to 3. The thread check now lets a short spike pass the
  `--thread-spike` line (up to twice it; failing only if three samples in a row, `--spike-hold`, stay above the first line), and its end level is the median of the last
  sixth rather than of the last three samples. `scripts/test_soak.py` tests the verdict.

### Fixed

- **NFS clients keep their handles when the container restarts, under any runtime (#218).** Under
  Podman, nearly every restart gave `Stale file handle` on open files, `stat` and `ls`, until a
  remount; under Docker it could happen too. NFS-Ganesha's handles no longer hold the FUSE mount's
  device number. Clients of an earlier release go stale once, when the first fixed container
  starts: remount. CI now restarts the container under Docker and Podman, also with the device number
  changed on purpose (`deploy/ci-restart-test.sh`).
- **Clients retrying through a long hang of one image no longer stall the other images (#241).** A
  read that fails with `EIO` after the request timeout keeps its thread until the source answers, so
  clients that kept retrying on a hung image took one more read thread each time. Once all 64 were
  blocked, reads of healthy images failed too, in both formats, until the source answered: with 8
  clients, 2 decoder threads and a 2 s timeout, that took about 24 s. Now, once 4 reads of an image
  are still blocked past the timeout, its new reads fail with `EIO` at once, without taking a thread,
  until one of the blocked reads finishes. With the same test the hung image held at most 8 read
  threads over 60 s, and every healthy DSF and WAV read during the hang succeeded. Reads of an image
  that is slow but answers within the timeout are not affected, however many there are. A client
  retrying without a pause now gets its errors at once rather than once per timeout. Logged once
  when an image starts failing its reads this way and once when it reads again.

### Documentation

- `docs/deployment.md`, "SMB and restarts": what each client does when the container restarts. macOS
  needs a remount; the durable-handle message in Samba's log is expected, and no setting tried
  makes those handles survive a restart (#204).

## [0.4.1] - 2026-11-07

Tested as three release candidates. On rc.1 the scripted servers (Jellyfin, Navidrome, Lyrion, MPD,
MinimServer) were run against 0.4.0 side by side, and Roon, Plex, Windows (Explorer, PowerShell,
Robocopy), foobar2000, JRiver Media Center, Linux NFS and SMB mounts and the FFmpeg family of tools
were run on it; the results are in docs/clients.md. rc.2 adds the memory fix (#224) and
`SACDFS_MOUNT_TIMEOUT` (#232). rc.3 adds the fix for #238, which the 2-hour soak on rc.2 exposed (see
Fixed). The rc.2 gate, on 2026-11-06:

- **Memory,** a real library of 4,902 DSF files on a 32-core machine, listed / after a head-and-tail
  read of every file / after 10 minutes idle: 0.4.0 29.0 / 79.5 / 35.2 MiB, rc.1 33.2 / 95.1 / 54.0,
  rc.2 27.2 / 72.7 / 33.0.
- **Mount time,** whole library, warm NFS, fresh index cache, medians of 5 alternating rounds: 0.4.0
  5.00 s, rc.1 0.76 s, rc.2 1.02 s. First listing 1.55 / 1.36 / 1.42 s. rc.2 mounts about 0.26 s
  slower than rc.1 (accepted); the arena limit is the suspected cause, not confirmed. A cold-cache
  mount was not measured on rc.2.
- **Soak:** the 60-minute run passed (224,491 reads, 686 GB, 0 unexpected errors, 19 stalls, 11
  remounts, memory +1.4 MiB/h). The 2-hour soak on rc.2 failed on #238
  (fixed in rc.3), so it is not counted as a pass. On rc.3, every soak had 0 unexpected errors, against
  0 to 14 for rc.2 run beside it. Two hours passed on one machine; on another, two 2-hour runs failed
  the memory trend check (+8.02 and +8.90 MiB/h, limit 4.4) on single steps of about 5 to 6.5 MiB. Longer
  runs settled it: 6 hours on that machine passed at +0.08 MiB/h, and 8 hours on a third passed at +1.14
  MiB/h, both flat, while rc.2 failed both on unexpected errors. What the steps are is not known, nor why
  rc.3 sits a steady 4 to 5 MiB above rc.2 on the one machine (#247).
- **Share paths:** SMB 3.1.1 and NFS 4.2 listings were correct; 10 files were byte-identical over
  both; writes were refused; SMB reconnected after a restart.
- **Container:** `SACDFS_MOUNT_TIMEOUT` and the health check passed on the rc.2 image under Docker and
  rootful Podman. `docker compose` was not run. The timeout expiring was shown on a local build of
  the same code, not on the published image.
- **Upgrade from rc.1 to rc.2** on the NAS apps: identical names and counts, and no re-read seen for
  12 minutes after the move. FUSE request counts per kind were not obtained.
- **Not verified:** the arm64 image at run time; a cold-cache mount.

### Upgrade notes

What a scanner may see after the move from 0.4.0 to 0.4.1, with counts from one library of 4,902
tracks. These are what was seen, not a prediction for yours.

- **Cover images are added inside the area folders**, so a scanner sees additions: 359 files on
  that library.
- **More folders may be hidden than you expect.** A folder tree with no audio is hidden: on that
  library 11 folder trees (59 folders, about 590 images and text files) have no audio and no longer
  show.
- **A scanner's one-time cost can be large.** Plex re-read 18.25 GiB once, over 140 s, after the
  upgrade; a no-change scan after that read nothing.
- **Discs without text change once; other discs don't.** Files of a disc that lacks a title, album
  or artist get new tags, a new size, a modification time one step (7 hours) later and a new NFS
  generation; those with no title also get a new name. The output version moves for those files
  only, as with a tags file, so `DSF_VERSION` and `WAV_VERSION` stay. All other files keep their
  bytes, time and generation.
- **What scanners see.** The time of every folder with a disc below it moves one step once, because
  knowing which discs lack text takes reading each image's table of contents, which the mount
  doesn't do. So a scanner that skips unchanged folders (Roon) should walk those folders once, and
  any scanner that compares sizes and times should re-read only the changed files. Checked by
  comparing every file's and folder's size and time over the mount with 0.4.0's, on a library of
  4,890 tracks: 259 files changed, each with every folder above it moved. Roon and Plex were then
  run through the upgrade on rc.1 (docs/clients.md); Navidrome, LMS and MPD were run on rc.1 and on
  0.4.0 with fresh libraries, not upgraded in place. A renamed track should look to a scanner like a
  removal and an addition, so its play counts and edits in the player may not carry over (also untested).
- **A tags file that sets a title** changes the tag and not the file name, as before: such a track
  is named `NN - Track NN`.
- `sacdfs verify` and `sacdfs tags import` don't count text from the path as the disc's, so they
  still report and keep an extract's album or title that happens to match its folder's name.
- **`sacdfs tags import` now exits 2 when some extracts weren't imported (#198).** It exited 0 even
  with ERROR rows or `failed:` lines, so a script could not tell. It now follows `sacdfs verify`: 0
  when every extract was handled, 2 when any didn't match or couldn't be read, with the rows
  still written and the count in the summary, and 1 when it couldn't run. A script that ran it
  over extracts that don't all match, and took any non-zero status as a failure, now sees one.
- **Known limit: NFS under rootful Podman needs a remount after a container restart (#218).** An
  existing NFSv4.2 mount gets "Stale file handle" on files it had open, and on `stat` and `ls` of
  them, after `podman restart`, on 0.4.0, rc.1 and rc.2. A fresh mount works and shows the same
  inodes, times and sizes. Under Docker 29 the handles survive, and SMB reconnects by itself under
  both. The fix is planned for 0.5.0 (#221). See "NFS and restarts" in docs/deployment.md.

### Added

- **Names for discs without text, with no network** (#185, phase 1; the lookups are later
  phases). A track without a title is now tagged `Track NN` and named `NN - Track NN.dsf`, where
  0.4.0 served `NN.dsf` with an empty title. A disc without album or artist text takes them from
  its image's path, by a narrow rule, since a wrong artist is worse than none: `Artist - YYYY -
  Album [...]` in the image's or its folder's name, or a `YYYY - Album [...]` folder with an image
  name that agrees; otherwise the album is the image's file name and there is no artist (the table
  is in docs/user-guide.md, "Tagging behaviour"). A tags file wins over all of it. Audio is
  unchanged.
- **MPD finds folder art** (#148). When an image's art has no file named `cover.*`, one of the
  images (`folder.*`, then `front.*`, then the first by name) is also listed as `cover.jpg`
  (`cover.png`, `cover.webp`) inside each `Stereo` and `Multichannel` folder, as the same file
  (the same inode), after the tracks. MPD 0.24.15 found art in 6 of 6 areas with a cover, against
  1 of 6 with the 0.4.0 release on the same discs. It follows `--sidecars`; no audio file changes.
- Fuzz targets for sacdfs's own parsers and paths (#161): tags files, sidecar lists and paths,
  names, the ID3, DSF and WAV writers, the tree, and the `verify` readers, in `fuzz/` (see
  fuzz/README.md). A CI job runs each for about 60 s on every pull request and 20 minutes every
  week. It is not a required check.

- **`SACDFS_MOUNT_TIMEOUT` (container, #232).** How long the container waits for sacdfs to mount, in
  seconds (default 300, a whole number from 1 to 86400; anything else stops the container at once
  with a message). It was a fixed 60 s, which a large library on a slow disk exceeds: on an emulated
  single disk with nothing cached, 122,676 files took 194 s to mount on 0.4.1-rc.1. The health
  check's start period is now 300 s to match, in the `Containerfile`, `deploy/compose.yaml` and the
  commands in the guides.

### Fixed

- **A hung image's WAV reads no longer take decoder threads from other images (#238).** Reading a
  track as WAV read the image on a thread of the decoder pool, which all images share. While one
  image's source hung (a disk in error recovery, a hung share), its WAV reads held decoder threads
  for the length of the hang. A read of a healthy disc could then wait until the request timeout and
  fail with `EIO`; once the hung image's reads held every decoder thread, all DST and WAV reads on the
  mount did. WAV reads now read the image on their request thread, as DSF reads do, and only decoding
  and conversion use the pool. Present since 0.4.0 at least. A mount with `--format dsf` was not
  affected by this. Audio bytes are unchanged.

  **A long hang can still stop the whole mount (#241).** A read that has failed with `EIO` keeps its
  request thread until the source answers, so clients that keep retrying on the hung image take one
  more thread each time. Once 64 are blocked, reads of healthy images wait and fail too, in both
  formats, until the source answers. With 8 clients retrying at the default 30 s timeout, that took
  about 6 minutes on 2 decoder threads. Healthy reads recover at once when the source comes back.
- **`sacdfs verify` could exit 0 with files not checked (#196).** A panic while checking a file
  ended that worker silently, so the report and summary left the file out, and with one thread
  every file after it too. The exit status said that all was identical. A panic now gives that file
  an ERROR row, which `--resume` checks again, and the exit status is 2. `sacdfs tags import` runs
  the same check, so it no longer skips such a file without a row in its report.

  **Upgrade note.** On 0.4.0 and earlier, don't delete extracts on the exit status alone (tested
  on the 0.4.0 release). List them by their IDENTICAL rows, as the user guide does, and check that
  the report has one row per file.
- **A stopped `verify` no longer leaves a report that looks finished (#196).** Rows go to
  `FILE.partial`, and `FILE` appears only when every file has been checked. A report already at
  `FILE` moves to `FILE.previous` once a run starts writing, and is removed when it finishes.
  `--resume` reads all three. Anything that reads `FILE` during a run should read `FILE.partial`.
  `-o` naming a descriptor (`/dev/stdout`, `/dev/stderr`, `/dev/fd/N`, even redirected to a
  regular file) or a named pipe now works as a stream, where only the exit status tells whether
  the run finished. On 0.4.0 `-o /dev/stdout` and the like wrote the rows, then always exited 1
  when the report could not be rewritten beside them, and `>>` lost what the file held; the rows
  are now added to it (#202).
- **Folders with no SACD image are hidden, art included** (#175). A folder whose only `.iso`
  files were DVD-Video or Blu-ray images showed on the share holding just its `Artwork/` folder,
  and its parents showed too; a box set mixing an SACD with DVD discs showed the DVD disc
  folders. The mount's walk now reads the 8 signature bytes of each `.iso` and leaves out
  files that are not SACD images. On a real library (414 `.iso` files, 20 of them not SACDs)
  the share lost 649 entries, 4 top-level folders among them, and not one DSF file. A file
  that is not an SACD image yet (a download filling a file made at full size) comes back within
  the 2-second refresh once the TOC is written; a rescan reads no image it has looked at
  before unless its size or time changed. The mount's walk reads the top-level folders on
  16 threads, so the whole-library mount took 0.65 to 0.72 s against 3.6 to 3.8 s on 0.4.0
  (NFS cache warm; cold not measured).
- Found by those fuzz targets; none of these changes a served file (#161):
  - `sacdfs verify` and `sacdfs tags import` no longer panic on an ID3v2.3 tag whose extended
    header claims 4 GiB, and `sacdfs verify` no longer panics on a DFF file whose sound chunk
    claims 2^61 bytes or more: these are read as damaged files.
  - A repeated name with an extension of 243 bytes or more now gives `name (2)`, cut to fit,
    without keeping the extension apart. In the library such a name could pass the 255 bytes the
    kernel accepts. No such name was found to be reachable in a mount, where colliding names are
    shorter: they gave a usable name before as well.
  - A `verify` report's `loses` column is written the same after `--resume` when frame keys hold
    `,` or `|`.
- **Memory after the start-up walk (#224).** 0.4.1-rc.1 held 22 to 25 MiB more than 0.4.0 on a
  library of 414 images (95 to 98 MiB against 73 to 77 MiB after a scan; 100 MiB against 78 MiB for
  an import replay): the walk that hides folders with no SACD (#175) runs on 16 threads before the
  malloc arena limit (#73) was set, and each thread kept an arena. The limit now comes first. A build
  from source with the change held 73.8 to 77.1 MiB on the whole library (4,902 DSF files; 0.4.0:
  76.0 to 83.4 MiB, rc.1: 93.3 to 98.6 MiB). On the rc.2 binary, on the same library, the figure
  after a head-and-tail read of every file was 72.7 MiB (0.4.0: 79.5, rc.1: 95.1). The whole-library
  mount, warm NFS, medians of 5 alternating rounds, took 1.02 s on rc.2 against 0.76 s on rc.1 and
  5.00 s on 0.4.0, so the change costs about 0.26 s of rc.1's speed (suspected cause: the arena
  limit; not confirmed). In the build-from-source runs, a library on a share mounted in 1.6 s against
  0.4.0's 3.1 s (medians), and a warm local one in 0.2 s against 0.15 s. These come from different
  libraries and machines, so they are not combined.

- **The weekly leak workflow is fixed (a broken check, no leak found).** It had failed on
  every job. The memcheck job flagged a once-per-process, 55-byte mount that `serve` keeps on
  purpose (its suppression matched the legacy `_ZN...` symbol names and stopped matching with
  Rust 1.99's v0 names), an uninitialised read inside the `nix` crate that fuser calls once
  per mount, and the system tools the tests start (uutils `mkfifo`, `ls`, `df`). The soak job
  could not mount: `fusermount3` was refused ("Permission denied") in its work directory
  under `/var/tmp` on the Ubuntu runner (probably its AppArmor profile; not checked). The
  soak now mounts under the runner's home, where it works (a 20-minute run passes), and `scripts/soak.py` prints
  sacdfs's stderr when the mount fails.
- **The soak no longer ignores read errors.** `scripts/soak.py` counted every failed read as
  "expected" and the verdict never looked at it (8% of the reads in one run). It now sorts
  them by path and errno into the ones its churner and staller cause and everything else, and
  fails on any other. See `docs/development.md`.

### Changed

- **scarletbook and scarletbook-pcm 0.5.7 → 0.5.8** (#206). The release fixes the saved seek
  index loader (`AreaIndex::from_bytes` now refuses a form the library never writes, so saved
  indexes keep loading) and reports no speed change. Output bytes are unchanged: the DSF and PCM
  golden tests and the corpus tests pass, and every DSF and WAV file served from the 136-image
  corpus and from the five bench discs hashes the same as with 0.4.0. Saved seek indexes: a
  mount of 4 890 DSF files over 394 images with a real cache of 399 files loaded all 32 454 saved
  hints on both 0.5.7 and 0.5.8, and discarded none. Straight reads through the mount, 0.4.0 →
  this branch (medians of 11, alternating, 2026-11-05): DST stereo DSF 8.75 → 8.72 s, DST
  6-channel DSF 28.28 → 28.30 s, plain-DSD DSF 2.83 → 2.55 s (runs overlap), DST stereo WAV
  9.40 → 9.43 s, mount and first listing of the corpus 0.087 → 0.091 s.
- `scripts/soak.py` judges threads by the median and end level of each half, with a hard
  ceiling (`--thread-spike`), instead of the maximum of each half. A single sample taken during
  an induced stall no longer fails a run, and accumulating threads still do (#179).

### Continuous integration

- Pull requests that touch the code run an informational benchmark: base and head read DST stereo,
  DST multichannel and plain DSD through the mount, and a slowdown over 10% is flagged in the job
  summary and as a warning. Not a required check (#162).

- Skipped tests now show (#207). A guard that returned early reported as a pass; guards now print
  a `SKIPPED` line, and the test jobs count skips in their summary and fail if they differ from
  `tests/expected-skips.json` (`scripts/check_skips.py`, see docs/development.md).

### Documentation

- **Install guides for ASUSTOR and TerraMaster, and results for Synology, QNAP and UGREEN's kernel.**
  Checked on ADM 5.1.4, TOS 7.0.1422, DSM 7.4.1 and QuTScloud 5.2.9 in VMs, not on the vendors'
  hardware. The release binary mounts and serves correctly on all of them but QuTScloud, which has
  glibc 2.21 ([#244](https://github.com/aclater/sacdfs/issues/244)). The container ran on ADM with
  Docker CE. The user guide's NFS section now says `--allow-other` is needed even when sacdfs runs
  as root.
- **Small NAS known limit re-measured on rc.3** (#230). With a fresh cache, the starvation of a
  6-channel WAV player by whole-file scanners also reaches 4 cores; `SACDFS_CACHE_MB=8` kept the player
  at real time in every cell tested. The earlier 4-core figures (0.31 s, 11.2 s) were taken with a
  player re-reading a position and are replaced. Figures are given per rig, not as N100-class.

- The TrueNAS SCALE guide is re-validated with the published 0.4.0 image, and notes the harmless
  `parse_dfs_path_strict ... is not ours` Samba lines, that `NFS_CLIENTS` takes one address, and
  that TrueNAS's "SMB share" app storage has no port field (#176).
- The crates.io install (`cargo install --locked sacdfs`) is validated on 0.4.0: run in a clean
  `rust:1.99.1` container with no other packages, it installs a working `sacdfs` (#155).
- **Docs: "Small NAS: sizing"** in the deployment guide: the container limits that worked (2 Gracemont E-cores and 256 MiB for about 2,000 images;
  4 cores and 512 MiB for about 5,000), a sizing rule, where
  the limits break, idle behaviour and one known limit (#230, #231). Measured on 0.4.1-rc.1. Memory and
  idle-size claims elsewhere now say they hold for a small library.
- **Docs: client notes for 0.4.1-rc.1** on Roon, Plex, Windows (Explorer, PowerShell, Robocopy, with
  Robocopy speed), foobar2000 and JRiver Media Center. Windows can use another SMB port with `/TCPPORT:`;
  the deployment guide said it could not.

## [0.4.0] - 2026-11-03

Tested as five release candidates. On rc.2 to rc.5, a TrueNAS SCALE NAS ran the TrueNAS guide
as written, with Roon and Plex imports. A Fedora client with SELinux enforcing found no errors
from `ls -l` or `getfattr -d` over NFS on rc.5. A Roon Force Rescan over SMB sent no
extended-attribute requests on rc.5 (21,767 on rc.4). A 2-hour soak on real disc images, with a
new album name on every add, levelled off (second-half trend −0.89 MiB/h). Jellyfin, Navidrome,
LMS, MPD and MinimServer were re-checked on rc.2. Against 0.3.0, peak memory of an import-like
scan fell from 109 to 45 MiB, and the first listing of a 200-image library was 55 % faster, with
no read slower by more than noise (docs/clients.md, #151).

### Added

- **sacdfs on crates.io, from 0.4.0** (#155): `cargo install --locked sacdfs`. The release
  workflow publishes final versions (not pre-releases) through crates.io trusted publishing,
  after the GitHub release; the first version is published by hand. The crate holds only
  the sources, Cargo.toml, Cargo.lock, README.md, COPYING and CHANGELOG.md. The crates.io
  install path is not yet validated.
- Leak hunting tools (#151). `tests/no_growth.rs` runs in CI and checks that mount cycles leave no
  heap behind. Off CI, `scripts/soak.py` runs hours of reads and churn and judges the memory trend,
  and `scripts/leak-check.sh` runs valgrind memcheck over the test suite and massif over a soak.
  A weekly workflow runs memcheck and a short soak. See `docs/development.md`.

### Changed

- **Shorter README** (#159, docs). The README is now a two-minute read: what sacdfs is, a
  quick start (container, then a build), links to the install guides and client notes, a
  pointer to `sacdfs verify`, and a docs index. Its details moved, unchanged, to
  docs/options.md (the options table; the `readme options` CI check now reads it there),
  docs/layout.md (layout, formats and quality), docs/limits.md, docs/install/binary.md
  (release binaries, crates.io, from source; the download example now names 0.3.0), the
  user guide (the quick start with its tips) and the deployment guide (where to run it).

- **Less memory** (#150). The default `--cache-mb` is now 32 (was 256; `SACDFS_CACHE_MB` in the
  container too): the smallest size whose reads, seeks and scans stay within 5 % of a 256 MiB
  cache. On the bench images a Roon-like import replay peaks at 45 MiB instead of 134, and a
  Plex-like scan at 46 MiB instead of 103; reads and seeks are within 1.5 % of 0.3.0. The fixed
  overhead fell from about 38 to 13 MiB: a WAV file's header no longer keeps its 256 KiB of
  padding in memory, and DST decoders and PCM converters are pooled instead of kept by every
  decoder thread. An idle mount (no reads for 60 s) gives its cache and decoders back, about
  11 MiB idle. The served bytes are unchanged (DSF gate 93/93).

- **scarletbook and scarletbook-pcm 0.5.6 → 0.5.7, now from crates.io** (#155). The two crates
  are taken from the registry, pinned exactly (`=0.5.7`), instead of from a git tag, which also
  lets cargo-deny drop its git exception. Output bytes are unchanged: the DSF gate (93/93), the
  golden PCM and DSF hashes and the real-disc WAV test pass. Straight reads through the mount,
  0.5.6 (main) → 0.5.7 (means of 3, alternating): plain-DSD DSF 1.45 → 1.29 s, plain-DSD WAV
  2.30 → 2.26 s, DST stereo DSF 4.93 → 4.65 s, DST 6-channel DSF 13.33 → 13.13 s; the import
  replay peaks at 44 → 45 MiB at the default 32 MiB cache.

### Fixed

- **`ls -l` over NFS no longer prints "Unknown error 524"** (#174). sacdfs answered
  extended-attribute requests with `ENOSYS` (since 0.1.1, #61), which NFS-Ganesha passed on as
  "not supported" and the Linux NFS client showed as error 524 on every entry of `ls -l` and
  `getfattr -d`. `listxattr` now answers an empty list, one cheap request per entry of an
  `ls -l` (1245 on the corpus, about 4 µs each, against 1 before). `getxattr` still answers
  `ENOSYS`, so the kernel asks once per mount. On 0.4.0-rc.4 it answered `ENODATA`, which
  regressed Roon over SMB: Samba then asked for `security.capability` and the POSIX ACLs on
  every file, 21,767 `GETXATTR` in a Roon force rescan (0 on rc.3), 35,892 requests in all
  against 14,970, and 1,314 in an hour with no scan. Known limit: `getfattr -n <name>` over
  NFS still prints error 524. The container test now runs `ls -lR` over SMB and NFS and
  `getfattr -R -d` over NFS.

- **Removed and renamed images no longer stay in memory** (#153, after #163). The persistent
  seek index kept the disc of every image opened, with its seek index, for as long as the mount
  ran, even after the tree had freed it. A dropped disc is now saved at the next flush (every
  30 s) and then let go of. The mtimes passed up for removed folders (#123) and the watch of
  their tags files (#81) are dropped too. On four real discs, with an album added or removed
  every few seconds under a new name each time, memory over 45 minutes (with #167) grew 123 to
  138 MiB, against 102 to 181 MiB on 0.4.0-rc.2's code. The second-half trend fell from +94 to
  +14 MiB an hour. What remains comes with reads, not with removed albums, and is looked into
  separately.

- **Spare request threads now end** (#167). A pool's spare threads were meant to end after 10 s
  without work, but while jobs kept coming more often than that (any mount being read) none
  ever did. The pool kept as many threads as at its busiest moment (a stall, a burst of
  read-ahead), up to 64. A spare thread now ends after a job once no job has found every
  thread busy for 10 s.

- **Removed and renamed images are freed** (#153). A folder that is re-read now frees what it
  no longer lists, and everything below it: the image's directory, its areas and track files,
  and the disc they hold (its seek state and image handle). Before, every image ever opened
  stayed in memory, so a library whose folders a tagger renames, or whose albums come and go,
  grew for as long as it was mounted (9 nodes and one disc per removed album). Inode numbers do
  not change: they come from paths, so a freed number is never given to anything else.
  A file that is open when its image is removed reads on until it is closed, with the same
  bytes, as long as the cache of open images keeps the image's handle (`--max-open-images`, and
  5 minutes idle); before, its reads failed with `EIO` a second after the removal.
  In a 20-minute soak on the test corpus (an album added or removed every 5 s, every one under
  a new name), the mount's node count stays flat at about 1,400 (was +27 a minute, 1,378 to
  1,915); resident memory moved the same before and after, as corpus discs are small (the
  soak on real discs is still to run). DST stereo reads are unchanged (0.517 ms before and
  after, median of 3,000).

### Documentation

- Short install guides in `docs/install/` (#82), one per NAS system and music server, each
  with numbered steps and a check that it works: TrueNAS SCALE, Docker or Podman, OpenMediaVault,
  Linux clients (NFS, SMB, fstab), Roon, Plex, Jellyfin, Navidrome, Lyrion and MPD. Each says what
  it was validated on; Unraid, Synology, QNAP and UGREEN UGOS are placeholders until someone
  reports a run. `docs/deployment.md` stays the reference and points to them first.

## [0.3.0] - 2026-11-02

Tested as a release candidate (0.3.0-rc.1) on a NAS with Roon (new files and folders found at
once, scanners that skip unchanged folders find deep changes after a restart, multichannel
tags) and with five other media servers on the candidate itself (see `docs/clients.md`).

### Added

- `--pauses kept` (`SACDFS_PAUSES=kept` in the container, #112): serves every track with its
  pause, as extractors that keep pauses write it, as DSF and as WAV, so those exact files can be
  had without re-extracting and `sacdfs verify` reports IDENTICAL, not IDENTICAL-PAUSE, against
  them. Off by default (`--pauses dropped`); the default output is unchanged byte for byte.
- The tag layer (#81): `--tags DIR` (`SACDFS_TAGS` in the container) serves user tags from a
  tags directory, one TOML file per disc named by its TOC identity (`sacdfs tags key IMAGE`),
  so edits survive renames. A file overrides and adds album- and track-level tags (genres,
  MusicBrainz IDs, corrected names, any ID3 text, `TXXX`, `COMM` or `UFID` frame) and embeds a
  picture only when it names one; an album it names gets no ` (Multichannel)` suffix. Only the
  files it changes get new bytes, a new mtime and a new generation; every other file is byte
  for byte as before. `sacdfs tags import` writes tags files from the tags of extracts that
  `sacdfs verify` matches, so they can be deleted without losing them; it writes only the tags
  directory. Adding, editing or removing a tags file shows without a remount, and moves the
  modification time of the disc's folders and every folder above, as a new sidecar does, so
  that scanners that skip unchanged folders (Roon) find it. See the user guide, "Your own tags".

### Changed

- The frame cache keeps its frames in slabs of memory mapped outside the heap, at most
  `--cache-mb` of them, instead of on the heap (#73). Evicted frames no longer leave holes
  that the process keeps: a full-library import replay (271 images) peaks at the cap plus
  what sacdfs holds without a cache (`--cache-mb 16`: 73 → 62 MiB; at the default 256:
  304 → 300 MiB).

- **scarletbook and scarletbook-pcm 0.5.3 → 0.5.6** (#128). Output bytes are unchanged: the DSF
  gate (all 93 tracks), the golden PCM and DSF hashes and the real-disc WAV test pass. Straight
  reads through the mount, against 0.2.0 (means of 3, same machine): plain-DSD stereo WAV
  2.14 s against 2.63 s (-19 %), plain-DSD DSF 1.28 s against 1.65 s (-22 %), DST stereo WAV
  3.80 s against 3.83 s, DST stereo DSF 3.46 s against 3.55 s, DST 6-channel DSF 11.68 s
  against 11.89 s. **This supersedes 0.2.0's known issue** (plain-DSD WAV about 29 % slower
  than in 0.1.1, scarletbook-rs#216): the plain-DSD WAV read is back at 0.1.1's speed.

- Each listed image's table of contents is held once, by scarletbook's disc, instead of twice
  (#136): one TOC's size less per listed image, about 14 KB on small discs and up to 60 KB (see
  docs/performance.md).

### Fixed

- A stalled source (a hung NFS or SMB share, or a slow disk) no longer blocks other requests
  (#63): lookups, listings, opens and the root's attributes run off FUSE's own threads, and
  busy request pools grow up to 64 threads. A watchdog fails any request older than
  `--request-timeout` (default 30 s) with EIO and logs what it waited on, so no read can hang
  for hours (#72).
- The deployment guide now says how to make Plex show a disc's multichannel copy as its own
  album (#138): turn on "Prefer local metadata". Plex's default agent gives both copies the
  matched release's title, so they looked like one album shown twice. No served bytes change.
- A sacdfs that got its stop signal after its mount had already been unmounted from outside
  (`fusermount3 -u`) could unmount the next sacdfs mounted on the same path (#54). The kernel
  reuses a freed device number at once, and sacdfs told its own mount by that number alone;
  it now also checks that its own FUSE connection is still up.

### Documentation

- docs/clients.md (#82): the clients sacdfs has been tested with (Roon, Plex, Jellyfin, Navidrome, Lyrion, MPD, MinimServer, Windows, foobar2000; JRiver untested), the versions and results, and the recommended setup for each.

## [0.2.0] - 2026-11-02

Tested as four release candidates, the last of them (rc.4) on a NAS with Roon: memory
during a full-library import halved (325 MB at the default cache, 60 MB at `--cache-mb 16`,
against 631 and 92 MB in 0.1), folder art in every area folder, multichannel discs as their
own albums, and new files and changed tags found by scanners that skip unchanged folders.
rc.2 to rc.4 fixed how folder modification times follow changes below them (#123) and two
regressions of new-file pickup (#101) that the NAS tests found.

### Added

- `sacdfs verify SOURCE [EXTRACTS]...` (#80): checks extracted DSF and DFF files against
  what sacdfs serves, before they are deleted. It compares the raw DSD audio (ignoring tags,
  padding, bit order and container) with the matching track of the same disc, gives each file
  a status (IDENTICAL, DIFFERENT, NO-MATCH, UNSUPPORTED, ERROR, or NOT-COMPARABLE for FLAC and
  WAV) with the reason, lists the tags a deletion would lose (MusicBrainz IDs, art,
  ReplayGain, others), and writes a TSV report and a summary of the reclaimable space.
  `--map` handles extracts kept in a tree of their own, `--resume` continues an earlier run.
  It never deletes anything. See the user guide, "Before you delete extracts".

- Folder art (the jpg, jpeg, png and webp sidecars) also shows inside each area folder
  (`Stereo/`, `Multichannel/`), as the same file with a second name and inode number, so
  Lyrion Music Server, MPD, MinimServer and Plex find it (#95). It follows `--sidecars`; no
  audio file changes.
- New and removed images and sidecar files show up without a remount (#101): a folder is read
  again, at most every 2 seconds, when its modification time in `SOURCE` changed. Unchanged
  entries keep their inode numbers. The kernel remembers the names of such a folder for 2
  seconds instead of 24 hours.

### Changed

- A library scan's read of a file's head no longer starts read-ahead (#78): reads within the
  first 1 MiB of a file are decoded on demand. Under Roon's import pattern a 6-channel DST
  file costs about a quarter of the CPU (141–177 → 42–44 ms with 12 decoder threads) and
  half the time.

- **Multichannel files get their own album name** (#97). The album tag of the DSF and WAV files of a disc's multichannel area ends in ` (Multichannel)`; stereo files are unchanged. `--no-multichannel-album-suffix` (`SACDFS_MC_ALBUM_SUFFIX=0` in the container) turns it off.
  - *Why:* the owner's decision, after client tests (#82) showed Navidrome, LMS, MPD and MinimServer all treating a disc's Stereo and Multichannel copies as one album. Only the audio has to match an extract's, not the tag bytes, so a tag change is allowed. Alternatives rejected: changing nothing; a separate album-artist or comment tag (servers group by album name); hiding an area (loses content).
  - *What changes:* the tag bytes of multichannel files, so the output versions move: DSF to 3 and WAV to 4 (#89's rule). Their mtimes move forward one version step and NFS generations change. Audio is unchanged (the pinned PCM hashes keep their values; only their recorded WAV version moved), `sacdfs verify` still compares audio only, and the DSF gate compares audio, so it holds.
- `sacdfs verify` recognises pause-kept extracts (#93): a served track with pauses before and/or
  after it, or a track cut short, is `IDENTICAL-PAUSE` when everything extra is silence (the DSD
  idle pattern, or residue at or below -60.2 dB), and otherwise `NO-MATCH` with the length and
  level of the extra audio. Serving pauses is not part of this change.
- `sacdfs verify` looks for images up to two levels below the extract's folder when none is
  beside or above it, trying every candidate (#98).
- `sacdfs verify` no longer lists `TRCK` as lost when the extract's `n/N` matches the served
  track number `n` and `N` is the area's track count (#99).

- **WAV conversion is scarletbook-rs's `scarletbook-pcm` (scarletbook 0.5.3)** (#84). sacdfs's own converter (ported from pysacd's) is deleted, together with its `SACDFS_SIMD` override and the `multiversion` and `rustversion` dependencies.
  - *Why:* the owner's decision was to switch once scarletbook's clean converter was within the 15 % offload leeway and met sacdfs's targets, and 0.5.3 does both. One thread, 176.4 kHz, 40 kHz filter: 298x real time against 315x (generic build), 273x against 316x (x86-64-v3 build). The 30 kHz and wide filters are 13 to 39 % faster. Stopband at least 109 dB from 48, 36 and 88.2 kHz. Ripple ±0.004 dB. DSD silence is exact zero, area edges included.
  - *What changes:* the WAV files' samples. They differ from 0.1's by more than 72 dB below the signal in the audio band, an inaudible ripple difference. So the WAV output version moves to 3 (#89's rule): WAV files' mtimes move forward one more version step (25,203 s) and their NFS generations change, so copies and library scanners pick up the new bytes. This change leaves the DSF files as they are (gate 93/93).
  - Details and measurements: `docs/pcm-quality.md` and `docs/performance.md`.

- Memory stays near `--cache-mb` (#73): one malloc arena, and the frame cache reuses the
  buffers of evicted frames within its cap. A library import's reads over 60 images peak at
  271 MiB with the default 256 MiB cache (was 473 MiB) and 58 MiB at 16 MiB (was 125 MiB);
  straight reads peak at 327 MiB (was about 1 GiB). DST straight reads take about 4 % longer.

- Seeking is scarletbook's (#28): a read that does not continue an open file's cursor starts
  a scarletbook provisional (cold) reading or, after a remount, one from the saved seek hints;
  sacdfs's own frame locator is gone. Cold seeks make fewer calls to the image (3.06 a seek on
  average on the real discs, was 4.44; DST areas 3.0 to 3.4, were 4.8 to 5.5), warm ones one
  call with fewer bytes (84 KiB, was 110). Seek times through the mount are within 15 % of
  0.1's, and output is unchanged (DSF gate 93/93). Areas whose layout scarletbook finds
  suspect are read exactly. Details and figures: `docs/design.md`, "Seeking", and
  `docs/performance.md`.
- **Output change:** DSF files are written by scarletbook (`output::Layout`, its tag from
  `TagSet::from_toc`), and WAV files carry that same ID3 tag in their `id3 ` chunk, their
  `INFO` chunk built from the same items; sacdfs's own DSF header, block layout and ID3 writer
  are gone. The audio is unchanged (DSF gate 93/93, WAV real-disc test). Of the 93 real-disc
  tracks, 53 DSF and 53 WAV files are byte-identical; the other 40 of each differ only in
  `TCOP`, now the year, a space and the copyright, as ID3v2.4 asks (5 bytes longer). Both
  output versions moved to 2, so every DSF and WAV file shows a new mtime and NFS generation.
- The persistent seek index holds scarletbook's seek hints (format version 2): a cache written
  by sacdfs 0.1 is ignored and rewritten, and 0.1 ignores 0.2's.

### Fixed

- A folder's modification time is now the newest of everything shown below it, up to the
  mount's root (#123), not its source folder's own. Roon, which skips folders whose time did
  not change, missed new output versions (and so new tags) and new playlists or art deeper
  down; a plain rescan now finds them. Times stay the same across remounts; folders keep their
  inode numbers and NFS handles. A change while mounted shows within about 2 seconds.

- A new playlist or art file next to an image is listed again within about 2 seconds over SMB
  (#101, #123). In 0.2.0-rc.2 its folder's time moved before its listing was read again, so an
  SMB client kept the old listing and never showed the new file.

- A new playlist or art file is listed in a folder that was not listed since the mount (#101,
  #123). In 0.2.0-rc.3 such a folder's first listing showed it as it was at mount, and the new
  file never appeared.

### Known issues

- Reading plain-DSD WAV files is about 29 % slower than in 0.1.1 (a whole stereo area,
  2.07 s against 2.68 s on a Zen 5 machine). This comes from the PCM conversion, which is now
  scarletbook-pcm's. DST WAV files are about 10 % slower and DSF files are faster. It is still
  about 1,000x real time per stream. The fix is being worked on upstream
  (scarletbook-rs#216), and sacdfs will pick it up in a later release.

### Documentation

- CI: a "no private names" check of commit identities and tracked files (`scripts/check-private-names.sh`, adapted from pysacd's). The Roon example in `docs/deployment.md` uses an arbitrary container subnet.

## [0.1.1] - 2026-11-01

Folder art, scans and playlists for players such as Roon, and far fewer requests per file. These come from a real-world Roon test on a NAS. scarletbook 0.5.2. The audio files are byte-identical to 0.1.0's.

### Added

- Sidecar files are passed through (#79, #75): folder art (`.jpg`, `.jpeg`, `.png`,
  `.webp`), `Artwork/` and `Scans/` folders with all their content, and `.m3u`/`.m3u8`
  playlists next to the images show at their relative path, read-only and byte for byte,
  with their source file's size and times. Players such as Roon find the folder art, and
  playlists can live on the share, so extracted copies can go without losing either. With
  `--flatten`, a collapsed chain's sidecars show in the collapsed folder. They have stable
  inode numbers (the same with and without `--flatten`) and NFS handles that survive a
  restart and go stale when the file changes. `--sidecars LIST` (`SACDFS_SIDECARS` in the
  container) widens, narrows or turns off the list (`default,cue`, `default,-webp`, `none`).
  Hidden files are never passed, and a link is followed only to a file inside SOURCE and
  outside the mount point. The audio files are unchanged.

### Changed

- scarletbook 0.5.2 (was 0.5.0), the engine for 0.2.0: sacdfs's output is unchanged (DSF
  files bit-identical to the reference extractor on all 93 tracks of the real discs, the same
  WAV bytes). One test changed: on the hostile corpus image `area-audio-past-end`,
  scarletbook's provisional (cold) readings now all equal its exact ones, where 0.5.0's
  differed, and no corpus image gives a differing one on 0.5.2. A new test makes one (a copy
  of a corpus image with late audio sectors written again near the area's start) and checks
  that every provisional reading that differs is refuted.

- Cheaper probes (#61): `getxattr` and `listxattr` answer `ENOSYS`, so the kernel stops
  asking (a second `ls -l` of a 48-file area sent 49 `LISTXATTR` requests, now none), and a
  missing name gets a negative entry cached for 24 hours (repeated `stat`s of `cover.jpg`,
  `desktop.ini` and the like reached sacdfs every time, now once). Extended attribute calls
  now fail with "Operation not supported" instead of "No data available".
- A track file's modification time moves when its bytes do (#64): WAV files with a
  non-default `--pcm-cutoff` or `--pcm-gain` show their image's time plus a fixed offset per
  option set (up to 7 hours), and a future output-version bump moves the times of that
  format forward again, so that rsync and library scanners see the change. With the default
  options, the times stay the images' own; NFS generations are unchanged.

### Fixed

- `df` and SMB clients saw a 0-byte filesystem (#62): `statfs` now reports the size of the
  filesystem holding SOURCE, with nothing free.

### Documentation

- `docs/deployment.md`, Roon: discs also present as extracted files are merged by Roon,
  which may play either copy. Folder art and playlists now work (#79, #75).
- Server-side figures from the owner's NAS (#21 part 2), in `docs/performance.md`, "On a NAS,
  server-side". The published 0.1.0 image ran as a TrueNAS SCALE 25.10 app, serving over NFS
  and SMB on the LAN:
  - **Cold seeks:** 15 to 22 ms from the NAS's cache, and 31 to 61 ms (worst 92 ms) from its
    hard disks, against a target of 25 ms.
  - **Straight reads:** stereo at the client's 2.5 Gbit/s link, and 6-channel DST at 65 to
    83 MB/s.
  - **4 players and a tagger over NFS:** worst reads 52 to 133 ms, all of them first touches
    of the disks.
  - **A stall:** one SMB read waited about 6 hours, not reproduced since (#72).
- `docs/deployment.md`, TrueNAS steps:
  - **Tested:** they were tested on a real system and worked as written.
  - **Ports:** when the NAS's own SMB and NFS services hold 445 and 2049, publish other ports
    (the simplest) or give the app an alias address. Client mounts on other ports are added.
  - **Logs:** NFS-Ganesha's harmless D-Bus and UDP lines are explained.
  - **libsmbclient:** a note on how clients built on libsmbclient show mapped names.
  - **Roon on the same NAS:** tested. sacdfs gets a fixed address on its own Docker network,
    so Roon reaches its SMB share on port 445 without clashing with the NAS's own SMB.
    Roon's container needs `SYS_ADMIN`, `DAC_READ_SEARCH` and AppArmor unconfined to mount
    the share. DSF only, so tracks don't appear twice.

## [0.1.0] - 2026-11-01

The first release.

### Added

- `sacdfs SOURCE MOUNTPOINT`: a read-only FUSE filesystem showing SACD images (one file, or a
  directory searched recursively for `*.iso`) as DSF files with ID3v2.4 tags, and WAV files
  next to them, decoded on demand by scarletbook 0.5.0. Options `-f`, `--threads`,
  `--cache-mb`, `--allow-other`, `-v`; unmounts on SIGINT, SIGTERM and SIGHUP, and detaches a busy
  mount (open files, an NFS export), stopping when the last open file is closed.
- WAV files (#7, #8, plan #11): each track also appears as `NN - title.wav`, 24-bit PCM
  converted on demand. RIFF/WAVE with `WAVE_FORMAT_EXTENSIBLE`, RF64 for files over 4 GiB; the
  DSF file's tag in an `id3 ` chunk and a `LIST`/`INFO` chunk, after the audio, which is padded
  to start at 256 KiB and end on a page boundary, so that listing, `stat`, and reading the
  header or the tags never convert. Gapless: a track's edge frames are converted with the
  neighbouring audio of the area. Options `--format dsf|wav|dsf,wav` (default both),
  `--pcm-rate 88200|176400|352800` (default 176400), `--pcm-cutoff 40|30|wide` (default 40;
  `wide`, about 80 kHz, only at 176.4 and 352.8 kHz), `--pcm-gain DB` (default 0, -60 to +24;
  clipped samples are clamped and logged once per file).
- The DSD-to-PCM converter, `sacdfs::pcm` (#5, #16): stateless per frame (frame f from frames
  f-1, f, f+1, equal bit for bit to a whole-area conversion), aligned to the DSD sample, the
  same bytes on every CPU. Pass band flat to ±0.00003 dB, stopband at least 114.9 dB. Ported
  from pysacd's converter; its first stage uses the byte-lookup-table technique of Sebastian
  Gesemann's dsd2pcm (BSD-3-Clause). Its code is chosen at run time (#23): AVX2 where the CPU
  has it, SSE2 otherwise (`SACDFS_SIMD=baseline|v3|v4` forces one), so a generic build
  converts 176.4 kHz stereo at about 300 times real time per core. Plain DSD frames are
  converted four at a time (`Converter::convert_run`, #24).
- Speaker layouts (#31): the WAV channel mask and the DSF channel type come from the area's
  loudspeaker layout on the disc: stereo, 5-channel 3/2 ("5.0") and 5.1. An area without a
  defined layout gets the layout of its channel count, logged once per disc.
- Frame locator: a learned index and an interpolating search aimed with the TOC's track
  starts, sized reads, per-open-file sequential continuation, read-ahead, a bounded cache of
  decoded and converted frames (`--cache-mb`) and parallel DST decoding. A cold seek costs 3
  to 5.6 reads of the image (#4, #20).
- Persistent seek index (#20): the frame positions sacdfs learns are kept in
  `$XDG_CACHE_HOME/sacdfs` (`--index-cache DIR`, `--no-index-cache`), so that a seek after a
  remount needs about one read of the image instead of four to five. Named by a hash of the
  image's path, keyed by its size and mtime, checksummed, 32 MiB in all; every saved position
  is checked when used, so a stale or damaged cache only costs speed.
- Open image files are kept in a bounded cache (#26): `--max-open-images N` (default 64), least
  recently used closed first, closed after 5 minutes unused, opened again if the file changed.
- Concurrent reads of one image (#28): positional reads, one image clone per read, so players,
  taggers and read-ahead on one disc don't wait for each other's round trips on a network
  share.
- `--flatten` (#13): a chain of folders below the top level that holds a single image
  collapses into its outermost folder. Decided from the names walked at mount, so mounting is
  as fast as without it.
- Persistent file identity and NFS export (#18): inode numbers come from what an entry is, the
  same on every mount and with `--flatten` on or off; generation numbers change with the
  image and the PCM options, so stale handles fail with `ESTALE`. The kernel NFS server can
  export the mount, and its clients read on through a restart of sacdfs.
- A container image for a NAS (#19, `Containerfile`): sacdfs with Samba and NFS-Ganesha,
  sharing the mount read-only over SMB and NFSv4, configured with environment variables, with
  a health check, logs on standard output, and NFS and SMB clients reading on across a
  restart. Built for generic x86-64. Compose file in `deploy/`, guide in
  `docs/deployment.md` (Docker, Podman, Compose, TrueNAS SCALE, client mounts).
- Release workflow (#44): on a version tag, Linux x86_64 binaries (generic and x86-64-v3,
  glibc 2.28 or later, stripped, with COPYING, README and CHANGELOG), SHA-256 checksums,
  build provenance attestations, release notes from this file, and the container image on
  `ghcr.io/aclater/sacdfs` (version and `latest`) after the share test passed on it. Run by
  hand, it is a dry run that publishes nothing. Not on crates.io while scarletbook is a git
  dependency.
- Diagnostics and measurement: `Disc::sector_reads`, `Disc::sector_bytes`, the total reads of
  the images logged at `-v` when sacdfs stops, `examples/seek_latency`,
  `examples/library_seek`, `examples/pcm_track.rs`, and a criterion benchmark of the converter.
- Tests against the CC0 conformance corpus, with and without mounting; proptest of the offset
  mapping, the WAV mapping and the seek index format; scarletbook's exact seek index as a
  reference for every frame the locator finds; concurrent reads; NFS export and restarts in
  CI; the container's SMB and NFS shares in CI.
- CI: fmt, clippy, tests with FUSE, MSRV, cargo-deny, CodeQL, zizmor, dependency review,
  OpenSSF Scorecard, coverage, a check that the README's options table matches
  `sacdfs --help`, and a link check of the Markdown files.

### Documentation

- The oldest distributions the README names were tested (#58): RHEL 8.10, Debian 10,
  Ubuntu 20.04 and Fedora 29, each on its own kernel (4.18 to 5.4) with its own
  `fusermount3` (3.3.0 to 3.9.0), using the generic release build. Mounting with `-f` and
  in the background, with and without `--allow-other`, reads identical to a current
  system's, SIGTERM with a file open and `fusermount3 -u` all pass; nothing needed fixing
  and the README's claim stands. Results in `docs/design.md`, "Oldest distributions".
- README as a user guide (install, quick start, options, layout, formats, deployment,
  limits, credits); `docs/user-guide.md` (options in detail, formats, tagging behaviour,
  sharing over NFS, troubleshooting); `docs/design.md` with the current design and the reason
  for each decision; `docs/performance.md` by deployment against the targets of #6, with a
  history of each improvement (#37).

### Fixed

- A sacdfs restarted on the same mount point while the previous one was still exiting lost
  its mount (#51). fuser 0.18.0 unmounts by path when a session ends, and its check that the
  mount still exists always passes, so a sacdfs unmounted with `fusermount3 -u` unmounted the
  path again on its way out, removing the new mount (every time the old one exited after the
  new one had mounted). sacdfs now serves the session without fuser's unmount, and unmounts
  or detaches on a signal only while the mount at its path is its own (by device number).
  A sacdfs started while the old mount is still there waits for it to go (up to 10 s) instead
  of mounting over it, where the old one's unmount removed the new mount (as root) or the
  restart failed (as a user). Regression tests in `tests/mount.rs`.
- A SOURCE path holding a comma could not be mounted (#55): fuser 0.18 passes the
  filesystem name to `fusermount3` unescaped, so `Bach, J.S.` failed with "unknown option",
  and a name such as `x,allow_other` would have added the option. The mount is now always
  named `sacdfs` (in `df`, `mount`, `/proc/mounts`); `-v` logs the source.
- Mounting at or above SOURCE (`sacdfs X X`, `sacdfs X/sub X`) hung the mount on its first
  access, beyond SIGKILL (#56): sacdfs read its images through its own mount. It now refuses
  to start when SOURCE is or lies inside MOUNTPOINT, when MOUNTPOINT lies inside SOURCE and
  holds images, and when `--index-cache` lies inside MOUNTPOINT (the default cache directory
  there only turns the cache off, with a warning). A non-empty MOUNTPOINT is allowed, as in
  FUSE 3, with a warning.
- After sacdfs was killed or crashed, a restart on the same mount point waited 10 s and then
  failed with "already a FUSE mount (another sacdfs?)" (#57). A dead sacdfs mount of the same
  user (ENOTCONN, found with a bounded `statfs`) is now detached at once and the restart
  mounts in milliseconds; another dead mount, or another user's FUSE mount, fails at once
  with an accurate message.
- A SIGINT, SIGTERM or SIGHUP right after start could kill sacdfs with its mount in place,
  leaving a dead mount (#57; 7 of 100 starts with a SIGTERM 0 to 30 ms after the start): the
  signal handlers were set up after mounting. They are now set up before. SIGHUP's behaviour
  (unmount and stop, like SIGTERM) is documented.

[Unreleased]: https://github.com/aclater/sacdfs/compare/v0.4.1...HEAD
[0.4.1]: https://github.com/aclater/sacdfs/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/aclater/sacdfs/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/aclater/sacdfs/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/aclater/sacdfs/compare/v0.1.1...v0.2.0
[0.1.1]: https://github.com/aclater/sacdfs/compare/v0.1.0...v0.1.1
[0.1.0]: https://github.com/aclater/sacdfs/releases/tag/v0.1.0
