// SPDX-License-Identifier: GPL-3.0-or-later

//! `sacdfs-win`'s command line: the Linux options that mean the same on Windows, with the
//! same names and defaults, plus the few that only Windows has.

use std::ffi::OsString;
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
#[command(
    name = "sacdfs-win",
    version = crate::notice::VERSION,
    long_version = crate::notice::LONG_VERSION,
    about,
    after_help = sacdfs::serve::TUNING_HELP
)]
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
    /// (as a service or under WinFsp.Launcher). Needed to share the drive over SMB. `=no`: only
    /// the user who mounted (and SYSTEM and Administrators) may read the mount, also when run
    /// as SYSTEM.
    #[arg(long, value_name = "YES|NO", num_args = 0..=1, require_equals = true,
          default_missing_value = "yes", value_parser = sacdfs::serve::parse_yes_no())]
    pub allow_other: Option<bool>,
    /// The older spelling of --allow-other=no (#560).
    #[arg(long, hide = true, conflicts_with = "allow_other")]
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

impl Args {
    /// The older spellings on this command line (#560).
    pub fn renamed(&self) -> Vec<sacdfs::serve::Renamed> {
        let mut out = self.options.renamed();
        if self.no_allow_other {
            out.push(sacdfs::serve::no_allow_other_renamed());
        }
        out.extend(self.limits.renamed());
        out
    }
}

/// The program's arguments, with a launcher's quoting undone (#466): WinFsp.Launcher quotes
/// each argument it passes on, so a folder that ends in a backslash (`D:\Music\`, `E:\`)
/// arrives as `"D:\Music\" "M:"`, where `\"` reads as an escaped quote and the two
/// arguments run together into one. A Windows path never holds a `"`, so an argument with one
/// means this happened: the command line is then split again with backslashes taken literally.
/// The second value says so, for the log.
pub fn args() -> (Vec<OsString>, bool) {
    let args: Vec<OsString> = std::env::args_os().collect();
    if !quote_inside(&args) {
        return (args, false);
    }
    match raw_command_line() {
        Some(line) => {
            let mut again = split_literal(&line);
            // The program's own name as Windows gave it.
            if let (Some(first), Some(name)) = (again.first_mut(), args.first()) {
                first.clone_from(name);
            }
            (again, true)
        }
        None => (args, false),
    }
}

/// Whether an argument after the program's name holds a `"`.
pub fn quote_inside(args: &[OsString]) -> bool {
    args.iter().skip(1).any(|a| a.to_string_lossy().contains('"'))
}

/// A command line split at spaces and tabs outside quotes, each `"` starting or ending a quoted
/// part and every backslash kept as it is.
pub fn split_literal(line: &[u16]) -> Vec<OsString> {
    const QUOTE: u16 = b'"' as u16;
    const SPACE: u16 = b' ' as u16;
    const TAB: u16 = b'\t' as u16;
    let mut out = Vec::new();
    let mut arg: Vec<u16> = Vec::new();
    let (mut started, mut quoted) = (false, false);
    for &c in line {
        match c {
            QUOTE => (started, quoted) = (true, !quoted),
            SPACE | TAB if !quoted => {
                if started {
                    out.push(wide_to_os(&std::mem::take(&mut arg)));
                    started = false;
                }
            }
            _ => {
                arg.push(c);
                started = true;
            }
        }
    }
    if started {
        out.push(wide_to_os(&arg));
    }
    out
}

#[cfg(windows)]
fn wide_to_os(w: &[u16]) -> OsString {
    std::os::windows::ffi::OsStringExt::from_wide(w)
}

#[cfg(not(windows))]
fn wide_to_os(w: &[u16]) -> OsString {
    OsString::from(String::from_utf16_lossy(w))
}

/// The process's command line as Windows holds it.
#[cfg(windows)]
fn raw_command_line() -> Option<Vec<u16>> {
    use windows::Win32::System::Environment::GetCommandLineW;
    // SAFETY: GetCommandLineW returns the process's NUL-terminated command line, which lives as
    // long as the process; it is copied at once.
    unsafe {
        let line = GetCommandLineW();
        if line.is_null() {
            return None;
        }
        Some(line.as_wide().to_vec())
    }
}

/// Other systems pass arguments as they are: nothing to undo.
#[cfg(not(windows))]
fn raw_command_line() -> Option<Vec<u16>> {
    None
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
        assert!(a.options.flatten && a.options.musicbrainz());
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

    /// The older spellings (#560) still parse and are reported with their new forms.
    #[test]
    fn older_spellings_parse_and_warn() {
        let a = parse(&["s", "X:", "--no-allow-other", "--no-index-cache"]).unwrap();
        assert_eq!(sacdfs::serve::allow_other_flags(a.allow_other, a.no_allow_other), (false, true));
        assert_eq!(a.renamed().len(), 2);
        let b = parse(&["s", "X:", "--allow-other=no", "--index-cache", "off"]).unwrap();
        assert_eq!(sacdfs::serve::allow_other_flags(b.allow_other, b.no_allow_other), (false, true));
        assert!(b.renamed().is_empty());
    }

    #[test]
    fn mount_points() {
        assert_eq!(MountPoint::parse("x:"), MountPoint::Drive("X:".into()));
        assert_eq!(MountPoint::parse("Q:\\"), MountPoint::Drive("Q:".into()));
        assert_eq!(MountPoint::parse("C:\\mnt\\sacd"), MountPoint::Folder("C:\\mnt\\sacd".into()));
        assert_eq!(MountPoint::parse("1:"), MountPoint::Folder("1:".into()));
    }

    fn split(line: &str) -> Vec<String> {
        let w: Vec<u16> = line.encode_utf16().collect();
        split_literal(&w).into_iter().map(|a| a.into_string().unwrap()).collect()
    }

    fn os(args: &[&str]) -> Vec<OsString> {
        args.iter().map(OsString::from).collect()
    }

    /// The command lines WinFsp.Launcher makes from `%1 %2` and `%1 %2 --log-file FILE` (#466):
    /// a SOURCE or MOUNTPOINT ending in a backslash keeps it, and the arguments come apart.
    #[test]
    fn launcher_quoting_undone() {
        let exe = r#""C:\Program Files\sacdfs-win.exe" "#;
        for (tail, want) in [
            (r#""D:\Music\SACD\" "M:""#, vec![r"D:\Music\SACD\", "M:"]),
            (r#""E:\" "S:""#, vec![r"E:\", "S:"]),
            (r#""E:\" "S:\""#, vec![r"E:\", r"S:\"]),
            (r#""D:\My Music\" "M:""#, vec![r"D:\My Music\", "M:"]),
            (
                r#""D:\Music\" "M:" --log-file C:\ProgramData\sacdfs.log"#,
                vec![r"D:\Music\", "M:", "--log-file", r"C:\ProgramData\sacdfs.log"],
            ),
            (r#""\\nas\isos\" "N:""#, vec![r"\\nas\isos\", "N:"]),
            (r#""D:\Music" "M:""#, vec![r"D:\Music", "M:"]),
        ] {
            let got = split(&format!("{exe}{tail}"));
            assert_eq!(got[0], r"C:\Program Files\sacdfs-win.exe", "{tail}");
            assert_eq!(got[1..], want, "{tail}");
            let a = parse(&got[1..].iter().map(String::as_str).collect::<Vec<_>>()).unwrap();
            assert_eq!(a.source, PathBuf::from(want[0]), "{tail}");
            assert_eq!(a.mountpoint, want[1], "{tail}");
        }
        assert_eq!(split(" a\t\"\"  b "), ["a", "", "b"]);
    }

    /// What Windows makes of the launcher's line for `D:\Music\` (one argument with a quote
    /// in it) is spotted; ordinary arguments are left alone.
    #[test]
    fn quote_inside_an_argument_is_spotted() {
        assert!(quote_inside(&os(&["sacdfs-win", "D:\\Music\" M:"])));
        assert!(quote_inside(&os(&["sacdfs-win", "D:\\Music\" M: --log-file x"])));
        assert!(!quote_inside(&os(&["sacdfs-win", "D:\\Music\\", "M:"])));
        assert!(!quote_inside(&os(&["\"C:\\x\".exe", "E:\\.", "S:"])));
        // The arguments of a process whose command line has no quote inside one are its own.
        assert!(!args().1);
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
