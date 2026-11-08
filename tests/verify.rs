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
use std::process::{Command, Stdio};
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
        pauses: "dropped".into(),
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

/// The extracts of the crash tests: `n` copies of each served track beside the image, and a
/// bin to run on them.
fn copies(dir: &Path, n: usize) -> PathBuf {
    let lib = dir.join("lib");
    fs::create_dir_all(&lib).unwrap();
    std::os::unix::fs::symlink(common::corpus().join("images/dsd-stereo-mch-2048.iso"), lib.join("x.iso"))
        .unwrap();
    let files = served(&lib);
    for (i, (_, _, bytes)) in files.iter().enumerate() {
        fs::write(lib.join(format!("t{i}.dsf")), bytes).unwrap();
        for c in 1..n {
            fs::hard_link(lib.join(format!("t{i}.dsf")), lib.join(format!("t{i}-{c}.dsf"))).unwrap();
        }
    }
    lib
}

fn run_verify(lib: &Path, report: &Path, extra: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_sacdfs"))
        .args(["verify", "-q", "--threads", "1", "-o"])
        .arg(report)
        .args(extra)
        .arg(lib)
        .output()
        .unwrap()
}

/// `sacdfs verify` killed with SIGKILL once its partial report has rows, or `None` if it
/// finished first.
fn killed_verify(lib: &Path, report: &Path, extra: &[&str]) -> Option<()> {
    use std::os::unix::process::ExitStatusExt;
    let partial = PathBuf::from(format!("{}.partial", report.display()));
    let _ = fs::remove_file(&partial);
    let mut child = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
        .args(["verify", "-q", "--threads", "1", "-o"])
        .arg(report)
        .args(extra)
        .arg(lib)
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    while fs::read_to_string(&partial).map_or(0, |t| t.lines().count()) < 3 {
        if child.try_wait().unwrap().is_some() {
            return None;
        }
    }
    child.kill().unwrap();
    let status = child.wait().unwrap();
    assert!(!status.success());
    // It may have finished just before the signal.
    (status.signal() == Some(9) && !report.exists()).then_some(())
}

/// #196: killed runs leave nothing that reads as finished, and resuming ends with what a run
/// that was never killed writes, from a stopped run's rows and from a finished report.
#[test]
fn kill_and_resume() {
    let dir = tempfile::tempdir().unwrap();
    let lib = copies(dir.path(), 1500);
    let report = dir.path().join("r.tsv");
    let partial = dir.path().join("r.tsv.partial");
    let previous = dir.path().join("r.tsv.previous");

    // The first run, killed: no report at all to read, only rows named as partial.
    killed_verify(&lib, &report, &[]).expect("the run ended before it could be killed");
    assert!(!report.exists() && partial.exists() && !previous.exists());
    let rows = fs::read_to_string(&partial).unwrap().lines().count();
    assert!(rows >= 3);

    // Resumed from those rows: a finished report, the same as a run from nothing.
    let out = run_verify(&lib, &report, &["--resume"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert!(!partial.exists() && !previous.exists());
    let finished = fs::read_to_string(&report).unwrap();
    assert!(finished.lines().all(|l| l.starts_with("IDENTICAL") || l.starts_with("status")));
    let total = finished.lines().count();
    assert!(total > rows, "resumed before the first run was through");
    let fresh = dir.path().join("fresh.tsv");
    assert_eq!(run_verify(&lib, &fresh, &[]).status.code(), Some(0));
    assert_eq!(fs::read_to_string(&fresh).unwrap(), finished);

    // Resumed from a finished report: nothing to do, the same report, no leftovers.
    assert_eq!(run_verify(&lib, &report, &["--resume"]).status.code(), Some(0));
    assert_eq!(fs::read_to_string(&report).unwrap(), finished);
    assert!(!partial.exists() && !previous.exists());

    // A run killed with a finished report in place keeps that report aside, under a name
    // that isn't the report's. So does a second one, with no report left to move.
    for _ in 0..2 {
        killed_verify(&lib, &report, &[]).expect("the run ended before it could be killed");
        assert!(!report.exists());
        assert_eq!(fs::read_to_string(&previous).unwrap(), finished);
    }

    // Resuming after that keeps the rows of the report aside, and tidies up. The kept rows go
    // to the partial report at once, so that killing this one costs nothing.
    let out = run_verify(&lib, &report, &["--resume"]);
    assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(fs::read_to_string(&report).unwrap(), finished);
    assert!(!partial.exists() && !previous.exists());

    // A run that can't start touches nothing.
    let out = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
        .args(["verify", "-q", "-o"])
        .arg(&report)
        .arg(dir.path().join("no-such-source"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(fs::read_to_string(&report).unwrap(), finished);
    assert!(!partial.exists() && !previous.exists());
}

/// A DFF file whose sound chunk claims so much that a sample count overflowed in 0.4.0, which
/// panicked: damaged, or at worst a bug. In no case a clean run without it (#196).
fn huge_dff() -> Vec<u8> {
    fn chunk(out: &mut Vec<u8>, id: &[u8; 4], size: u64, body: &[u8]) {
        out.extend_from_slice(id);
        out.extend_from_slice(&size.to_be_bytes());
        out.extend_from_slice(body);
    }
    let mut snd = b"SND ".to_vec();
    chunk(&mut snd, b"FS  ", 4, &2_822_400u32.to_be_bytes());
    chunk(&mut snd, b"CHNL", 2, &1u16.to_be_bytes());
    let mut form = b"DSD ".to_vec();
    chunk(&mut form, b"PROP", snd.len() as u64, &snd);
    chunk(&mut form, b"DSD ", 1 << 61, &[0; 16]);
    let mut file = Vec::new();
    chunk(&mut file, b"FRM8", form.len() as u64, &form);
    file
}

/// #196 through the command line, with any number of threads: an extract that makes a worker
/// panic is an ERROR row and exit status 2, the other files are all checked, and the guide's
/// list of what to delete holds only those that are IDENTICAL.
#[test]
fn a_file_that_panicked_is_not_a_clean_run() {
    let dir = tempfile::tempdir().unwrap();
    let lib = copies(dir.path(), 1);
    fs::write(lib.join("a-first.dff"), huge_dff()).unwrap();
    let dsf = walkdir(&lib).into_iter().filter(|p| p.extension().is_some_and(|e| e == "dsf")).count();
    assert!(dsf >= 2);
    for threads in ["1", "3"] {
        let report = dir.path().join(format!("r{threads}.tsv"));
        let out = Command::new(env!("CARGO_BIN_EXE_sacdfs"))
            .args(["verify", "-q", "--threads", threads, "-o"])
            .arg(&report)
            .arg(&lib)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
        let text = fs::read_to_string(&report).unwrap();
        assert_eq!(text.lines().count(), dsf + 2, "a row per file and the header: {text}");
        let status = |path: &str| {
            text.lines().find(|l| l.split('\t').nth(1).is_some_and(|p| p.ends_with(path))).unwrap()
        };
        assert!(status("a-first.dff").starts_with("ERROR\t"), "{text}");
        let listed = Command::new("awk")
            .args(["-F\t", "$1 == \"IDENTICAL\" && $11 == \"\" {print $2}"])
            .arg(&report)
            .output()
            .unwrap();
        let listed = String::from_utf8(listed.stdout).unwrap();
        assert_eq!(listed.lines().count(), dsf);
        assert!(!listed.contains("a-first.dff"));
    }
}

/// #196: a report to a pipe is a stream: every row comes through, and only the exit status says
/// how it went. Standard output too.
#[test]
fn report_to_a_pipe() {
    let dir = tempfile::tempdir().unwrap();
    let lib = copies(dir.path(), 1);
    fs::write(lib.join("a-first.dff"), huge_dff()).unwrap();
    let dsf = walkdir(&lib).into_iter().filter(|p| p.extension().is_some_and(|e| e == "dsf")).count();
    let bin = env!("CARGO_BIN_EXE_sacdfs");
    let check = |out: std::process::Output| {
        assert_eq!(out.status.code(), Some(2), "{}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8(out.stdout).unwrap();
        assert_eq!(text.lines().count(), dsf + 2, "{text}");
        assert!(text.lines().any(|l| l.starts_with("ERROR\t")), "{text}");
    };
    // No -o; and -o /dev/stdout, with standard output a pipe.
    check(Command::new(bin).args(["verify", "-q"]).arg(&lib).output().unwrap());
    check(Command::new(bin).args(["verify", "-q", "-o", "/dev/stdout"]).arg(&lib).output().unwrap());
    check(
        Command::new(bin).args(["verify", "-q", "-o", "/dev/stdout", "--resume"]).arg(&lib).output().unwrap(),
    );
    let leftovers: Vec<_> = fs::read_dir("/dev")
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("stdout."))
        .collect();
    assert!(leftovers.is_empty());
}

/// #202: `-o` naming a descriptor is a stream whatever it is redirected to, a regular file too:
/// finished, with every row, adding to what the file held, and with nothing left beside it.
#[test]
fn report_to_a_descriptor_redirected_to_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let lib = copies(dir.path(), 1);
    let dsf = walkdir(&lib).into_iter().filter(|p| p.extension().is_some_and(|e| e == "dsf")).count();
    let bin = env!("CARGO_BIN_EXE_sacdfs");
    let file = dir.path().join("out.tsv");
    // As `>` opens it, and as `>>` does.
    let open = |text: &str| {
        fs::write(&file, text).unwrap();
        fs::OpenOptions::new().write(true).append(!text.is_empty()).open(&file).unwrap()
    };
    let check = |out: std::process::Output, held: &str| {
        assert_eq!(out.status.code(), Some(0), "{}", String::from_utf8_lossy(&out.stderr));
        let text = fs::read_to_string(&file).unwrap();
        let rest = text.strip_prefix(held).unwrap_or_else(|| panic!("what it held is gone: {text}"));
        assert!(rest.starts_with("status\t"), "{text}");
        // The summary shares standard error with such a report, and overwrites none of it.
        assert_eq!(rest.lines().filter(|l| l.starts_with("IDENTICAL\t")).count(), dsf, "{text}");
    };
    let verify = |o: &str| {
        let mut c = Command::new(bin);
        c.args(["verify", "-q", "-o", o]).arg(&lib);
        c
    };

    // Standard output, also through a link of the user's, and with --resume.
    let link = dir.path().join("link");
    std::os::unix::fs::symlink("/dev/stdout", &link).unwrap();
    for (o, resume) in [
        ("/dev/stdout", false),
        ("/dev/stdout", true),
        ("/proc/self/fd/1", false),
        (link.to_str().unwrap(), false),
    ] {
        let mut c = verify(o);
        if resume {
            c.arg("--resume");
        }
        check(c.stdout(open("")).output().unwrap(), "");
    }
    // `>>` keeps what the file holds.
    check(verify("/dev/stdout").stdout(open("before\n")).output().unwrap(), "before\n");
    // Standard error, `2>` and `2>>`.
    check(verify("/dev/stderr").stderr(open("")).output().unwrap(), "");
    check(verify("/dev/stderr").stderr(open("before\n")).output().unwrap(), "before\n");
    // Another descriptor, set up by a shell.
    fs::write(&file, "").unwrap();
    let out = Command::new("sh")
        .arg("-c")
        .arg(r#""$0" verify -q -o /dev/fd/3 "$1" 3>"$2""#)
        .args([bin.as_ref(), lib.as_os_str(), file.as_os_str()])
        .output()
        .unwrap();
    check(out, "");
    assert!(!dir.path().join("out.tsv.partial").exists());

    // A descriptor that isn't open is an error, not a report beside it. Closed by the shell
    // first: a wrapper of the test run may hold it open (`flock 9`), and the child inherits it
    // (#272).
    let out = Command::new("sh")
        .arg("-c")
        .arg(r#"exec 9>&-; exec "$0" verify -q -o /dev/fd/9 "$1""#)
        .args([bin.as_ref(), lib.as_os_str()])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1), "{}", String::from_utf8_lossy(&out.stderr));
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
