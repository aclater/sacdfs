// SPDX-License-Identifier: GPL-3.0-or-later

//! `sacdfs verify` (#80) on a library of corpus images with extracts made from them: the
//! files sacdfs serves, rewritten with other tags, as DFF, bit-flipped, truncated, cut
//! short, MSB-first, DST-compressed, and FLAC, next to the images and in a tree of their
//! own.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use sacdfs::disc::Tuning;
use sacdfs::engine::Engine;
use sacdfs::track::{Reader, TrackFile};
use sacdfs::tree::{Output, SourceDir, Tree};
use sacdfs::verify::{Args, Status, run};

/// The served DSF files of the images under `lib`: (path in the mount, bytes).
fn served(lib: &Path) -> Vec<(PathBuf, Arc<TrackFile>, Vec<u8>)> {
    let tree = Tree::with_options(SourceDir::scan(lib).unwrap(), Tuning::default(), Output::dsf_only());
    let engine = Engine::new(2, 16 << 20).unwrap();
    tree.all_files()
        .into_iter()
        .map(|(p, f)| {
            let r = Reader::new(f.clone());
            let (bytes, _) = r.read(&engine, 0, f.size).unwrap();
            (p, f, bytes)
        })
        .collect()
}

/// A served DSF file's parts: header, audio blocks, tag.
fn split(dsf: &[u8], f: &TrackFile) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    let h = f.header_len() as usize;
    let e = f.data_end() as usize;
    (dsf[..h].to_vec(), dsf[h..e].to_vec(), dsf[e..].to_vec())
}

/// A DSF file from a header, blocks and a tag, with its sizes and tag pointer fixed.
fn join(header: &[u8], data: &[u8], tag: &[u8]) -> Vec<u8> {
    let mut f = [header, data, tag].concat();
    let total = f.len() as u64;
    f[12..20].copy_from_slice(&total.to_le_bytes());
    let at = if tag.is_empty() { 0 } else { (header.len() + data.len()) as u64 };
    f[20..28].copy_from_slice(&at.to_le_bytes());
    f
}

/// Channels and blocks of a DSF header.
fn fmt(header: &[u8]) -> (usize, usize, usize) {
    let u32at = |at: usize| u32::from_le_bytes(header[at..at + 4].try_into().unwrap()) as usize;
    let samples = u64::from_le_bytes(header[64..72].try_into().unwrap()) as usize;
    (u32at(52), u32at(72), samples / 8)
}

/// The canonical stream (interleaved, MSB first) of DSF blocks.
fn canonical(header: &[u8], data: &[u8]) -> Vec<u8> {
    let (ch, block, bpc) = fmt(header);
    let mut out = vec![0u8; bpc * ch];
    for i in 0..bpc {
        for c in 0..ch {
            out[i * ch + c] = data[(i / block) * block * ch + c * block + i % block].reverse_bits();
        }
    }
    out
}

/// An ID3v2.4 tag of UTF-8 text frames (`TXXX:desc` for TXXX) and raw frames.
fn id3(text: &[(&str, &str)], raw: &[(&str, &[u8])]) -> Vec<u8> {
    let ss = |v: usize| {
        let v = v as u32;
        [((v >> 21) & 0x7F) as u8, ((v >> 14) & 0x7F) as u8, ((v >> 7) & 0x7F) as u8, (v & 0x7F) as u8]
    };
    let mut body = Vec::new();
    let mut put = |id: &str, d: &[u8]| {
        body.extend_from_slice(id.as_bytes());
        body.extend_from_slice(&ss(d.len()));
        body.extend_from_slice(&[0, 0]);
        body.extend_from_slice(d);
    };
    for (k, v) in text {
        let mut d = vec![3u8];
        let id = match k.strip_prefix("TXXX:") {
            Some(desc) => {
                d.extend_from_slice(desc.as_bytes());
                d.push(0);
                "TXXX"
            }
            None => k,
        };
        d.extend_from_slice(v.as_bytes());
        put(id, &d);
    }
    for (k, d) in raw {
        put(k, d);
    }
    let mut t = b"ID3\x04\x00\x00".to_vec();
    t.extend_from_slice(&ss(body.len()));
    t.extend_from_slice(&body);
    t
}

/// A DFF file holding `canon` (`compression` "DSD " or "DST ").
fn dff(canon: &[u8], ch: u16, compression: &[u8; 4]) -> Vec<u8> {
    let chunk = |id: &[u8], body: &[u8]| {
        let mut c = id.to_vec();
        c.extend_from_slice(&(body.len() as u64).to_be_bytes());
        c.extend_from_slice(body);
        if body.len() % 2 == 1 {
            c.push(0);
        }
        c
    };
    let mut prop = b"SND ".to_vec();
    prop.extend(chunk(b"FS  ", &2_822_400u32.to_be_bytes()));
    let mut chnl = ch.to_be_bytes().to_vec();
    for i in 0..ch {
        chnl.extend_from_slice(&[b'C', b'0', b'0', b'0' + i as u8]);
    }
    prop.extend(chunk(b"CHNL", &chnl));
    prop.extend(chunk(b"CMPR", &[&compression[..], b"\x00"].concat()));
    let mut form = b"DSD ".to_vec();
    form.extend(chunk(b"FVER", &0x0105_0000u32.to_be_bytes()));
    form.extend(chunk(b"PROP", &prop));
    form.extend(chunk(if compression == b"DST " { b"DST " } else { b"DSD " }, canon));
    chunk(b"FRM8", &form)
}

/// The library: two images in folders of their own, with extracts next to one and in a
/// tree of their own for the other. Returns (library, extract tree, expected status by
/// file name).
fn setup(dir: &Path) -> (PathBuf, PathBuf, BTreeMap<String, Status>) {
    let corpus = common::corpus().join("images");
    let lib = dir.join("lib");
    let ext = dir.join("ext");
    for (folder, image) in [("a", "dsd-stereo-mch-2048"), ("b", "dst-stereo-5ch-2054")] {
        fs::create_dir_all(lib.join(folder)).unwrap();
        std::os::unix::fs::symlink(
            corpus.join(format!("{image}.iso")),
            lib.join(folder).join(format!("{image}.iso")),
        )
        .unwrap();
    }
    let files = served(&lib);
    let pick = |prefix: &str, n: usize| {
        files
            .iter()
            .filter(|(p, _, _)| p.starts_with(prefix))
            .nth(n)
            .unwrap_or_else(|| panic!("{prefix} {n}"))
    };
    let mut want = BTreeMap::new();
    let mut put = |path: PathBuf, bytes: &[u8], status: Status| {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, bytes).unwrap();
        want.insert(path.file_name().unwrap().to_string_lossy().into_owned(), status);
    };
    let a = lib.join("a");
    let x = a.join("extracts").join("Stereo");

    // Stereo track 1 with another tag: identical audio, but MusicBrainz IDs and art lost.
    let (_, f, b) = pick("a/dsd-stereo-mch-2048/Stereo", 0);
    let (h, d, _) = split(b, f);
    let tag = id3(
        &[("TIT2", "Edited"), ("TXXX:MusicBrainz Album Id", "x")],
        &[("APIC", b"\x00image/png\x00\x03\x00png")],
    );
    put(x.join("01 - retagged.dsf"), &join(&h, &d, &tag), Status::Identical);
    // The same, as served, with no tag at all.
    put(x.join("01 - untagged.dsf"), &join(&h, &d, &[]), Status::Identical);
    // MSB-first (bits per sample 8).
    let mut msb_h = h.clone();
    msb_h[60..64].copy_from_slice(&8u32.to_le_bytes());
    let msb_d: Vec<u8> = d.iter().map(|b| b.reverse_bits()).collect();
    put(x.join("01 - msb.dsf"), &join(&msb_h, &msb_d, &[]), Status::Identical);
    // DFF of the same audio.
    let canon = canonical(&h, &d);
    put(x.join("01 - track.dff"), &dff(&canon, 2, b"DSD "), Status::Identical);
    // DST-compressed DFF.
    put(x.join("01 - dst.dff"), &dff(&canon, 2, b"DST "), Status::Unsupported);
    // One bit flipped in the audio.
    let mut flipped = d.clone();
    flipped[100] ^= 0x10;
    put(x.join("01 - flipped.dsf"), &join(&h, &flipped, &[]), Status::Different);
    // Truncated in the audio.
    let full = join(&h, &d, &[]);
    put(x.join("01 - truncated.dsf"), &full[..full.len() / 2], Status::Error);
    // 1000 bytes per channel short (no track has that length), with a header that says so.
    let (ch, block, bpc) = fmt(&h);
    let short_bpc = bpc - 1000;
    let mut sh = h.clone();
    sh[64..72].copy_from_slice(&((short_bpc * 8) as u64).to_le_bytes());
    let groups = short_bpc.div_ceil(block);
    let mut sd = vec![0u8; groups * block * ch];
    for i in 0..short_bpc {
        for c in 0..ch {
            let at = (i / block) * block * ch + c * block + i % block;
            sd[at] = d[at];
        }
    }
    put(x.join("01 - short.dsf"), &join(&sh, &sd, &[]), Status::NoMatch);
    // FLAC.
    put(a.join("cd").join("01 - cd.flac"), b"fLaC\0\0\0\x22", Status::NotComparable);
    // Not an audio file at all.
    put(x.join("02 - garbage.dsf"), b"not a dsf file", Status::Error);
    // A multichannel track as DFF.
    let (_, f, b) = pick("a/dsd-stereo-mch-2048/Multichannel", 1);
    let (h, d, _) = split(b, f);
    put(
        a.join("mch").join("02 - mch.dff"),
        &dff(&canonical(&h, &d), fmt(&h).0 as u16, b"DSD "),
        Status::Identical,
    );

    // Sidecar files, next to the images and among the extracts kept apart: not audio,
    // never extracts (#79).
    for p in [
        a.join("folder.jpg"),
        a.join("album.m3u"),
        a.join("Artwork").join("back.png"),
        ext.join("b").join("cover.jpg"),
        ext.join("b").join("list.m3u8"),
    ] {
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(&p, b"not audio").unwrap();
    }
    // A DST image, extracts kept apart: every track as served.
    for (p, f, b) in files.iter().filter(|(p, _, _)| p.starts_with("b")) {
        let name = format!("b-{}-{}", p.parent().unwrap().file_name().unwrap().to_string_lossy(), f.number);
        put(ext.join("b").join(format!("{name}.dsf")), b, Status::Identical);
    }
    (lib, ext, want)
}

fn args(source: &Path, extracts: Vec<PathBuf>, map: Vec<PathBuf>, output: Option<PathBuf>) -> Args {
    Args {
        source: source.to_owned(),
        extracts,
        map,
        output,
        resume: false,
        threads: 3,
        cache_mb: 8,
        quiet: true,
        verbose: 0,
    }
}

#[test]
fn verify_classifies_every_extract() {
    let dir = tempfile::tempdir().unwrap();
    let (lib, ext, want) = setup(dir.path());
    let report = dir.path().join("report.tsv");
    let s = run(&args(
        &lib,
        vec![lib.clone(), ext.clone()],
        vec![ext.clone(), lib.clone()],
        Some(report.clone()),
    ))
    .unwrap();
    let got: BTreeMap<String, Status> = s
        .rows
        .iter()
        .map(|r| (r.path.file_name().unwrap().to_string_lossy().into_owned(), r.status))
        .collect();
    assert_eq!(got, want);
    let by = |name: &str| s.rows.iter().find(|r| r.path.file_name().unwrap() == name).unwrap();
    let r = by("01 - retagged.dsf");
    assert!(r.loses.musicbrainz && r.loses.art && !r.loses.replaygain, "{:?}", r.loses);
    assert!(r.loses.other.contains("TIT2"), "{:?}", r.loses);
    assert_eq!(r.track, Some(1));
    assert_eq!(r.area, "Stereo");
    assert!(by("01 - untagged.dsf").loses.is_empty());
    // The same audio, the same digest, whatever the container.
    assert_eq!(by("01 - track.dff").sha256, r.sha256);
    assert_eq!(by("01 - msb.dsf").sha256, r.sha256);
    assert!(by("01 - truncated.dsf").reason.contains("truncated"), "{}", by("01 - truncated.dsf").reason);
    assert!(by("01 - dst.dff").reason.contains("DST"));
    // Cut short: found inside the track (#93); what sacdfs serves beyond it is audio.
    assert!(by("01 - short.dsf").reason.contains("part of"), "{}", by("01 - short.dsf").reason);
    assert!(by("01 - short.dsf").reason.contains("audio at"), "{}", by("01 - short.dsf").reason);
    assert!(!s.all_identical());
    let text = s.render();
    assert!(text.contains("Nothing was deleted"));
    // The sidecars are neither rows nor checked, only counted.
    assert_eq!(s.ignored, 5, "{text}");
    assert!(text.contains("5 other files"), "{text}");
    assert!(s.rows.iter().all(|r| !matches!(
        r.path.extension().and_then(|e| e.to_str()),
        Some("jpg" | "png" | "m3u" | "m3u8")
    )));
    // The report has every row, sorted, and parses back.
    let tsv = fs::read_to_string(&report).unwrap();
    assert_eq!(tsv.lines().count(), want.len() + 1);
    assert!(tsv.starts_with("status\tpath\t"));
}

#[test]
fn separate_tree_without_map_finds_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let (lib, ext, _) = setup(dir.path());
    let s = run(&args(&lib, vec![ext], Vec::new(), None)).unwrap();
    assert!(s.rows.iter().all(|r| r.status == Status::NoMatch), "{:?}", s.rows);
    assert!(s.rows[0].reason.contains("--map"));
}

#[test]
fn command_line_exit_status_and_resume() {
    let dir = tempfile::tempdir().unwrap();
    let (lib, ext, want) = setup(dir.path());
    let report = dir.path().join("r.tsv");
    let bin = env!("CARGO_BIN_EXE_sacdfs");
    // SOURCE, and the extracts kept apart, all identical: exit status 0.
    let out = Command::new(bin)
        .args(["verify", "-q", "--threads", "2", "--map"])
        .arg(&ext)
        .arg(&lib)
        .arg("-o")
        .arg(&report)
        .arg(&lib)
        .arg(&ext)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("Reclaimable"), "{stderr}");
    // The whole library: some are not identical, exit status 2.
    let run = |resume: bool| {
        let mut c = Command::new(bin);
        c.args(["verify", "-q", "--map"]).arg(&ext).arg(&lib).arg("-o").arg(&report);
        if resume {
            c.arg("--resume");
        }
        c.arg(&lib).arg(&lib).arg(&ext).output().unwrap()
    };
    let first = run(false);
    assert_eq!(first.status.code(), Some(2));
    let a = fs::read_to_string(&report).unwrap();
    assert_eq!(a.lines().count(), want.len() + 1);
    // A resumed run keeps the rows (the ERROR rows are checked again) and writes the same.
    let second = run(true);
    assert_eq!(second.status.code(), Some(2));
    assert_eq!(fs::read_to_string(&report).unwrap(), a);
    // Verify never writes next to the extracts.
    let mut names: Vec<_> = walkdir(&lib)
        .into_iter()
        .filter(|p| p.extension().is_some_and(|e| e == "tsv" || e == "tmp"))
        .collect();
    names.retain(|p| p != &report);
    assert!(names.is_empty(), "{names:?}");
    // A sacdfs-style argument error.
    let bad = Command::new(bin).args(["verify", "/nonexistent-sacdfs-verify"]).output().unwrap();
    assert_eq!(bad.status.code(), Some(1));
}

fn walkdir(p: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for e in fs::read_dir(p).unwrap().flatten() {
        let path = e.path();
        if e.file_type().unwrap().is_dir() {
            out.extend(walkdir(&path));
        } else {
            out.push(path);
        }
    }
    out
}

/// #98: the image is in a subfolder of the extract's folder, not beside or above it.
#[test]
fn image_in_a_subfolder_is_found() {
    let dir = tempfile::tempdir().unwrap();
    let corpus = common::corpus().join("images").join("dsd-stereo-mch-2048.iso");
    let lib = dir.path().join("lib");
    let link = |rel: &str| {
        let d = lib.join(rel);
        fs::create_dir_all(&d).unwrap();
        std::os::unix::fs::symlink(&corpus, d.join("disc.iso")).unwrap();
    };
    link("near/Album (SACD ISO)");
    link("deep/Disc 1/ISO");
    link("toodeep/a/b/c");
    let files = served(&lib);
    let (_, _, bytes) = files
        .iter()
        .find(|(p, _, _)| p.starts_with("near") && p.to_string_lossy().contains("/Stereo/"))
        .unwrap();
    for folder in ["near", "deep", "toodeep"] {
        fs::write(lib.join(folder).join("01 - track.dsf"), bytes).unwrap();
    }
    let s = run(&args(&lib, vec![lib.clone()], Vec::new(), None)).unwrap();
    let by =
        |folder: &str| s.rows.iter().find(|r| r.path == lib.join(folder).join("01 - track.dsf")).unwrap();
    assert_eq!(by("near").status, Status::Identical, "{}", by("near").reason);
    assert_eq!(by("deep").status, Status::Identical, "{}", by("deep").reason);
    assert_eq!(by("toodeep").status, Status::NoMatch);
    assert!(by("toodeep").reason.contains("subfolders"), "{}", by("toodeep").reason);
}

/// #93: extracts that are a served track with pauses around it, or a track cut short.
#[test]
fn pause_kept_extracts() {
    let dir = tempfile::tempdir().unwrap();
    let lib = dir.path().join("lib");
    fs::create_dir_all(lib.join("a")).unwrap();
    std::os::unix::fs::symlink(
        common::corpus().join("images").join("dsd-stereo-mch-2048.iso"),
        lib.join("a").join("dsd-stereo-mch-2048.iso"),
    )
    .unwrap();
    let files = served(&lib);
    let (_, f, b) = files.iter().find(|(p, _, _)| p.starts_with("a/dsd-stereo-mch-2048/Stereo")).unwrap();
    let (h, d, _) = split(b, f);
    let canon = canonical(&h, &d);
    // Whole 1/75 s frames of two channels: 4704 bytes per channel.
    let idle = |frames: usize| vec![0x69u8; frames * 4704 * 2];
    let x = lib.join("a").join("x");
    fs::create_dir_all(&x).unwrap();
    let put = |name: &str, canon: Vec<u8>| fs::write(x.join(name), dff(&canon, 2, b"DSD ")).unwrap();
    put("01 - idle both.dff", [idle(3), canon.clone(), idle(2)].concat());
    put("01 - head only.dff", [idle(1), canon.clone()].concat());
    put("01 - residue tail.dff", [canon.clone(), [0x0F, 0xF0, 0x3C, 0xC3].repeat(4704 / 2)].concat());
    put("01 - loud tail.dff", [canon.clone(), vec![0xFF; 4704 * 2]].concat());
    put("01 - cut short.dff", canon[..canon.len() - 10].to_vec());
    let mut other = canon.clone();
    other[canon.len() / 2] ^= 0x10;
    put("01 - other audio.dff", [idle(1), other].concat());
    let s = run(&args(&lib, vec![x.clone()], Vec::new(), None)).unwrap();
    let by =
        |n: &str| s.rows.iter().find(|r| r.path.file_name().unwrap().to_str().unwrap().contains(n)).unwrap();
    for n in ["idle both", "head only", "residue tail"] {
        assert_eq!(by(n).status, Status::IdenticalPause, "{n}: {}", by(n).reason);
        assert_eq!(by(n).track, Some(1));
        assert!(by(n).loses.is_empty() || !by(n).loses.musicbrainz);
    }
    assert!(by("idle both").reason.contains("digital silence"), "{}", by("idle both").reason);
    assert!(by("idle both").reason.contains("0.040 s"), "{}", by("idle both").reason);
    assert!(by("residue tail").reason.contains("residue at"), "{}", by("residue tail").reason);
    assert_eq!(by("idle both").sha256, by("head only").sha256);
    let loud = by("loud tail");
    assert_eq!(loud.status, Status::NoMatch);
    assert!(loud.reason.contains("audio at 0.0 dB") && loud.reason.contains("would lose"), "{}", loud.reason);
    // Cut short: part of the track; whether it matches depends on whether the track ends in silence.
    let cut = by("cut short");
    assert!(cut.reason.contains("part of"), "{}", cut.reason);
    assert_eq!(by("other audio").status, Status::NoMatch);
    assert!(!by("other audio").reason.contains("holds"), "{}", by("other audio").reason);
    let text = s.render();
    assert!(text.contains("IDENTICAL-PAUSE"), "{text}");
    assert!(text.contains("3 of them IDENTICAL-PAUSE"), "{text}");
}
