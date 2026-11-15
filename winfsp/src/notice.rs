// SPDX-License-Identifier: GPL-3.0-or-later

//! What `--version` says: sacdfs's version, and the notice WinFsp's FLOSS exception asks every
//! program that uses WinFsp to show in its user interface.

/// sacdfs's version (the core's, read from its Cargo.toml by build.rs).
pub const VERSION: &str = env!("SACDFS_VERSION");

/// `--version` and `-V`.
pub const LONG_VERSION: &str = concat!(
    env!("SACDFS_VERSION"),
    "\nLicence: GPL-3.0-or-later; this program with the winfsp crate: GPL-3.0.",
    "\nUses WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos",
    "\nhttps://github.com/winfsp/winfsp"
);

#[cfg(test)]
mod tests {
    #[test]
    fn version_is_the_cores() {
        let toml = include_str!("../../Cargo.toml");
        let line = toml.lines().find(|l| l.starts_with("version = ")).unwrap_or_default();
        assert_eq!(line, format!("version = \"{}\"", super::VERSION));
    }

    /// The notice WinFsp's licence exception asks for, word for word.
    #[test]
    fn notice_is_in_the_long_version() {
        for line in [
            "WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos",
            "https://github.com/winfsp/winfsp",
        ] {
            assert!(super::LONG_VERSION.contains(line), "{line}");
        }
    }
}
