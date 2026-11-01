// SPDX-License-Identifier: GPL-3.0-or-later
//! `-o` mount options, for `mount -t fuse.sacdfs` and fstab (#60).
//!
//! `mount.fuse3` runs `sacdfs SOURCE MOUNTPOINT -o OPTIONS`. The options are turned into the
//! equivalent command-line options before clap sees them, so there is one place that knows
//! what each option does. Options that only mean something to `mount(8)` or the kernel are
//! accepted and ignored: sacdfs is always read-only, and systemd reads `x-systemd.*` itself.
//! Anything else is an error, so a typo does not silently do nothing.

use std::ffi::{OsStr, OsString};

/// Options that mount(8), fstab or libfuse understand and sacdfs has no use for.
const IGNORED: &[&str] = &[
    "ro",
    "rw",
    "defaults",
    "auto",
    "noauto",
    "nofail",
    "_netdev",
    "user",
    "users",
    "nouser",
    "owner",
    "group",
    "suid",
    "nosuid",
    "dev",
    "nodev",
    "exec",
    "noexec",
    "atime",
    "noatime",
    "relatime",
    "norelatime",
    "diratime",
    "nodiratime",
    "sync",
    "async",
    "dirsync",
    "default_permissions",
    "nonempty",
    "noforget",
    "x-gvfs-show",
    "x-gvfs-hide",
];
/// Prefixes that are ignored too: `x-systemd.automount`, `x-mount.mkdir`, `fsname=…`.
const IGNORED_PREFIXES: &[&str] = &["x-", "fsname=", "subtype=", "uid=", "gid=", "max_read=", "context="];
/// sacdfs options that take no value, as the option name without the dashes.
const FLAGS: &[&str] = &["foreground", "flatten", "allow_other", "no_index_cache"];
/// sacdfs options that take a value (`key=value`).
const VALUES: &[&str] = &[
    "threads",
    "cache_mb",
    "format",
    "pcm_rate",
    "pcm_cutoff",
    "pcm_gain",
    "sidecars",
    "index_cache",
    "max_open_images",
    "log_file",
];

/// The command line with every `-o OPTIONS` (also `-oOPTIONS` and `--options=...`) replaced
/// by the options it stands for. `args[0]` is the program name.
///
/// A comma separates `-o` options, so a list value uses `+` instead (`format=dsf+wav`,
/// `sidecars=default+cue`).
pub fn expand(args: Vec<OsString>) -> Result<Vec<OsString>, String> {
    let mut out = Vec::with_capacity(args.len());
    let mut it = args.into_iter();
    out.extend(it.next());
    while let Some(a) = it.next() {
        let attached =
            a.to_str().and_then(|s| s.strip_prefix("-o")).filter(|r| !r.is_empty() && !r.starts_with('-'));
        let list = if a == "-o" {
            Some(it.next().ok_or("-o needs a list of options")?)
        } else {
            attached.map(OsString::from)
        };
        match list {
            Some(l) => option_list(&l, &mut out)?,
            None => out.push(a),
        }
    }
    Ok(out)
}

fn option_list(list: &OsStr, out: &mut Vec<OsString>) -> Result<(), String> {
    let list = list.to_str().ok_or("-o: the options are not valid UTF-8")?;
    for item in list.split(',').filter(|i| !i.is_empty()) {
        let (key, value) = match item.split_once('=') {
            Some((k, v)) => (k, Some(v)),
            None => (item, None),
        };
        let long = || format!("--{}", key.replace('_', "-"));
        match value {
            None if IGNORED.contains(&key) => {}
            _ if IGNORED_PREFIXES.iter().any(|p| item.starts_with(p)) => {}
            None if FLAGS.contains(&key) => out.push(long().into()),
            None if key == "verbose" => out.push("-v".into()),
            Some(v) if key == "verbose" && v.parse::<u8>().is_ok() => {
                out.push(format!("-{}", "v".repeat(usize::from(v.parse::<u8>().unwrap_or(0)))).into());
            }
            Some(v) if VALUES.contains(&key) => {
                out.push(long().into());
                out.push(v.replace('+', ",").into());
            }
            _ => return Err(format!("-o {item}: not a sacdfs mount option")),
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn run(a: &[&str]) -> Result<Vec<String>, String> {
        let v = a.iter().map(OsString::from).collect();
        expand(v).map(|v| v.into_iter().map(|s| s.into_string().unwrap()).collect())
    }

    #[test]
    fn plain_command_lines_pass_through() {
        assert_eq!(run(&["sacdfs", "-f", "/s", "/m"]).unwrap(), ["sacdfs", "-f", "/s", "/m"]);
    }

    #[test]
    fn mount_fuse_style_line() {
        let got = run(&[
            "sacdfs", "/srv/sacd", "/mnt/sacd", "-o",
            "rw,nosuid,nodev,_netdev,nofail,noauto,x-systemd.automount,allow_other,cache_mb=16,format=dsf+wav,foreground",
        ])
        .unwrap();
        assert_eq!(
            got,
            [
                "sacdfs",
                "/srv/sacd",
                "/mnt/sacd",
                "--allow-other",
                "--cache-mb",
                "16",
                "--format",
                "dsf,wav",
                "--foreground"
            ]
        );
    }

    #[test]
    fn attached_and_verbose_forms() {
        assert_eq!(run(&["s", "-oro,verbose=2"]).unwrap(), ["s", "-vv"]);
        assert_eq!(
            run(&["s", "-o", "log_file=/var/log/x,flatten"]).unwrap(),
            ["s", "--log-file", "/var/log/x", "--flatten"]
        );
    }

    #[test]
    fn unknown_options_are_refused() {
        assert!(run(&["s", "-o", "cache_mbb=1"]).unwrap_err().contains("cache_mbb=1"));
        assert!(run(&["s", "-o", "bogus"]).is_err());
        assert!(run(&["s", "-o"]).is_err());
        assert!(run(&["s", "-o", "threads"]).is_err());
    }
}
