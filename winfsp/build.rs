// SPDX-License-Identifier: GPL-3.0-or-later

fn main() {
    // sacdfs's version, from the core's manifest, so that the two never differ.
    println!("cargo:rerun-if-changed=../Cargo.toml");
    let toml = std::fs::read_to_string("../Cargo.toml").unwrap_or_default();
    let version =
        toml.lines().find_map(|l| l.strip_prefix("version = \"")?.strip_suffix('"')).unwrap_or("unknown");
    println!("cargo:rustc-env=SACDFS_VERSION={version}");
    // winfsp-x64.dll is loaded at its first use, from WinFsp's install folder (see
    // `winfs::load_winfsp`), so that a missing WinFsp is a plain message, not a loader error.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winfsp::build::winfsp_link_delayload();
    }
}
