# Conformance corpus

This directory holds synthetic SACD disc images and, in `manifest.json`, what a conforming
reader must produce from each: the TOC, the warnings, and a hash of every track's decoded DSD
per channel, or the error that rejects the image.

The images are synthetic. They contain generated test signals, never audio from a real disc.
The corpus is dedicated to the public domain under CC0 1.0 (see [LICENSE](LICENSE)).

## Contents

| Path | Content |
|---|---|
| `manifest.json` | one entry per case (schema below) |
| `images/<id>.iso` | the input image of case `<id>`, uncompressed |

There are two categories:

- **valid** (27 cases): discs a reader accepts in full, some with deliberate oddities (text
  that decodes to U+FFFD, a TOC-2 fallback with its warning), covering the three sector formats, stereo and
  multichannel areas, DST and plain DSD, every optional DST syntax feature at 2, 5 and 6
  channels, every character set code (0 to 7), the positions where published CJK mappings
  disagree, multi-disc set fields, a disc with no text, a disc with every text field, an area
  without ISRCs and genres, frame format 3, and a TOC-2 fallback.
- **malformed** (102 cases): mostly one broken thing each (a few cases combine several, as their
  purpose says), mostly starting from the same small base disc (case `hostile-base`): overlapping, inverted and out-of-image ranges, absurd counts
  and durations, truncated areas and images, garbage, truncated, oversized and invalid DST
  frames, sector-layout confusion, and invalid text encodings.

Each case's `purpose` says what it exercises and why its outcome is what it is.

## Manifest schema

The top level is an object:

| Key | Type | Meaning |
|---|---|---|
| `format` | string | `sacd-spec conformance manifest` |
| `format_version` | integer | 1 |
| `description` | string | one-line description of the file |
| `spec_version` | string | version of the specifications the expectations follow |
| `license` | string | `CC0-1.0` |
| `cases` | array | one object per case |

### Case object

| Key | Type | Meaning |
|---|---|---|
| `id` | string | unique name; the image is `images/<id>.iso` |
| `category` | string | `valid` or `malformed` |
| `origin` | string | `pysacd hostile-image corpus` for cases taken from that corpus (their ids are the names that corpus uses, kept for traceability), `new` for cases added for this repository |
| `purpose` | string | what the case exercises |
| `input` | string | path of the image, relative to the repository root |
| `input_sha256` | string | SHA-256 of the image file, lower-case hex |
| `input_bytes` | integer | size of the image file in bytes |
| `frames` | array, optional | for some DST cases, what each frame of an area contains (below); its absence means nothing |
| `expected` | object | the expected result (below) |

`frames` entries are informative: `area` (slot), `frame` (frame number), `content` (prose),
and for the invalid-frame cases `dst_valid` (whether the frame is valid under
[specification 5](../spec/05-dst.md)). The rule names in `content` are those of
specification 5, section 5.6.

### Expected result

| Key | Type | Meaning |
|---|---|---|
| `outcome` | string | `ok`, or `reject` if the image is rejected |
| `error` | string or null | for `reject`, the error code ([specification 7](../spec/07-validation-and-limits.md), 7.7.1) |
| `warnings` | array | the expected warnings (below); empty for `reject` |
| `toc` | object or null | the expected TOC (below); null for `reject` |
| `audio` | array | per area, the expected track audio (below); empty for `reject` |

A warning is an object with `code` (specification 7, 7.7.2 or 7.7.3), `area` (the slot, `2ch`
or `multichannel`, when the code concerns an area) and `track` (the track number, when it
concerns a track). A key that does not apply is left out; for comparison, a left-out `area` or
`track` means "none" and equals a reader's null or absent value.

### TOC object

Disc level (specification 2):

| Key | Spec | Meaning |
|---|---|---|
| `sector_size` | 1.2 | stored sector size `P`: 2048, 2054 or 2064 (2048 for a disc read over the network) |
| `sectors` | 1.3 | `N`, the number of sectors in the image (the reported disc size over the network) |
| `version` | 2.3 | [major, minor] |
| `album_set_size`, `album_sequence_number` | 2.3.1 | as stored |
| `album_catalog_number`, `disc_catalog_number` | 2.6.5 | fixed text fields (may be empty) |
| `album_genres`, `disc_genres` | 2.4 | 4 objects each, `{category, genre}`: `category` is the genre table byte and `genre` the genre index byte |
| `hybrid` | 2.3 | boolean, bit 7 of offset 80 |
| `date` | 2.3.2 | `{year, month, day}` as stored |
| `locales` | 2.5.2 | reported list of `{language, charset}`; `charset` is the stored byte |
| `text` | 2.5.4 | object mapping item names to strings; absent items are left out; always present, `{}` when there is no text |
| `manufacturer_sector` | 2.7 | boolean, whether `SACD_Man` is present |
| `areas` | 3.1 | the areas in slot order |

Disc text item names: `album_title`, `album_artist`, `album_publisher`, `album_copyright`,
`album_title_phonetic`, `album_artist_phonetic`, `album_publisher_phonetic`,
`album_copyright_phonetic`, and the same eight with `disc_` instead of `album_`.

Area object (specification 3):

| Key | Spec | Meaning |
|---|---|---|
| `slot` | 3.1 | `2ch` or `multichannel`: which master TOC slot the area came from |
| `signature` | 3.3 | `TWOCHTOC` or `MULCHTOC` |
| `version` | 3.3 | [major, minor] |
| `toc_lsn` | 3.2 | LSN of the copy used (TOC-1 or TOC-2) |
| `toc_sectors` | 3.2 | area TOC size from the master TOC |
| `max_byte_rate` | 3.3 | as stored |
| `sample_frequency_code` | 3.4.2 | as stored |
| `frame_format` | 3.4.2 | low 4 bits of offset 21 |
| `channel_count` | 3.4.1 | as stored |
| `area_config` | 3.4.1 | the whole byte at offset 33 |
| `max_available_channels`, `area_mute_flags` | 3.3 | as stored |
| `track_attribute` | 3.3 | low 4 bits of offset 48 |
| `playtime_frames` | 3.3 | frame number of the total play time |
| `track_offset` | 3.3 | as stored |
| `audio_start_lsn`, `audio_end_lsn` | 3.3 | as stored |
| `locales` | 3.5 | reported list of `{language, charset}` |
| `text` | 3.6 | object with `description`, `copyright`, `description_phonetic`, `copyright_phonetic` when present; always present, `{}` when there is none |
| `tracks` | 3.9 | array of track objects |

Track object:

| Key | Spec | Meaning |
|---|---|---|
| `number` | 3.9.1 | 1-based |
| `start_lsn`, `length_sectors` | 3.7.1 | start LSN and length in sectors, as stored |
| `start_frame`, `duration_frames` | 3.7.2, 3.9.2 | frame numbers **after** clamping |
| `start_flags`, `duration_flags` | 3.7.2 | the fourth byte of each timecode entry |
| `isrc` | 3.7.3 | string, or null for no ISRC |
| `genre` | 3.7.3 | `{category, genre}`, or null when the area has no `SACD_IGL` |
| `text` | 3.8 | object mapping item names to strings: `title`, `performer`, `songwriter`, `composer`, `arranger`, `message`, `extra_message`, and each with `_phonetic` appended; always present, `{}` when there is none |

### Audio array

One object per area, in the order of `toc.areas`:

| Key | Meaning |
|---|---|
| `area` | the slot |
| `tracks` | one object per track of the area, in track order |

Track audio object ([specification 4](../spec/04-audio-multiplexing.md)):

| Key | Meaning |
|---|---|
| `number` | track number |
| `first_frame`, `end_frame` | the frame range `[first_frame, end_frame)` of 4.7 |
| `outcome` | `ok`, `skip` (track skipped) or `error` (track refused) |
| `error` | for `skip` and `error`, the code of 4.8 / 7.7.4; left out for `ok` |
| `missing_frames` | for `ok`, how many frames of the range were missing (filled with 0x69) |
| `invalid_dst_frames` | for `ok`, how many frames of the range were invalid DST frames (filled with 0x55) |
| `sha256` | for `ok`, one SHA-256 (lower-case hex) per channel, in channel order, of that channel's track audio (4.7 item 2): `(end_frame - first_frame) x 4704` bytes, DSD bytes with the earliest sample in the MSB |

## How to check a reader

For each case:

1. Verify the image's SHA-256 against `input_sha256`.
2. Read the image. If `expected.outcome` is `reject`, the reader must reject it with
   `expected.error` and must not produce a TOC or audio. Nothing else is compared for a rejected
   image: `warnings` is empty in the manifest, and warnings a reader emits before rejecting are
   not checked (specification 0, 0.8).
3. Otherwise the reader must not reject the image. Compare the reader's TOC with
   `expected.toc`: every key listed above must have an equal value; text objects must have the
   same set of items with equal strings (code point for code point). A reader's own TOC
   representation may hold more information; only the keys listed above are compared.
4. Compare warnings: the set of (`code`, `area`, `track`) triples the reader reports from
   specification 7, 7.7.2 and 7.7.3, excluding the optional codes `audio-sector-malformed`,
   `master-toc-copies-differ` and `sample-frequency-unknown`, must equal the set in
   `expected.warnings`. Repeated warnings count once.
5. The reader's areas must be those of `expected.audio` (by slot) and each area's tracks those
   listed (by number); an extra or missing area or track is a failure. For each track in
   `expected.audio` (matched by `number`, with `first_frame` and `end_frame`
   compared too): if the outcome is `skip` (track skipped) or `error` (track refused), the reader
   must produce no audio and report that code; if `ok`, the reader's audio for each channel must
   hash to the given value. The frame counts are a diagnostic aid: a reader that reports them
   must report these values.

## How the corpus was made

The images were built with the synthetic-image generator and the test DST encoder of pysacd
(a GPL-licensed SACD toolkit), which are not part of this repository. The expected values were
computed with pysacd's TOC reader and decoding engine, configured to follow these
specifications where its command-line tools differ (whole-area scan clipped to the image,
per-track capacity check, zero-length tracks skipped). One expected value was set by hand from
the specification because pysacd does not apply the rule: track 2 of `dst-code-all-ones` (DST
rule 17). The DST feature cases were also checked
against the encoder's input signal: their decoded DSD equals the signal that was encoded.
[PROVENANCE.md](../PROVENANCE.md) records the details.

Implementations must not depend on how the corpus was made: the specifications define every
expected value.

## Known coverage limits

A reader that gets these rules wrong still passes every case; the specifications are the
authority for them:

- the 4096-byte cut of text strings, the whitespace set of fixed text fields, and trailing-space
  removal in language codes;
- the `SACD_IGL` extent check (`area-toc-malformed` is never expected), "last `SACD_IGL`
  wins" and "first `SACDTTxt` wins", and stopping the sub-structure walk at an unknown
  signature;
- area text offsets between 2048 and the end of the area TOC; disc text decoded with locale 0's
  character set when the text channel count is 0;
- the exact 65 536-byte boundary of a frame under assembly, stopping a sector at an overrunning
  packet, and discarding a frame that starts without a frame record;
- the warnings `manufacturer-missing` (never expected) and `area-toc-too-large` (only in
  rejected images, where warnings are not compared);
- the exact acceptance rule of the timeline (a repeated timecode is discarded), zero-length
  audio packets counting towards a DST frame's sector count, the "at least 1 byte" condition
  of frame completeness, and the exact threshold of 7 unread DST code bits;
- the cap of 10 area locales, and that an unknown track text type with an empty string is not
  reported;
- DST rule 1 for a 1-byte frame whose `DST_coded` bit is 0 (every 1-byte frame is invalid,
  but all 1-byte frames in the corpus have the bit set);
- a raw 2054-byte image whose sectors are misaligned (`layout-2054-shifted` is, despite its
  name, aligned); a ninth ptable segment; EUC-KR composition sequences.

## Size

About 177 MB (169 MiB) uncompressed, 129 files of 1.0 to 2.0 MB. Most of each image is the 510 empty
sectors before the master TOC, which compress well, so the repository stays far smaller.
