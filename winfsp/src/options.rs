// SPDX-License-Identifier: GPL-3.0-or-later

//! `sacdfs-win`'s command line: the Linux options that mean the same on Windows, with the
//! same names and defaults, plus the few that only Windows has.

use std::path::PathBuf;

use clap::Parser;
use sacdfs::serve::{Limits, Options};

/// Show the audio of SACD disc images as DSF and WAV files on Windows, decoded on demand.
///
/// SOURCE is an image file or a folder searched recursively for *.iso files. MOUNTPOINT is a
/// drive letter (`X:`) or a folder that does not exist yet. Each image appears as
/// MOUNTPOINT\<relative folder>\<image name>\{Stereo,Multichannel}\NN - title.dsf, with
/// NN - title.wav (24-bit PCM, converted on demand) next to it. The mount is read-only and
/// lasts until sacdfs-win stops: Ctrl+C or Ctrl+Break in its console, or a stop of the
/// service or WinFsp.Launcher instance that runs it.
#[derive(Debug, Parser)]
#[command(name = "sacdfs-win", version = crate::notice::VERSION, long_version = crate::notice::LONG_VERSION, about)]
pub struct Args {
    /// An SACD image file, or a folder of them.
    pub source: PathBuf,
    /// A drive letter (`X:`) or a folder that does not exist yet.
    pub mountpoint: String,
    /// The options every front end shares, as on Linux.
    #[command(flatten)]
    pub options: Options,
    /// Let Windows cache file contents and attributes until sacdfs-win says they changed
    /// (the default). With --no-kernel-cache every read and every query reaches sacdfs-win.
    #[arg(long)]
    pub no_kernel_cache: bool,
    /// Let other users of this PC read the mount: the default when sacdfs-win runs as SYSTEM
    /// (as a service or under WinFsp.Launcher). Needed to share the drive over SMB.
    #[arg(long)]
    pub allow_other: bool,
    /// Only the user who mounted (and SYSTEM and Administrators) may read the mount, also
    /// when run as SYSTEM.
    #[arg(long, conflicts_with = "allow_other")]
    pub no_allow_other: bool,
    #[command(flatten)]
    pub limits: Limits,
    /// Append the log to FILE instead of standard error.
    #[arg(long, value_name = "FILE")]
    pub log_file: Option<PathBuf>,
    /// Log more (repeat for more still); RUST_LOG overrides it.
    #[arg(short, long, action = clap::ArgAction::Count)]
    pub verbose: u8,
}

/// The SID of SYSTEM (LocalSystem), which services and WinFsp.Launcher run as.
pub const SYSTEM_SID: &str = "S-1-5-18";

/// Whether other users may read the mount: `--allow-other`, else the default for the account
/// (as on Linux, where root's mount lets other users read and a user's doesn't): yes for SYSTEM,
/// whose mount every session sees, no for a user.
pub fn allow_other(flag: bool, no_flag: bool, sid: &str) -> bool {
    !no_flag && (flag || sid == SYSTEM_SID)
}

/// What MOUNTPOINT names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MountPoint {
    /// A drive letter, upper case, with its colon (`X:`).
    Drive(String),
    /// A folder, which must not exist yet: WinFsp makes it.
    Folder(PathBuf),
}

impl MountPoint {
    /// `X:`, `x:`, `X:\` are drive letters; anything else is a folder.
    pub fn parse(s: &str) -> MountPoint {
        let b = s.as_bytes();
        let drive = (b.len() == 2 || (b.len() == 3 && (b[2] == b'\\' || b[2] == b'/')))
            && b[0].is_ascii_alphabetic()
            && b[1] == b':';
        if drive {
            MountPoint::Drive(format!("{}:", (b[0] as char).to_ascii_uppercase()))
        } else {
            MountPoint::Folder(PathBuf::from(s))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sacdfs::pcm::{Cutoff, PcmRate};
    use sacdfs::serve::{Formats, pcm_rate};

    fn parse(args: &[&str]) -> Result<Args, clap::Error> {
        Args::try_parse_from(std::iter::once("sacdfs-win").chain(args.iter().copied()))
    }

    #[test]
    fn defaults_are_the_linux_defaults() {
        let a = parse(&["C:\\music", "X:"]).unwrap();
        assert_eq!(a.options.format, Formats { dsf: true, wav: true });
        assert_eq!(a.options.pcm_rate, 176_400);
        assert_eq!(a.options.pcm_cutoff, Cutoff::Khz40);
        assert_eq!(a.options.pauses, "dropped");
        assert!(!a.options.flatten && !a.no_kernel_cache && !a.limits.no_index_cache);
        assert_eq!(a.limits.max_open_images, sacdfs::handles::DEFAULT_MAX_OPEN);
        assert_eq!(a.limits.request_timeout, sacdfs::watchdog::DEFAULT_TIMEOUT.as_secs());
    }

    #[test]
    fn options_parse_as_on_linux() {
        let a = parse(&[
            "C:\\music",
            "M:",
            "--format",
            "wav",
            "--pcm-rate",
            "352800",
            "--pcm-cutoff",
            "wide",
            "--pcm-gain",
            "-6",
            "--flatten",
            "--sidecars",
            "default,-webp",
            "--cache-mb",
            "64",
            "--tags",
            "C:\\tags",
            "--musicbrainz",
        ])
        .unwrap();
        assert_eq!(a.options.format, Formats { dsf: false, wav: true });
        assert_eq!(pcm_rate(a.options.pcm_rate), Some(PcmRate::R352800));
        assert_eq!(a.options.pcm_cutoff, Cutoff::Wide80);
        assert_eq!(a.options.pcm_gain, -6.0);
        assert!(a.options.flatten && a.options.musicbrainz);
        assert_eq!(a.options.cache_mb, Some(64));
    }

    #[test]
    fn bad_values_are_refused() {
        assert!(parse(&["s", "X:", "--pcm-rate", "44100"]).is_err());
        assert!(parse(&["s", "X:", "--format", "flac"]).is_err());
        assert!(parse(&["s", "X:", "--pcm-gain", "30"]).is_err());
        assert!(parse(&["s", "X:", "--musicbrainz"]).is_err(), "--musicbrainz needs --tags");
        assert!(parse(&["s", "X:", "--index-cache", "d", "--no-index-cache"]).is_err());
        assert!(parse(&["s", "X:", "--max-open-images", "0"]).is_err());
    }

    #[test]
    fn other_users_by_default_only_for_system() {
        assert!(allow_other(false, false, SYSTEM_SID));
        assert!(!allow_other(false, false, "S-1-5-21-1-2-3-1001"));
        assert!(allow_other(true, false, "S-1-5-21-1-2-3-1001"));
        assert!(!allow_other(false, true, SYSTEM_SID));
        assert!(parse(&["s", "X:", "--allow-other", "--no-allow-other"]).is_err());
    }

    #[test]
    fn mount_points() {
        assert_eq!(MountPoint::parse("x:"), MountPoint::Drive("X:".into()));
        assert_eq!(MountPoint::parse("Q:\\"), MountPoint::Drive("Q:".into()));
        assert_eq!(MountPoint::parse("C:\\mnt\\sacd"), MountPoint::Folder("C:\\mnt\\sacd".into()));
        assert_eq!(MountPoint::parse("1:"), MountPoint::Folder("1:".into()));
    }

    #[test]
    fn version_names_winfsp() {
        let e = parse(&["--version"]).unwrap_err();
        let text = e.to_string();
        assert!(
            text.contains("WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos"),
            "{text}"
        );
        assert!(text.contains("https://github.com/winfsp/winfsp"), "{text}");
    }
}
