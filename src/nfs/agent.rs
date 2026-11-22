// SPDX-License-Identifier: GPL-3.0-or-later

//! `sacdfs-nfs --launch-agent ...` (#420): a launchd agent that runs the same command at each
//! login on macOS, so that the mount comes back without a terminal.
//!
//! The agent runs the command in the folder it was written from, so relative paths in it
//! mean what they meant there. launchd starts it again if it fails (at most every 30 s), not
//! after a clean stop (`launchctl unload`, which sends SIGTERM: the server unmounts first).

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};

/// The agent's label, and its file's name without `.plist`.
pub const LABEL: &str = "io.github.aclater.sacdfs-nfs";

/// Writes the agent for `argv` (as `sacdfs-nfs` was started, `--launch-agent` included) to
/// ~/Library/LaunchAgents, and prints how to start and stop it.
pub fn write(argv: &[OsString]) -> io::Result<()> {
    let home =
        std::env::var_os("HOME").map(PathBuf::from).ok_or_else(|| io::Error::other("HOME is not set"))?;
    let exe = std::env::current_exe()?.canonicalize()?;
    let cwd = std::env::current_dir()?;
    // A property list is text: an argument that is not UTF-8 is written with U+FFFD in place
    // of the bytes that are not, and so names another path.
    let args: Vec<String> = std::iter::once(exe.to_string_lossy().into_owned())
        .chain(
            argv.iter().skip(1).filter(|a| *a != "--launch-agent").map(|a| a.to_string_lossy().into_owned()),
        )
        .collect();
    let log = home.join("Library/Logs/sacdfs-nfs.log");
    let dir = home.join("Library/LaunchAgents");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{LABEL}.plist"));
    std::fs::write(&path, plist(&args, &cwd, &log))?;
    let p = path.display();
    println!("wrote {p}");
    println!("It runs at each login. To start it now:   launchctl load -w {p}");
    println!("To stop it and keep it from starting:     launchctl unload -w {p}");
    println!("Its log: {}", log.display());
    Ok(())
}

fn escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The agent's property list: `args` run in `cwd`, logging to `log`.
pub fn plist(args: &[String], cwd: &Path, log: &Path) -> String {
    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n",
    );
    out += &format!("  <key>Label</key>\n  <string>{LABEL}</string>\n");
    out += "  <key>ProgramArguments</key>\n  <array>\n";
    for a in args {
        out += &format!("    <string>{}</string>\n", escape(a));
    }
    out += "  </array>\n";
    out += &format!("  <key>WorkingDirectory</key>\n  <string>{}</string>\n", escape(&cwd.to_string_lossy()));
    out += "  <key>RunAtLoad</key>\n  <true/>\n";
    // Again after a failure (a SOURCE not there yet at login), not after a clean stop.
    out += "  <key>KeepAlive</key>\n  <dict>\n    <key>SuccessfulExit</key>\n    <false/>\n  </dict>\n";
    out += "  <key>ThrottleInterval</key>\n  <integer>30</integer>\n";
    let log = escape(&log.to_string_lossy());
    out += &format!("  <key>StandardOutPath</key>\n  <string>{log}</string>\n");
    out += &format!("  <key>StandardErrorPath</key>\n  <string>{log}</string>\n");
    out += "</dict>\n</plist>\n";
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_plist_holds_the_command() {
        let args =
            ["/usr/local/bin/sacdfs-nfs", "/Volumes/Music/SACD", "/Users/a/SACD & more"].map(String::from);
        let p = plist(&args, Path::new("/Users/a"), Path::new("/Users/a/Library/Logs/sacdfs-nfs.log"));
        assert!(p.contains("<string>io.github.aclater.sacdfs-nfs</string>"));
        assert!(p.contains("<string>/Volumes/Music/SACD</string>"));
        assert!(p.contains("<string>/Users/a/SACD &amp; more</string>"));
        assert!(p.contains("<key>WorkingDirectory</key>\n  <string>/Users/a</string>"));
        assert!(p.ends_with("</plist>\n"));
    }
}
