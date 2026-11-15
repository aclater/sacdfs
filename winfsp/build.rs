// SPDX-License-Identifier: GPL-3.0-or-later

fn main() {
    // winfsp-x64.dll is loaded at the first call, from WinFsp's install folder.
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        winfsp::build::winfsp_link_delayload();
    }
}
