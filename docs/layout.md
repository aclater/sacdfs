# What the mount shows

The folders and files sacdfs shows, and what is in them. The [user guide](user-guide.md)
has the details.

## Layout

```
MOUNTPOINT/
  <directory of the image, relative to SOURCE>/<image file name without .iso>/
    Stereo/
      01 - <title>.dsf
      01 - <title>.wav
      02 - <title>.dsf
      02 - <title>.wav
    Multichannel/
      01 - <title>.dsf
      01 - <title>.wav
```

- Each track is there twice, as a DSF and a WAV file of the same name; `--format` shows one
  kind only. `Stereo` and `Multichannel` appear only for the areas a disc has.
- File names come from the track titles (`NN.dsf` for a track without one); repeated names
  get ` (2)`, ` (3)`.
- Files that are not SACD images (a DVD-Video or Blu-ray `.iso`, say), folders with no SACD image
  below them (their sidecars too), and tracks the disc cannot play are left out. A box set that
  mixes an SACD with DVD discs shows only the SACD.
- **Sidecar files** next to the images, `folder.jpg`, `cover.png`, an `Artwork/` or `Scans/`
  folder, `.m3u`/`.m3u8` playlists, are shown where they are in `SOURCE`, read-only and byte
  for byte, with their own size and times: players and file managers find the folder art,
  and you can delete extracted copies without losing it. The images among them (folder art)
  also show inside `Stereo/` and `Multichannel/`, as the same file, for servers that look
  only beside the audio. `--sidecars` sets which
  ([user guide](user-guide.md#sidecar-files-art-scans-playlists)). Hidden files are never
  passed, nor a link that points outside `SOURCE`.

With `--flatten`, a folder whose only content is one image, directly or through a chain of
folders that each hold nothing else, holds that image's `Stereo` and `Multichannel` folders
itself. Folders directly in `SOURCE` (usually artists) are always kept. For example

```
SOURCE/Artist/2001 - Album [SACD]/Artist - Album (rip)/Album/disc.iso
```

shows as `MOUNTPOINT/Artist/2001 - Album [SACD]/Stereo/01 - Title.dsf` instead of
`MOUNTPOINT/Artist/2001 - Album [SACD]/Artist - Album (rip)/Album/disc/Stereo/01 - Title.dsf`.
It is decided from the file names alone, so it does not slow down mounting. The sidecar files
of the collapsed folders all show in the one that is kept, next to `Stereo`.

## Formats and quality

The **DSF files** (DSD64, 2.8224 MHz, 1 bit) hold each track's audio bit-identical to what
the reference extractors (pysacd's `pysacd-extract`, scarletbook's `bench decode`) write, with
an ID3v2.4 tag from the disc's text. The **WAV files** are the same tracks converted to 24-bit
PCM at 176.4 kHz (or 88.2 or 352.8 kHz), band-limited to 40 kHz by default (30 kHz, or about
80 kHz with `--pcm-cutoff wide`), gapless from one track to the next, with the same tag in an
`id3 ` chunk and a `LIST`/`INFO` chunk; channel masks follow the disc's speaker layout
(stereo, 5.0, 5.1), and files over 4 GiB are RF64. The conversion is scarletbook-rs's
`scarletbook-pcm`: its pass band is flat to ±0.004 dB, its stopband at least 109 dB down, and
DSD silence comes out as digital silence ([docs/pcm-quality.md](pcm-quality.md)). Sizes are exact; listing, `stat`
and reading a WAV file's header or tags never decode anything. Details, and how taggers and
scanners are served: [user guide](user-guide.md#formats).
