// SPDX-License-Identifier: GPL-3.0-or-later

//! A stalling image for the soak (#422): mounts a FUSE filesystem at MOUNTPOINT holding one
//! regular file, `disc.iso`, that reads as IMAGE. Commands on standard input, one a line:
//!
//! - `stall`: from now on, opens and reads of the file are left unanswered, as on a hung share;
//! - `release`: answers the held ones (with IMAGE's bytes) and serves normally again;
//! - `quit`, or the end of standard input: releases and unmounts.
//!
//! Standard output has `ready` once mounted, then a line per command: `ok`, or for `release`
//! `ok OPENS READS`, how many opens and reads were held (0 and 0: the stall reached nothing).
//! Reads bypass the page cache (`FOPEN_DIRECT_IO`), so that a stalled image is stalled for
//! sacdfs whatever it read before. *Why not a FIFO:* since 0.6.4 sacdfs refuses an image that
//! is not a regular file, before any read; tests/stall.rs stalls the same way.
//!
//! ```text
//! cargo build --release --example stallfs
//! target/release/examples/stallfs IMAGE MOUNTPOINT
//! ```
//!
//! Linux only, like the FUSE front end.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::print_stdout, clippy::print_stderr)]

#[cfg(target_os = "linux")]
fn main() {
    linux::main();
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("stallfs: Linux only");
    std::process::exit(2);
}

#[cfg(target_os = "linux")]
mod linux {
    use std::ffi::OsStr;
    use std::fs::File;
    use std::io::{BufRead, Write};
    use std::os::unix::fs::{FileExt, MetadataExt};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, SystemTime};

    use fuser::{
        Config, Errno, FileAttr, FileHandle, FileType, Filesystem, FopenFlags, Generation, INodeNo,
        LockOwner, MountOption, OpenFlags, ReplyAttr, ReplyData, ReplyEntry, ReplyOpen, Request, Session,
    };

    const FILE_INO: INodeNo = INodeNo(2);
    const NAME: &str = "disc.iso";

    /// What is held while stalled.
    #[derive(Default)]
    struct Held {
        stalled: bool,
        opens: Vec<ReplyOpen>,
        reads: Vec<(u64, u32, ReplyData)>,
    }

    struct StallFs {
        image: File,
        size: u64,
        mtime: SystemTime,
        held: Arc<Mutex<Held>>,
    }

    impl StallFs {
        fn attr(&self, ino: INodeNo) -> FileAttr {
            let dir = ino == INodeNo::ROOT;
            FileAttr {
                ino,
                size: if dir { 0 } else { self.size },
                blocks: if dir { 0 } else { self.size.div_ceil(512) },
                atime: self.mtime,
                mtime: self.mtime,
                ctime: self.mtime,
                crtime: self.mtime,
                kind: if dir { FileType::Directory } else { FileType::RegularFile },
                perm: if dir { 0o555 } else { 0o444 },
                nlink: if dir { 2 } else { 1 },
                uid: 0,
                gid: 0,
                rdev: 0,
                blksize: 4096,
                flags: 0,
            }
        }
    }

    /// `size` bytes of `image` at `offset`, short at its end.
    fn read(image: &File, offset: u64, size: u32) -> std::io::Result<Vec<u8>> {
        let mut buf = vec![0u8; size as usize];
        let mut got = 0;
        while got < buf.len() {
            match image.read_at(&mut buf[got..], offset + got as u64)? {
                0 => break,
                n => got += n,
            }
        }
        buf.truncate(got);
        Ok(buf)
    }

    fn answer(image: &File, offset: u64, size: u32, reply: ReplyData) {
        match read(image, offset, size) {
            Ok(b) => reply.data(&b),
            Err(_) => reply.error(Errno::EIO),
        }
    }

    impl Filesystem for StallFs {
        fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
            if parent == INodeNo::ROOT && name == NAME {
                reply.entry(&Duration::ZERO, &self.attr(FILE_INO), Generation(0));
            } else {
                reply.error(Errno::ENOENT);
            }
        }

        fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
            reply.attr(&Duration::ZERO, &self.attr(ino));
        }

        fn open(&self, _req: &Request, _ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
            let mut h = self.held.lock().unwrap();
            if h.stalled {
                h.opens.push(reply);
            } else {
                reply.opened(FileHandle(0), FopenFlags::FOPEN_DIRECT_IO);
            }
        }

        fn read(
            &self,
            _req: &Request,
            _ino: INodeNo,
            _fh: FileHandle,
            offset: u64,
            size: u32,
            _flags: OpenFlags,
            _lock_owner: Option<LockOwner>,
            reply: ReplyData,
        ) {
            let mut h = self.held.lock().unwrap();
            if h.stalled {
                h.reads.push((offset, size, reply));
            } else {
                drop(h);
                answer(&self.image, offset, size, reply);
            }
        }
    }

    /// Ends the stall: the opens and reads held, answered now.
    fn release(held: &Mutex<Held>, image: &File) -> (usize, usize) {
        let (opens, reads) = {
            let mut h = held.lock().unwrap();
            h.stalled = false;
            (std::mem::take(&mut h.opens), std::mem::take(&mut h.reads))
        };
        let counts = (opens.len(), reads.len());
        for o in opens {
            o.opened(FileHandle(0), FopenFlags::FOPEN_DIRECT_IO);
        }
        for (offset, size, r) in reads {
            answer(image, offset, size, r);
        }
        counts
    }

    pub fn main() {
        let args: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
        let [image, mountpoint] = args.as_slice() else {
            eprintln!("usage: stallfs IMAGE MOUNTPOINT");
            std::process::exit(2);
        };
        let file = File::open(image).expect("IMAGE");
        let meta = file.metadata().expect("IMAGE");
        let held = Arc::new(Mutex::new(Held::default()));
        let fs = StallFs {
            image: file.try_clone().expect("IMAGE"),
            size: meta.size(),
            mtime: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            held: held.clone(),
        };
        let mut config = Config::default();
        config.mount_options = vec![MountOption::RO, MountOption::FSName("sacdfs-stallfs".to_owned())];
        let session = Session::new(fs, mountpoint, &config).expect("mount").spawn().expect("mount");
        let mut out = std::io::stdout();
        writeln!(out, "ready").and_then(|()| out.flush()).ok();
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            let ack = match line.trim() {
                "stall" => {
                    held.lock().unwrap().stalled = true;
                    "ok".to_owned()
                }
                "release" => {
                    let (opens, reads) = release(&held, &file);
                    format!("ok {opens} {reads}")
                }
                "quit" => break,
                other => {
                    eprintln!("stallfs: unknown command {other:?}");
                    continue;
                }
            };
            writeln!(out, "{ack}").and_then(|()| out.flush()).ok();
        }
        release(&held, &file);
        drop(session);
    }
}
