// SPDX-License-Identifier: GPL-3.0-or-later

//! DSF files, written by scarletbook (`output::Layout`, #28): the header chunks, the audio
//! in 4096-byte blocks per channel, and an ID3v2.4 tag from the TOC (`TagSet::from_toc`), with
//! sacdfs's choices of the writer's options. The same tag is the WAV files' `id3 ` chunk.

use scarletbook::output::{
    AlbumArtistFrame, DateFrame, Format, Id3Options, Layout as LibLayout, OutputOptions, PositionSource,
    RegionKind, SourceTrack, TagItem, TagSet, Tags, TextSource,
};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::tags::Stamp;

use scarletbook::{Area, Slot, Toc, Track};

/// DSD bytes per channel in one SACD frame (1/75 s at 2.8224 MHz).
pub const FRAME_BYTES: u64 = 4704;

/// Bytes per channel in one DSF block.
pub const BLOCK: u64 = 4096;

/// Bytes before the first audio byte: the DSD chunk, the fmt chunk and the data chunk header.
pub const HEADER_LEN: u64 = 28 + 52 + 12;

/// The sampling frequency of SACD audio, in Hz.
pub const SAMPLE_RATE: u32 = 2_822_400;

/// The audio of one track file: `frames` SACD frames of `channels` channels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Layout {
    /// Channel count, 1 to 6.
    pub channels: u8,
    /// Number of frames (timeline positions) in the file.
    pub frames: u32,
}

impl Layout {
    /// DSD samples per channel.
    pub fn samples(&self) -> u64 {
        u64::from(self.frames) * FRAME_BYTES * 8
    }
}

/// The ID3 frames in the order sacdfs writes them.
pub const FRAME_ORDER: [&[u8; 4]; 12] = [
    b"TIT2", b"TPE1", b"TPE2", b"TALB", b"TRCK", b"TPOS", b"TCOM", b"TDRC", b"TSRC", b"TCON", b"TPUB",
    b"TCOP",
];

/// The writer's options sacdfs uses for the track `track` of `area`.
///
/// - ID3: no footer and no padding (the tag ends the file), the frames in
///   [`FRAME_ORDER`] (those not in it after them, in the library's order), the album artist
///   as `TPE2` and the date as `TDRC`, as sacdfs 0.1 wrote them. *Why these:* with them, 53 of
///   the 93 real-disc files kept their bytes; the other 40 differ only in `TCOP`, which the
///   library writes as ID3v2.4 asks (the date's year, a space, the copyright).
/// - The channel type of an area without a defined loudspeaker layout: that of its channel
///   count ([`count_channel_type`]); the library leaves the choice to the writer.
pub fn options(area: &Area, track: &Track) -> OutputOptions {
    OutputOptions::new()
        .with_source_track(SourceTrack::Track(track.number))
        .with_dsf_unspecified_channel_type(Some(count_channel_type(area.channel_count)))
        .with_id3(
            Id3Options::new()
                .with_footer(false)
                .with_padding(0)
                .with_frame_order(FRAME_ORDER.iter().map(|f| **f).collect())
                .with_album_artist(AlbumArtistFrame::Tpe2)
                .with_date(DateFrame::Tdrc),
        )
}

/// Whether multichannel files get [`MC_ALBUM_SUFFIX`] on their album tag (#97). On by default;
/// `--no-multichannel-album-suffix` turns it off. *Why a process-wide switch:* it is set once
/// at startup, before any file exists, and it keeps the option out of the tree's and the
/// handles' signatures, which other work is changing.
static MC_ALBUM_SUFFIX_ON: AtomicBool = AtomicBool::new(true);

/// Set whether multichannel files get the album suffix (once, at startup).
pub fn set_mc_album_suffix(on: bool) {
    MC_ALBUM_SUFFIX_ON.store(on, Ordering::Relaxed);
}

/// The suffix of the album tag of multichannel files (#97).
pub const MC_ALBUM_SUFFIX: &str = " (Multichannel)";

/// The album tag of `set` for an area in slot `slot`, with the multichannel suffix if `on`.
/// A stereo area, or a set without an album, is unchanged.
///
/// *Why:* in client tests, Navidrome, LMS, MPD and MinimServer all showed a disc's Stereo and
/// Multichannel copies as one album (merged, tracks up to 4 times, or two albums with the same
/// name). Only the audio must match an extract's, not the tag (#82, #97).
///
/// The tag layer (#81): an album name a tags file supplies wins and gets no suffix
/// ([`tagged`]); a tags file that wants one writes it in `[multichannel.album]`.
fn apply_mc_album_suffix(set: &mut TagSet, slot: Slot, on: bool) {
    if !on || slot != Slot::Multichannel {
        return;
    }
    let Some(album) = set.get(TagItem::Album).first().filter(|a| !a.trim().is_empty()).cloned() else {
        return;
    };
    set.set(TagItem::Album, &format!("{album}{MC_ALBUM_SUFFIX}"));
}

/// The tag items of a track: the library's from the TOC, with two choices of sacdfs's:
/// - the album title, artist, publisher and copyright come from the album text, else the disc
///   text (the library takes one source, or the disc's first). *Why:* a set's discs share the
///   album title, so players group them as one album; a disc title ("Disc 2") would split it.
/// - the album gets [`MC_ALBUM_SUFFIX`] in a multichannel area (#97), unless turned off.
/// - no album set for a one-disc set. *Why:* "1/1" says nothing, and sacdfs 0.1 left it out.
pub fn tag_set(toc: &Toc, area: &Area, track: &Track, options: &OutputOptions) -> TagSet {
    let mut set = base_tag_set(toc, area, track, options);
    apply_mc_album_suffix(&mut set, area.slot, MC_ALBUM_SUFFIX_ON.load(Ordering::Relaxed));
    set
}

/// [`tag_set`] without the multichannel suffix.
fn base_tag_set(toc: &Toc, area: &Area, track: &Track, options: &OutputOptions) -> TagSet {
    let album = options.clone().with_text_source(TextSource::Album);
    let mut set = TagSet::from_toc(toc, area, Some(track), &album);
    let disc = TagSet::from_toc(toc, area, Some(track), &options.clone().with_text_source(TextSource::Disc));
    for item in [TagItem::Album, TagItem::AlbumArtist, TagItem::Publisher, TagItem::Copyright] {
        if set.get(item).is_empty() {
            for v in disc.get(item) {
                set.push(item, v);
            }
        }
    }
    if set.get(TagItem::DiscCount).first().is_some_and(|c| c == "1") {
        set.remove(TagItem::DiscNumber);
        set.remove(TagItem::DiscCount);
    }
    set
}

/// A track's DSF layout with its tag items, and what the tag layer (#81) changed.
#[derive(Debug)]
pub struct Tagged {
    /// The DSF layout, with the tag.
    pub layout: LibLayout,
    /// The tag items (for a WAV file's INFO chunk).
    pub set: TagSet,
    /// Set when a tags file changed the tag: then the file's mtime and generation change.
    pub stamp: Option<Stamp>,
}

/// The DSF layout of `track` of `area`, with its tag: [`tag_set`]'s items, changed by the
/// disc's tags file if there is one ([`crate::tags`]). `None` if the library refuses the
/// track (then no file is shown for it).
///
/// A tags file's album name gets no multichannel suffix (#97): the user named the album.
/// A file whose tag comes out the same as without the tags file has no stamp: its bytes,
/// mtime and generation are those without the tag layer.
pub fn tagged(toc: &Toc, area: &Area, track: &Track) -> Option<Tagged> {
    let options = options(area, track);
    let default = tag_set(toc, area, track, &options);
    let make = |tags: Tags| {
        LibLayout::new(
            toc,
            area.slot,
            track.start_frame,
            track.end_frame(),
            Format::Dsf,
            &options.clone().with_tags(tags),
        )
    };
    let of_set = |s: &TagSet| if s.is_empty() { Tags::None } else { Tags::Set(s.clone()) };
    let user = crate::tags::lookup(toc).and_then(|d| Some((d.mtime, d.overrides(area.slot, track.number)?)));
    if let Some((mtime, o)) = user {
        let mut set = base_tag_set(toc, area, track, &options);
        o.apply(&mut set);
        if !o.names_album {
            apply_mc_album_suffix(&mut set, area.slot, MC_ALBUM_SUFFIX_ON.load(Ordering::Relaxed));
        }
        if set != default || !o.only_items() {
            match make(of_set(&set)) {
                Ok(l) => {
                    let mut l = l;
                    let mut tag = region(&l, RegionKind::Trailer);
                    if !o.only_items() {
                        match crate::tags::append_frames(&tag, &o)
                            .map(|raw| (make(Tags::Raw(raw.clone())), raw))
                        {
                            Some((Ok(raw_layout), raw)) => {
                                l = raw_layout;
                                tag = raw;
                            }
                            _ => log::warn!(
                                "{:?} area track {}: the tags file's frames and art do not fit in the tag; left out",
                                area.slot,
                                track.number
                            ),
                        }
                    }
                    return Some(Tagged { layout: l, set, stamp: Some(Stamp::new(mtime, &tag)) });
                }
                Err(e) => {
                    log::warn!("{:?} area track {}: tags file not applied: {e}", area.slot, track.number)
                }
            }
        }
    }
    let layout = make(of_set(&default))
        .inspect_err(|e| log::debug!("{:?} area track {}: no DSF file: {e}", area.slot, track.number))
        .ok()?;
    Some(Tagged { layout, set: default, stamp: None })
}

/// The DSF layout of `track` of `area`, with its tag; `None` if the library refuses the
/// track (then no file is shown for it).
pub fn layout(toc: &Toc, area: &Area, track: &Track) -> Option<LibLayout> {
    tagged(toc, area, track).map(|t| t.layout)
}

/// The bytes of the regions of `kind` of `layout` (its header or its tag), which hold no
/// audio.
pub fn region(layout: &LibLayout, kind: RegionKind) -> Vec<u8> {
    let mut out = Vec::new();
    for r in layout.regions().iter().filter(|r| r.kind == kind) {
        let at = out.len();
        out.resize(at + r.len as usize, 0);
        if let Err(e) = layout.fill(r.offset, &mut out[at..], &mut NoAudio) {
            log::debug!("DSF {kind:?}: {e}");
        }
    }
    out
}

/// A source of no audio, for the regions that need none.
struct NoAudio;

impl PositionSource for NoAudio {
    fn content(&mut self, _t: u32) -> Result<std::borrow::Cow<'_, [u8]>, scarletbook::Error> {
        Err(scarletbook::Error::InvalidArgument("no audio here"))
    }
}

/// The DSF channel type for a channel count, for areas without a defined loudspeaker
/// layout.
pub fn count_channel_type(channels: u8) -> u32 {
    match channels {
        1 => 1, // mono
        2 => 2, // stereo
        3 => 3, // 3 channels
        4 => 4, // quad
        5 => 6, // 5 channels
        _ => 7, // 5.1 channels
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::*;

    #[test]
    fn album_suffix_only_for_multichannel() {
        let mk = || {
            let mut s = TagSet::default();
            s.set(TagItem::Album, "Title");
            s
        };
        let mut mc = mk();
        apply_mc_album_suffix(&mut mc, Slot::Multichannel, true);
        assert_eq!(mc.get(TagItem::Album), ["Title (Multichannel)"]);
        for (slot, on) in [(Slot::TwoChannel, true), (Slot::Multichannel, false)] {
            let mut s = mk();
            apply_mc_album_suffix(&mut s, slot, on);
            assert_eq!(s.get(TagItem::Album), ["Title"]);
        }
        let mut none = TagSet::default();
        apply_mc_album_suffix(&mut none, Slot::Multichannel, true);
        assert!(none.get(TagItem::Album).is_empty());
    }

    #[test]
    fn channel_types() {
        assert_eq!((1..=6).map(count_channel_type).collect::<Vec<_>>(), [1, 2, 3, 4, 6, 7]);
    }
}
