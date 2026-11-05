// SPDX-License-Identifier: GPL-3.0-or-later

//! Writes the seed inputs of the fuzz targets (`fuzz/`, #161) from the test corpus and from
//! what sacdfs itself serves for it, so that the fuzzers start from inputs that pass the
//! first checks. See fuzz/README.md.
//!
//! ```sh
//! cargo run --locked --example fuzz_seeds -- fuzz/corpus
//! ```
//!
//! Writes `OUTDIR/<target>/seed-*`. No seed is checked in: they are made from the corpus
//! whenever the fuzzers run.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::fs;
use std::path::{Path, PathBuf};

use sacdfs::dsf;
use sacdfs::engine::Engine;
use sacdfs::tags::TagsFile;
use sacdfs::track::Reader;
use sacdfs::tree::{SourceDir, Tree};
use sacdfs::verify::id3;
use sacdfs::verify::report::escape;
use scarletbook::Disc;

/// The separator of the targets' fields (`sacdfs_fuzz::SEP`).
const SEP: u8 = 0xFE;

/// Bytes of a 2048-byte-sector image before its master TOC.
const LEAD_IN: usize = 510 * 2048;

/// Longest input the fuzzers are given (`-max_len` in fuzz/run.sh).
const MAX_INPUT: usize = 256 << 10;

struct Out {
    dir: PathBuf,
    count: usize,
}

impl Out {
    fn seed(&mut self, target: &str, bytes: &[u8]) {
        let dir = self.dir.join(target);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(format!("seed-{:05}", self.count)), &bytes[..bytes.len().min(MAX_INPUT)]).unwrap();
        self.count += 1;
    }
}

fn joined(parts: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            out.push(SEP);
        }
        out.extend_from_slice(p);
    }
    out
}

/// The tags files of the user guide's kinds: every level, every kind of key, picture names
/// fair and foul.
const TAGS: [&str; 6] = [
    "[album]\ngenre = \"Jazz\"\nart = \"cover.jpg\"\n\"TXXX:MusicBrainz Album Id\" = \"abc\"\n\n[track.3]\ntitle = \"Three\"\n\"UFID:http://musicbrainz.org\" = \"rec-3\"\n\n[multichannel.album]\nalbum = \"Name (Multichannel)\"\n\n[stereo.track.3]\n\"TXXX:replaygain_track_gain\" = \"-6.20 dB\"\n",
    "[album]\ntrack = \"1/12\"\ndisc = \"2/3\"\nTBPM = \"120\"\nCOMM = \"note\"\n\"COMM:desc\" = \"note\"\n",
    "[track.1]\ntitle = \"\"\nart = \"\"\n[track.255]\nisrc = \"XX\"\n",
    "[album]\nart = \"../secret.jpg\"\n[track.1]\nart = \"note.txt\"\n[track.2]\nart = \"cover.png\"\n",
    "[stereo.album]\nalbum = \"S\"\n[multichannel.track.1]\ntitle = \"日本語\\u0000\\n\"\n",
    "[track.0]\ntitle = \"x\"\n",
];

/// A DSF file of 1-bit audio with block size 4096 and `audio` bytes per channel.
fn dsf_file(channels: u32, audio: usize, tag: &[u8]) -> Vec<u8> {
    let data = audio.div_ceil(4096) * 4096 * channels as usize;
    let mut f = b"DSD ".to_vec();
    f.extend_from_slice(&28u64.to_le_bytes());
    f.extend_from_slice(&((92 + data + tag.len()) as u64).to_le_bytes());
    f.extend_from_slice(&(if tag.is_empty() { 0 } else { 92 + data } as u64).to_le_bytes());
    f.extend_from_slice(b"fmt ");
    f.extend_from_slice(&52u64.to_le_bytes());
    for v in [1, 0, channels, channels, 2_822_400, 1] {
        f.extend_from_slice(&u32::to_le_bytes(v));
    }
    f.extend_from_slice(&(audio as u64 * 8).to_le_bytes());
    f.extend_from_slice(&4096u32.to_le_bytes());
    f.extend_from_slice(&0u32.to_le_bytes());
    f.extend_from_slice(b"data");
    f.extend_from_slice(&((12 + data) as u64).to_le_bytes());
    f.extend((0..data).map(|i| ((i * 31) >> 3) as u8));
    f.extend_from_slice(tag);
    f
}

fn be_chunk(out: &mut Vec<u8>, id: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(id);
    out.extend_from_slice(&(body.len() as u64).to_be_bytes());
    out.extend_from_slice(body);
    if body.len() % 2 == 1 {
        out.push(0);
    }
}

/// A DFF file with every chunk the reader looks at.
fn dff_file(channels: u16, audio: usize, compression: &[u8; 4], tag: &[u8]) -> Vec<u8> {
    let mut snd = b"SND ".to_vec();
    be_chunk(&mut snd, b"FS  ", &2_822_400u32.to_be_bytes());
    be_chunk(&mut snd, b"CHNL", &channels.to_be_bytes());
    be_chunk(&mut snd, b"CMPR", compression);
    let mut form = b"DSD ".to_vec();
    be_chunk(&mut form, b"FVER", &[1, 5, 0, 0]);
    be_chunk(&mut form, b"PROP", &snd);
    let mut title = 5u32.to_be_bytes().to_vec();
    title.extend_from_slice(b"Title");
    let mut info = Vec::new();
    be_chunk(&mut info, b"DITI", &title);
    be_chunk(&mut info, b"DIAR", &title);
    be_chunk(&mut info, b"EMID", b"x");
    be_chunk(&mut form, b"DIIN", &info);
    be_chunk(&mut form, b"COMT", &[0, 0]);
    let data: Vec<u8> = (0..audio * usize::from(channels)).map(|i| ((i * 31) >> 3) as u8).collect();
    be_chunk(&mut form, b"DSD ", &data);
    if !tag.is_empty() {
        be_chunk(&mut form, b"ID3 ", tag);
    }
    let mut f = Vec::new();
    be_chunk(&mut f, b"FRM8", &form);
    f
}

/// A tag with a frame of each kind the reader treats apart, at ID3v2 version `major`.
fn old_tag(major: u8, flags: u8) -> Vec<u8> {
    let frame = |id: &str, data: &[u8]| {
        let mut f = id.as_bytes().to_vec();
        if major == 2 {
            f.extend_from_slice(&(data.len() as u32).to_be_bytes()[1..]);
        } else {
            f.extend_from_slice(&(data.len() as u32).to_be_bytes());
            f.extend_from_slice(&[0, 0]);
        }
        f.extend_from_slice(data);
        f
    };
    let mut body = Vec::new();
    if flags & 0x40 != 0 {
        body.extend_from_slice(&[0, 0, 0, 6, 0, 0, 0, 0, 0, 0]);
    }
    let ids: &[(&str, &[u8])] = if major == 2 {
        &[
            ("TT2", b"\0Title"),
            ("TXX", b"\0desc\0value"),
            ("COM", b"\0engd\0text"),
            ("PIC", b"\0JPG\x03\0\xFF\xD8\xFF"),
        ]
    } else {
        &[
            ("TIT2", b"\x01\xFF\xFET\0i\0"),
            ("TPE1", b"\x02\0A"),
            ("TXXX", b"\x03desc\0value"),
            ("COMM", b"\0engd\0text"),
            ("UFID", b"http://musicbrainz.org\0id"),
            ("TYER", b"\x002013"),
            ("TDAT", b"\x002304"),
            ("APIC", b"\0image/jpeg\0\x03d\0\xFF\xD8\xFFimg"),
        ]
    };
    for (id, data) in ids {
        body.extend_from_slice(&frame(id, data));
    }
    let n = body.len() as u32;
    let mut t = vec![b'I', b'D', b'3', major, 0, flags];
    t.extend_from_slice(&[
        (n >> 21) as u8 & 0x7F,
        (n >> 14) as u8 & 0x7F,
        (n >> 7) as u8 & 0x7F,
        n as u8 & 0x7F,
    ]);
    t.extend_from_slice(&body);
    t
}

/// The inputs for one image: its TOC region, its text and what sacdfs makes of them.
fn from_image(out: &mut Out, path: &Path, engine: &Engine) {
    let bytes = fs::read(path).unwrap();
    // Only 2048-byte sectors: the targets put the lead-in back at that size.
    if bytes.len() <= LEAD_IN || bytes.get(LEAD_IN..LEAD_IN + 8) != Some(b"SACDMTOC") {
        return;
    }
    let tail = &bytes[LEAD_IN..];
    out.seed("disc_text", tail);
    out.seed("tree", &joined(&[b"a", b"b", tail]));
    let Ok(disc) = Disc::open(std::fs::File::open(path).unwrap()) else { return };
    let toc = disc.toc();

    let mut titles: Vec<u8> = vec![7, 0];
    let mut values: Vec<u8> = vec![0; 16];
    for area in &toc.areas {
        for track in &area.tracks {
            if let Some(title) = &track.text.title {
                titles.extend_from_slice(title.as_bytes());
                titles.push(SEP);
            }
        }
        if let Some(track) = area.tracks.first() {
            let (set, _) = dsf::tag_set(toc, area, track, &dsf::options(area, track), None);
            for (_, _, item) in sacdfs::tags::ITEMS {
                values.extend_from_slice(set.get(item).first().map_or("", String::as_str).as_bytes());
                values.push(SEP);
            }
        }
    }
    out.seed("names", &titles);
    out.seed("headers", &values);

    // What the mount serves for it, for the readers of `verify`.
    let dir = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(path.canonicalize().unwrap(), dir.path().join("x.iso")).unwrap();
    let tree = Tree::new(SourceDir::scan(dir.path()).unwrap());
    for (_, file) in tree.all_files().into_iter().take(4) {
        out.seed("verify_id3", file.tag());
        if file.size <= MAX_INPUT as u64 && !matches!(file.format(), sacdfs::track::Format::Wav) {
            if let Ok((b, _)) = Reader::new(file.clone()).read(engine, 0, file.size) {
                out.seed("verify_container", &b);
                out.seed("verify_container", &joined(&[&b, &b]));
            }
        }
        let frames: Vec<String> = id3::frames(file.tag()).iter().map(|f| f.key.clone()).collect();
        out.seed(
            "verify_report",
            format!(
                "IDENTICAL\t{p}\t1\t2001-01-01T00:00:00\t{p}\tStereo\t1\t2\t8\t{h}\tother={k}\t{r}\n",
                p = escape(b"a/\xff\tb"),
                h = "00",
                k = frames.join("|"),
                r = escape(b"a\nb"),
            )
            .as_bytes(),
        );
    }
}

fn main() {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let [dir] = &args[..] else { panic!("usage: fuzz_seeds OUTDIR") };
    let mut out = Out { dir: PathBuf::from(dir), count: 0 };

    let images = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/corpus/images");
    let mut paths: Vec<PathBuf> = fs::read_dir(&images).unwrap().map(|e| e.unwrap().path()).collect();
    paths.sort();
    let engine = Engine::new(2, 64 << 20).unwrap();
    for path in &paths {
        from_image(&mut out, path, &engine);
    }

    for text in TAGS {
        assert!(text.contains('[') && (TagsFile::parse(text).is_ok() || text.contains("track.0")));
        out.seed("tags_toml", text.as_bytes());
    }

    for (list, name) in [
        ("jpg,jpeg,png,webp,m3u,m3u8,Artwork/,Scans/", "folder.jpg"),
        ("default,.cue,Covers/", "Covers"),
        ("default,-webp,-Scans/", ".hidden.jpg"),
        ("none,m3u,iso", "x.iso"),
    ] {
        for link in [
            "../../secret.jpg",
            "in.jpg",
            "../mnt/m.jpg",
            "@ROOT@/d/in.jpg",
            "@OUT@/secret.jpg",
            "up/secret.jpg",
            "./mntlink/m.jpg",
        ] {
            for rel in
                ["d/in.jpg", "d/out.jpg", "d/up/secret.jpg", "../secret.jpg", "mnt/m.jpg", "/etc/passwd"]
            {
                out.seed(
                    "sidecars",
                    &joined(&[list.as_bytes(), name.as_bytes(), link.as_bytes(), rel.as_bytes()]),
                );
            }
        }
    }

    out.seed("names", &joined(&[&[1, 0], b"a.b", b"a.b", &[b'a'; 300], &[0xFF; 255]]));
    for flags in [0, 0x40, 0x80, 0xC0] {
        for major in [2, 3, 4] {
            let tag = old_tag(major, flags);
            out.seed("verify_id3", &tag);
            out.seed("verify_id3", &joined(&[&tag, &old_tag(4, 0)]));
            out.seed("verify_container", &dsf_file(2, 5000, &tag));
            out.seed("verify_container", &dff_file(2, 5000, b"DSD ", &tag));
        }
    }
    let plain = dsf_file(2, 5000, b"");
    let small = dsf_file(2, 2048, b"");
    out.seed("verify_container", &joined(&[&plain, &small]));
    out.seed("verify_container", &dsf_file(1, 100, b""));
    out.seed("verify_container", &dsf_file(6, 100, b""));
    out.seed("verify_container", &dff_file(6, 100, b"DSD ", b""));
    out.seed("verify_container", &dff_file(2, 100, b"DST ", b""));

    println!("{} seeds in {}", out.count, out.dir.display());
}
