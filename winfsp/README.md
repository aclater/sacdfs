# sacdfs-win: Windows front end on WinFsp (spike, #419)

**A spike, not for release.** It mounts SOURCE read-only as a drive letter or folder on
Windows, through [WinFsp](https://github.com/winfsp/winfsp), over the same core
(`sacdfs::vfs::Volume`) as the Linux FUSE front end. Findings and the plan are in #419.

WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos,
<https://github.com/winfsp/winfsp>.

## Build

A crate of its own (its own lock file), so that sacdfs's Linux build and its crates.io package
are unchanged.

On Windows, with Rust (MSVC) and LLVM for bindgen (`LIBCLANG_PATH`), as the `windows-spike`
workflow does (checked only in that workflow):

```
cd winfsp
cargo build --release
```

From Linux, with [cargo-xwin](https://github.com/rust-cross/cargo-xwin) (it downloads the MSVC
CRT and Windows SDK, under Microsoft's licence), clang, and `llvm-lib` (a link to the `llvm-ar`
of the `llvm-tools` rustup component works):

```
rustup target add x86_64-pc-windows-msvc
cd winfsp
cargo xwin build --release --target x86_64-pc-windows-msvc
```

## Run

Install WinFsp 2.1 (its MSI), then:

```
sacdfs-win SOURCE X: [--threads N] [--cache-mb N] [--kernel-cache] [--pcm-rate HZ]
```

It shows DSF and WAV files as `sacdfs` does by default and runs until the process ends, which
removes the drive. A drive letter mounted from an elevated prompt is seen only by elevated
programs: mount from a normal prompt for Explorer (found in the #22 feasibility spike; not
rechecked with this front end).

`ci/compare.py` checks a mount against a Linux mount's `sha256sum` list, mapping names as the
front end does.
