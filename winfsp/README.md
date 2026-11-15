# sacdfs-win: the Windows front end

`sacdfs-win` mounts SOURCE read-only as a drive letter or folder on Windows, through
[WinFsp](https://github.com/winfsp/winfsp), over the same core (`sacdfs::vfs::Volume`) as the
Linux FUSE front end. Users start at [docs/install/windows.md](../docs/install/windows.md);
this page is for building and changing it.

WinFsp - Windows File System Proxy, Copyright (C) Bill Zissimopoulos,
<https://github.com/winfsp/winfsp>.

## Licence

The front end's own code is GPL-3.0-or-later, as sacdfs. It links the
[winfsp](https://crates.io/crates/winfsp) crate, which is GPL-3.0-only, so the Windows binary as
a whole is under GPL-3.0. It loads WinFsp's DLL, which WinFsp's licence allows for free
software under its FLOSS exception, on the condition that the program shows the notice above in
its user interface (`sacdfs-win --version`) and its documentation.

## Layout

A crate of its own, with its own lock file, so that sacdfs's Linux build and its crates.io
package are unchanged.

| File | What it does |
|---|---|
| `src/options.rs` | The command line: the core's shared options (`sacdfs::serve`) plus `--no-kernel-cache`, `--log-file` and `-v`. |
| `src/names.rs` | Windows names for the core's names: the SFM private-use mapping, bare reserved names, the 255 UTF-16 unit limit, case folding. |
| `src/paths.rs` | Paths to inodes: each folder's listing kept with its Windows names, lookups in any case, re-listing of folders the core re-reads and the changes found. |
| `src/winfs.rs` | The WinFsp file system: open, read, listings, security, change notification. |
| `src/run.rs` | Start and stop: the core in `sacdfs::serve`'s steps, the WinFsp volume, and WinFsp's service framework (console, Windows service or WinFsp.Launcher). |
| `ci/checks.ps1` | The mount checks the `windows` workflow runs; see its header. |

The modules that need no Windows API (`options`, `names`, `paths`, `notice`) build and test on
Linux too.

## Build

On Windows, with Rust (MSVC) and LLVM for bindgen (`LIBCLANG_PATH`), as the `windows` workflow
does:

```
cd winfsp
cargo build --release --locked
```

From Linux, with [cargo-xwin](https://github.com/rust-cross/cargo-xwin) (it downloads the MSVC
CRT and Windows SDK, under Microsoft's licence), clang, libclang, and `llvm-lib` (a link to the
`llvm-ar` of the `llvm-tools` rustup component works):

```
rustup target add x86_64-pc-windows-msvc
cd winfsp
cargo xwin build --release --target x86_64-pc-windows-msvc
```

The tests of the portable modules run anywhere: `cargo test` in `winfsp/`.

## Design notes

- **One copy per read.** A read's reply copies the core's bytes straight into WinFsp's buffer
  while the WinFsp thread waits (`winfs::Fill`).
- **Opens.** A file opened to read its data opens the core's handle at once, so that an image
  that can't be read fails the open, as on Linux; an open for attributes only (most of
  Explorer's) never asks the core.
- **Windows's cache.** By default WinFsp's `FileInfoTimeout`, `DirInfoTimeout` and
  `SecurityTimeout` are infinite: Windows keeps file contents (the cache manager), attributes,
  listings and security until sacdfs-win says something changed. Folders the core re-reads when
  SOURCE changes are listed again every second while they are in use, and each change is sent
  as a change notification (`FspFileSystemNotify`), which also drops what Windows kept for that
  name. `--no-kernel-cache` turns the cache off.
- **Image files** left idle are closed after 10 seconds, not the core's 5 minutes, because
  Windows refuses to rename or delete a folder while a file inside it is open.
- **Stopping.** WinFsp's service framework handles Ctrl+C, Ctrl+Break, a console close, a
  Windows service stop and WinFsp.Launcher's stop alike: it calls the stop callback, which stops
  the dispatcher, removes the mount point and saves the seek index.
