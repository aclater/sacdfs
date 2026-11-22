// SPDX-License-Identifier: GPL-3.0-or-later

//! Gapless chains (#442): DSF extracts written without padding (`sacd_extract -z`, with or
//! without `-b`). Each file but the last ends on a whole DSF block (4096 bytes per channel) and
//! the rest of its track opens the next file, so no file but the first can match a track on
//! its own, and the first is short of it.
//!
//! The files of one folder that are numbered as consecutive tracks, have one channel count and
//! end on a whole block (all but the last) form a chain. Their audio joined is compared with
//! the joined audio of the served tracks of those numbers, of one area: the same, or the one
//! inside the other with only silence beyond (the area's last frame that older extractors
//! drop), as [`super::pause`] measures it. Where each file ends is not compared, so this does
//! not depend on how the extractor splits the chain. Every file of a chain that matches is
//! IDENTICAL-CHAIN: the whole of its audio is in tracks sacdfs serves.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use super::container::{self, FileSource, Joined, Source, Stream};
use super::pause::{self, Found};
use super::report::Pauses;
use super::{
    Candidate, Ctx, Row, SACD_RATE, Status, TrackSource, folder_candidates, id3, leading_number, losses,
};
use crate::disc::Tuning;
use crate::tree::{Output, SourceDir, Tree};

/// Samples per channel in a DSF block of 4096 bytes.
pub(super) const BLOCK_SAMPLES: u64 = 4096 * 8;

/// The files of one chain, as indexes into the rows, in track order.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct Chain {
    /// The rows, one per track from `first`.
    pub members: Vec<usize>,
    /// The track number of the first file.
    pub first: u32,
}

impl Chain {
    fn last(&self) -> u32 {
        self.first + self.members.len() as u32 - 1
    }
}

fn is_dsf(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("dsf"))
}

/// The chains among `rows` worth comparing: files of one folder numbered as consecutive tracks,
/// of one channel count, every one but the last a whole number of blocks, at least two of them,
/// and among them one that `fresh` (checked in this run) says is NO-MATCH or DIFFERENT.
pub(super) fn find(rows: &[Row], fresh: &dyn Fn(usize) -> bool) -> Vec<Chain> {
    let mut by_dir: BTreeMap<&Path, Vec<(u32, usize)>> = BTreeMap::new();
    for (i, r) in rows.iter().enumerate() {
        if !is_dsf(&r.path) || matches!(r.status, Status::Error | Status::Unsupported | Status::NotComparable)
        {
            continue;
        }
        let (Some(_), Some(_), Some(n), Some(dir)) =
            (r.channels, r.samples, leading_number(&r.path), r.path.parent())
        else {
            continue;
        };
        by_dir.entry(dir).or_default().push((n, i));
    }
    let mut out = Vec::new();
    for (_, mut files) in by_dir {
        files.sort();
        let mut run: Vec<(u32, usize)> = Vec::new();
        let mut close = |run: &mut Vec<(u32, usize)>| {
            let worth =
                run.len() >= 2 && run.iter().any(|&(_, i)| rows[i].status.chain_may_match() && fresh(i));
            if worth {
                out.push(Chain { members: run.iter().map(|&(_, i)| i).collect(), first: run[0].0 });
            }
            run.clear();
        };
        for (n, i) in files {
            let joins = run.last().is_some_and(|&(pn, pi)| {
                let p = &rows[pi];
                pn + 1 == n
                    && p.channels == rows[i].channels
                    && p.samples.is_some_and(|s| s % BLOCK_SAMPLES == 0)
            });
            if !joins {
                close(&mut run);
            }
            run.push((n, i));
        }
        close(&mut run);
    }
    out
}

/// The served tracks in the other pause mode, made only when a chain needs them: an extract
/// made with `-z -b` holds every pause, which only `--pauses kept` serves, and one made with
/// `-z` alone none of them.
pub(super) struct OtherMode<'a> {
    /// The other mode.
    mode: Pauses,
    scanned: &'a SourceDir,
    root: &'a Path,
    tree: OnceLock<Tree>,
    folders: Mutex<HashMap<PathBuf, Arc<Vec<Arc<Candidate>>>>>,
}

impl<'a> OtherMode<'a> {
    /// The mode other than `mode`, over the images of `scanned`.
    pub(super) fn new(scanned: &'a SourceDir, root: &'a Path, mode: Pauses) -> OtherMode<'a> {
        OtherMode {
            mode: mode.other(),
            scanned,
            root,
            tree: OnceLock::new(),
            folders: Mutex::new(HashMap::new()),
        }
    }

    fn candidates(&self, folder: &Path) -> Arc<Vec<Arc<Candidate>>> {
        let tree = self.tree.get_or_init(|| {
            Tree::with_options(
                self.scanned.clone(),
                Tuning::default(),
                Output { pauses_kept: self.mode.kept(), ..Output::dsf_only() },
            )
        });
        let mut f = self.folders.lock().unwrap_or_else(|p| p.into_inner());
        f.entry(folder.to_owned())
            .or_insert_with(|| Arc::new(folder_candidates(tree, self.root, folder)))
            .clone()
    }
}

/// Where the chain was found among the served tracks of one area.
struct Hit<'c> {
    /// The served tracks, from the chain's first number.
    tracks: Vec<&'c Arc<Candidate>>,
    found: Found,
    /// Whether the chain is the longer of the two.
    chain_longer: bool,
}

impl Hit<'_> {
    fn silent(&self) -> bool {
        self.found.is_silence()
    }

    /// "Stereo tracks 1 to 3 of IMAGE", with what lies beyond the common audio.
    fn describe(&self, first: u32, last: u32) -> String {
        let c = self.tracks[0];
        let what = format!("{} tracks {first} to {last} of {}", c.area, c.image().display());
        let extras = self.found.around("them");
        match (extras.is_empty(), self.chain_longer) {
            (true, _) => what,
            (false, true) => format!("{what}, plus {extras}"),
            (false, false) => format!("{what}, less {extras} that sacdfs serves"),
        }
    }
}

/// The extracts of a chain, opened.
fn open_chain(chain: &Chain, rows: &[Row]) -> Option<Joined<'static>> {
    let mut parts: Vec<(container::Audio, Box<dyn Source>)> = Vec::new();
    for &i in &chain.members {
        let mut src = FileSource::open(&rows[i].path).ok()?;
        let audio = container::parse(&mut src).ok()?;
        if audio.rate != SACD_RATE || Some(audio.samples) != rows[i].samples {
            return None;
        }
        parts.push((audio, Box::new(src)));
    }
    Joined::new(parts)
}

/// Looks for the chain among the served tracks `cands` (of the extracts' folders). The first
/// match with only silence beyond wins; failing that, the first found at all.
fn search<'c>(
    chain: &Chain,
    joined: &mut Joined<'_>,
    chain_digest: &mut Option<[u8; 32]>,
    cands: &'c [Arc<Candidate>],
    ctx: &Ctx<'_>,
) -> Result<Option<Hit<'c>>, ()> {
    let ch = joined.channels();
    let chain_samples = joined.samples();
    let max_extra = pause::MAX_EXTRA_SECS * u64::from(SACD_RATE);
    // The areas with every track of the chain's numbers.
    let mut areas: BTreeMap<(&Path, &str), Vec<&Arc<Candidate>>> = BTreeMap::new();
    for c in cands.iter().filter(|c| c.file.layout.channels == ch) {
        let n = u32::from(c.file.number);
        if (chain.first..=chain.last()).contains(&n) {
            areas.entry((c.image(), c.area.as_str())).or_default().push(c);
        }
    }
    let mut groups: Vec<(u64, Vec<&Arc<Candidate>>)> = areas
        .into_values()
        .filter_map(|mut t| {
            t.sort_by_key(|c| c.file.number);
            let numbered = t.len() == chain.members.len()
                && t.iter().zip(chain.first..).all(|(c, n)| u32::from(c.file.number) == n);
            let samples = t.iter().map(|c| c.file.layout.samples()).sum::<u64>();
            (numbered && samples.abs_diff(chain_samples) <= max_extra).then_some((samples, t))
        })
        .collect();
    // The same length first: one hash each decides.
    groups.sort_by_key(|(s, _)| s.abs_diff(chain_samples));
    let mut near = None;
    for (served_samples, tracks) in groups {
        let mut parts: Vec<(container::Audio, Box<dyn Source + '_>)> = Vec::new();
        for c in &tracks {
            let mut src = TrackSource::new(c.file.clone(), ctx.engine);
            let Ok(audio) = container::parse(&mut src) else { return Err(()) };
            parts.push((audio, Box::new(src)));
        }
        let Some(mut served) = Joined::new(parts) else { continue };
        let digest = match chain_digest {
            Some(d) => *d,
            None => *chain_digest.insert(joined.digest(&ctx.read).map_err(|_| ())?),
        };
        let chain_longer = chain_samples > served_samples;
        let found = if served_samples == chain_samples {
            let d = served.digest(&ctx.read).map_err(|_| ())?;
            (d == digest).then(|| Found {
                head: pause::Meter::new(ch).finish(),
                tail: pause::Meter::new(ch).finish(),
                offset_samples: 0,
            })
        } else if chain_longer {
            let d = served.digest(&ctx.read).map_err(|_| ())?;
            pause::locate(joined, &mut served, &d, &ctx.read).map_err(|_| ())?
        } else {
            pause::locate(&mut served, joined, &digest, &ctx.read).map_err(|_| ())?
        };
        let Some(found) = found else { continue };
        let hit = Hit { tracks, found, chain_longer };
        if hit.silent() {
            return Ok(Some(hit));
        }
        near.get_or_insert(hit);
    }
    Ok(near)
}

/// Compares one chain; the rows to replace, by index.
pub(super) fn check(
    chain: &Chain,
    rows: &[Row],
    folders: &[PathBuf],
    ctx: &Ctx<'_>,
    other: &OtherMode<'_>,
) -> Vec<(usize, Row)> {
    let Some(mut joined) = open_chain(chain, rows) else { return Vec::new() };
    let (first, last) = (chain.first, chain.last());
    let files = format!("files {first:02} to {last:02} of its folder");
    let mut digest = None;
    let here: Vec<Arc<Candidate>> =
        folders.iter().filter_map(|f| ctx.candidates.get(f)).flatten().cloned().collect();
    let hit = match search(chain, &mut joined, &mut digest, &here, ctx) {
        Ok(h) => h,
        Err(()) => return Vec::new(),
    };
    let note = |rows: &[Row], why: String| -> Vec<(usize, Row)> {
        chain
            .members
            .iter()
            .filter(|&&i| rows[i].status.chain_may_match())
            .map(|&i| {
                let mut r = rows[i].clone();
                r.reason = if r.reason.is_empty() { why.clone() } else { format!("{}; {why}", r.reason) };
                (i, r)
            })
            .collect()
    };
    match hit {
        Some(h) if h.silent() => {
            let digest = digest.map(|d| id3::hex(&d)).unwrap_or_default();
            let mut out = Vec::new();
            for (k, &i) in chain.members.iter().enumerate() {
                if !rows[i].status.chain_may_match() {
                    continue;
                }
                let c = h.tracks[k];
                let mut r = rows[i].clone();
                c.fill(&mut r);
                r.sha256 = digest.clone();
                r.loses = match FileSource::open(&r.path) {
                    Ok(mut src) => match container::parse(&mut src) {
                        Ok(a) => losses(&a, &mut src, c.tag(ctx.engine), c.area_tracks),
                        Err(_) => r.loses,
                    },
                    Err(_) => r.loses,
                };
                r.status = Status::IdenticalChain;
                r.reason = format!(
                    "a gapless chain (sacd_extract -z), {files}: joined, they hold {}",
                    h.describe(first, last)
                );
                out.push((i, r));
            }
            out
        }
        Some(h) => note(
            rows,
            format!(
                "{files} look like a gapless chain (sacd_extract -z): joined, they hold {}; that is not \
                 silence, so {}",
                h.describe(first, last),
                if h.chain_longer { "deleting them would lose it" } else { "they lack audio sacdfs serves" }
            ),
        ),
        None => {
            let all: Vec<Arc<Candidate>> =
                folders.iter().flat_map(|f| other.candidates(f).to_vec()).collect();
            match search(chain, &mut joined, &mut digest, &all, ctx) {
                Ok(Some(h)) if h.silent() => note(
                    rows,
                    format!(
                        "{files} are a gapless chain (sacd_extract -z) of {} as sacdfs serves them with \
                         --pauses {}: check again with that option",
                        h.describe(first, last),
                        other.mode.as_str()
                    ),
                ),
                _ => Vec::new(),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(path: &str, status: Status, samples: u64) -> Row {
        let mut r = Row::new(path.into(), 1, "1.5".into()).with(status, String::new());
        r.channels = Some(2);
        r.samples = Some(samples);
        r
    }

    #[test]
    fn chains_are_consecutive_tracks_on_whole_blocks() {
        let b = BLOCK_SAMPLES;
        let rows = vec![
            row("/x/01 - a.dsf", Status::NoMatch, 3 * b),
            row("/x/02 - b.dsf", Status::NoMatch, 5 * b),
            row("/x/03 - c.dsf", Status::NoMatch, 5 * b + 8),
            // Not on a block: ends the chain before it, and the next one starts after it.
            row("/x/04 - d.dsf", Status::NoMatch, 2 * b),
            row("/x/05 - e.dsf", Status::Different, 2 * b + 8),
            // A gap in the numbers, and a lone file.
            row("/x/07 - f.dsf", Status::NoMatch, 2 * b),
            // Matched files only: nothing to do.
            row("/y/01.dsf", Status::Identical, b),
            row("/y/02.dsf", Status::Identical, 8),
            // Not DSF, or broken.
            row("/z/01.dff", Status::NoMatch, b),
            row("/z/02.dff", Status::NoMatch, 8),
            row("/w/01.dsf", Status::NoMatch, b),
            row("/w/02.dsf", Status::Error, 8),
        ];
        let all = find(&rows, &|_| true);
        assert_eq!(
            all,
            vec![Chain { members: vec![0, 1, 2], first: 1 }, Chain { members: vec![3, 4], first: 4 }]
        );
        // A chain none of whose unmatched files were checked in this run is left as it was.
        assert_eq!(find(&rows, &|i| i == 4), vec![Chain { members: vec![3, 4], first: 4 }]);
        // Other channel counts don't chain.
        let mut mixed = rows[..3].to_vec();
        mixed[1].channels = Some(6);
        assert!(find(&mixed, &|_| true).is_empty());
    }
}
