// SPDX-License-Identifier: GPL-3.0-or-later

//! The NFS front end (#420) on the wire, without a kernel client: calls written by hand to a
//! server on 127.0.0.1, so that it runs on every platform, as any user.
//!
//! - File handles from one server still work on a new server over the same SOURCE (a
//!   restart), and read the same bytes; another SOURCE's are stale.
//! - Only the server's user (and root) are answered; others, and AUTH_NONE, are refused,
//!   but anyone may ping.
//! - FSSTAT reports the volume's size.
//! - A record longer than the bound ends the connection.

#![cfg(feature = "nfs")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use sacdfs::disc::Tuning;
use sacdfs::engine::Engine;
use sacdfs::nfs::proto::tcp::NFSTcpListener;
use sacdfs::nfs::{Callers, NfsFront, handle};
use sacdfs::tree::{Output, SourceDir, Tree};
use sacdfs::vfs::Volume;

const UID: u32 = 4321;
const NFS: u32 = 100_003;
const MOUNT: u32 = 100_005;

/// A server over `source` on 127.0.0.1, on a runtime of its own; dropped, it stops.
struct Server {
    addr: SocketAddr,
    volume: Arc<Volume>,
    _runtime: tokio::runtime::Runtime,
}

fn serve(source: &Path) -> Server {
    let source = source.canonicalize().unwrap();
    let tree =
        Arc::new(Tree::with_options(SourceDir::scan(&source).unwrap(), Tuning::default(), Output::default()));
    let engine = Arc::new(Engine::new(2, 16 << 20).unwrap());
    let volume = Arc::new(Volume::new(tree, engine, 4, UID, UID).unwrap());
    let front = NfsFront::new(volume.clone(), handle::volume_id(&source), Callers::User(UID));
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let listener = runtime.block_on(NFSTcpListener::bind("127.0.0.1:0".parse().unwrap(), front)).unwrap();
    let addr = listener.local_addr().unwrap();
    runtime.spawn(async move { listener.handle_forever().await });
    Server { addr, volume, _runtime: runtime }
}

/// XDR, as much as the calls need.
#[derive(Default)]
struct Xdr(Vec<u8>);

impl Xdr {
    fn u32(mut self, v: u32) -> Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn u64(mut self, v: u64) -> Self {
        self.0.extend_from_slice(&v.to_be_bytes());
        self
    }
    fn opaque(mut self, b: &[u8]) -> Self {
        self = self.u32(b.len() as u32);
        self.0.extend_from_slice(b);
        self.0.resize(self.0.len().next_multiple_of(4), 0);
        self
    }
}

/// Credentials: AUTH_SYS with a uid, or AUTH_NONE.
#[derive(Clone, Copy)]
enum Cred {
    Sys(u32),
    None,
}

/// A reply, read field by field.
struct Reply {
    b: Vec<u8>,
    at: usize,
}

impl Reply {
    fn u32(&mut self) -> u32 {
        let v = u32::from_be_bytes(self.b[self.at..self.at + 4].try_into().unwrap());
        self.at += 4;
        v
    }
    fn u64(&mut self) -> u64 {
        (u64::from(self.u32()) << 32) | u64::from(self.u32())
    }
    fn opaque(&mut self) -> Vec<u8> {
        let n = self.u32() as usize;
        let v = self.b[self.at..self.at + n].to_vec();
        self.at += n.next_multiple_of(4);
        v
    }
    fn skip(&mut self, n: usize) {
        self.at += n;
    }
    /// Skips a post_op_attr.
    fn post_op_attr(&mut self) {
        if self.u32() == 1 {
            self.skip(84);
        }
    }
}

struct Client {
    s: TcpStream,
    xid: u32,
}

/// How a call was answered.
#[derive(Debug, PartialEq, Eq)]
enum Answer {
    /// Accepted, with this accept_stat.
    Accepted(u32),
    /// Denied: AUTH_ERROR with this auth_stat.
    Denied(u32),
}

impl Client {
    fn new(addr: SocketAddr) -> Client {
        let s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
        Client { s, xid: 1 }
    }

    fn call(&mut self, cred: Cred, prog: u32, proc_: u32, args: Xdr) -> (Answer, Reply) {
        self.xid += 1;
        let vers = 3;
        let mut x = Xdr::default().u32(self.xid).u32(0).u32(2).u32(prog).u32(vers).u32(proc_);
        x = match cred {
            Cred::Sys(uid) => {
                let body = Xdr::default().u32(0).opaque(b"test").u32(uid).u32(uid).u32(0);
                x.u32(1).opaque(&body.0)
            }
            Cred::None => x.u32(0).opaque(&[]),
        };
        x = x.u32(0).opaque(&[]);
        x.0.extend_from_slice(&args.0);
        let mut rec = ((x.0.len() as u32) | 1 << 31).to_be_bytes().to_vec();
        rec.extend_from_slice(&x.0);
        self.s.write_all(&rec).unwrap();
        let mut r = Reply { b: self.record().expect("a reply"), at: 0 };
        assert_eq!(r.u32(), self.xid, "xid");
        assert_eq!(r.u32(), 1, "a reply");
        let answer = match r.u32() {
            0 => {
                r.u32();
                r.opaque();
                Answer::Accepted(r.u32())
            }
            _ => {
                assert_eq!(r.u32(), 1, "AUTH_ERROR");
                Answer::Denied(r.u32())
            }
        };
        (answer, r)
    }

    /// The next record, `None` once the server closed the connection.
    fn record(&mut self) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        loop {
            let mut h = [0u8; 4];
            if self.s.read_exact(&mut h).is_err() {
                return None;
            }
            let h = u32::from_be_bytes(h);
            let mut b = vec![0u8; (h & !(1 << 31)) as usize];
            self.s.read_exact(&mut b).ok()?;
            out.extend_from_slice(&b);
            if h & 1 << 31 != 0 {
                return Some(out);
            }
        }
    }

    /// MNT "/": the root's handle.
    fn mount(&mut self) -> Vec<u8> {
        let (a, mut r) = self.call(Cred::Sys(UID), MOUNT, 1, Xdr::default().opaque(b"/"));
        assert_eq!(a, Answer::Accepted(0));
        assert_eq!(r.u32(), 0, "MNT3_OK");
        r.opaque()
    }

    /// GETATTR: the status, and the size if NFS3_OK.
    fn getattr(&mut self, fh: &[u8]) -> (u32, u64) {
        let (a, mut r) = self.call(Cred::Sys(UID), NFS, 1, Xdr::default().opaque(fh));
        assert_eq!(a, Answer::Accepted(0));
        let st = r.u32();
        if st != 0 {
            return (st, 0);
        }
        r.skip(20);
        (st, r.u64())
    }

    fn lookup(&mut self, dir: &[u8], name: &str) -> Vec<u8> {
        let (a, mut r) =
            self.call(Cred::Sys(UID), NFS, 3, Xdr::default().opaque(dir).opaque(name.as_bytes()));
        assert_eq!(a, Answer::Accepted(0));
        assert_eq!(r.u32(), 0, "LOOKUP {name}");
        r.opaque()
    }

    /// READ: the status and the bytes.
    fn read(&mut self, fh: &[u8], offset: u64, count: u32) -> (u32, Vec<u8>) {
        let (a, mut r) = self.call(Cred::Sys(UID), NFS, 6, Xdr::default().opaque(fh).u64(offset).u32(count));
        assert_eq!(a, Answer::Accepted(0));
        let st = r.u32();
        r.post_op_attr();
        if st != 0 {
            return (st, Vec::new());
        }
        r.u32();
        r.u32();
        (st, r.opaque())
    }
}

/// The first track file of `image`'s stereo area, by walking from the root.
fn first_track(c: &mut Client, root: &[u8], image: &str) -> Vec<u8> {
    let dir = c.lookup(root, image.trim_end_matches(".iso"));
    let stereo = c.lookup(&dir, "Stereo");
    let (a, mut r) = c.call(Cred::Sys(UID), NFS, 16, Xdr::default().opaque(&stereo).u64(0).u64(0).u32(4096));
    assert_eq!(a, Answer::Accepted(0));
    assert_eq!(r.u32(), 0, "READDIR");
    r.post_op_attr();
    r.skip(8);
    assert_eq!(r.u32(), 1, "an entry");
    r.u64();
    let name = String::from_utf8(r.opaque()).unwrap();
    c.lookup(&stereo, &name)
}

const IMAGE: &str = "dsd-stereo-mch-2048.iso";

fn source() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::copy(common::corpus().join("images").join(IMAGE), dir.path().join(IMAGE)).unwrap();
    dir
}

#[test]
fn handles_survive_a_restart() {
    let src = source();
    let s = serve(src.path());
    let mut c = Client::new(s.addr);
    let root = c.mount();
    assert_eq!(root.len(), handle::LEN);
    let file = first_track(&mut c, &root, IMAGE);
    let (st, size) = c.getattr(&file);
    assert_eq!(st, 0);
    let (st, before) = c.read(&file, 4096, 65536);
    assert_eq!(st, 0);
    assert_eq!(before.len() as u64, 65536.min(size - 4096));
    drop(c);
    drop(s);

    // A new server over the same SOURCE: the old handles, without a lookup first.
    let s = serve(src.path());
    let mut c = Client::new(s.addr);
    assert_eq!(c.getattr(&root).0, 0, "the root's handle after a restart");
    assert_eq!(c.getattr(&file), (0, size), "the file's handle after a restart");
    assert_eq!(c.read(&file, 4096, 65536), (0, before), "the same bytes after a restart");
    drop(c);
    drop(s);

    // Another SOURCE: stale (NFS3ERR_STALE, 70).
    let other = tempfile::tempdir().unwrap();
    std::fs::copy(common::corpus().join("images").join(IMAGE), other.path().join(IMAGE)).unwrap();
    let s = serve(other.path());
    let mut c = Client::new(s.addr);
    assert_eq!(c.getattr(&file).0, 70);
    // Not a handle at all (NFS3ERR_BADHANDLE, 10001).
    assert_eq!(c.getattr(&[0u8; 16]).0, 10001);
}

#[test]
fn a_changed_image_makes_its_files_stale() {
    let src = source();
    let s = serve(src.path());
    let mut c = Client::new(s.addr);
    let root = c.mount();
    let file = first_track(&mut c, &root, IMAGE);
    drop(c);
    drop(s);
    // The image's modification time changes its files' generation (#18).
    let f = std::fs::File::options().write(true).open(src.path().join(IMAGE)).unwrap();
    f.set_modified(std::time::SystemTime::now() + Duration::from_secs(3600)).unwrap();
    drop(f);
    let s = serve(src.path());
    let mut c = Client::new(s.addr);
    assert_eq!(c.getattr(&root).0, 0, "a folder's handle stays");
    assert_eq!(c.getattr(&file).0, 70, "a changed image's file is stale");
}

#[test]
fn only_the_user_is_answered() {
    let src = source();
    let s = serve(src.path());
    let mut c = Client::new(s.addr);
    let root = c.mount();
    // AUTH_BADCRED (1) for another uid, AUTH_TOOWEAK (5) for AUTH_NONE.
    assert_eq!(c.call(Cred::Sys(UID + 1), NFS, 1, Xdr::default().opaque(&root)).0, Answer::Denied(1));
    assert_eq!(c.call(Cred::None, NFS, 1, Xdr::default().opaque(&root)).0, Answer::Denied(5));
    assert_eq!(c.call(Cred::Sys(UID + 1), MOUNT, 1, Xdr::default().opaque(b"/")).0, Answer::Denied(1));
    // Root, and anyone's ping.
    assert_eq!(c.call(Cred::Sys(0), NFS, 1, Xdr::default().opaque(&root)).0, Answer::Accepted(0));
    assert_eq!(c.call(Cred::None, NFS, 0, Xdr::default()).0, Answer::Accepted(0));
}

#[test]
fn fsstat_is_the_volume_size() {
    let src = source();
    let s = serve(src.path());
    let mut c = Client::new(s.addr);
    let root = c.mount();
    let (a, mut r) = c.call(Cred::Sys(UID), NFS, 18, Xdr::default().opaque(&root));
    assert_eq!(a, Answer::Accepted(0));
    assert_eq!(r.u32(), 0);
    r.post_op_attr();
    let (tbytes, fbytes, abytes, tfiles) = (r.u64(), r.u64(), r.u64(), r.u64());
    let st = s.volume.statfs();
    assert_eq!(tbytes, st.blocks * u64::from(st.frsize));
    assert!(tbytes > 0 && tbytes < 1 << 40);
    assert_eq!((fbytes, abytes), (0, 0));
    assert_eq!(tfiles, st.files);
}

#[test]
fn an_overlong_record_ends_the_connection() {
    let src = source();
    let s = serve(src.path());
    let mut c = Client::new(s.addr);
    let len = sacdfs::nfs::proto::rpcwire::MAX_RECORD as u32 + 1;
    c.s.write_all(&(len | 1 << 31).to_be_bytes()).unwrap();
    assert_eq!(c.record(), None);
    // The server still serves others.
    let mut c = Client::new(s.addr);
    c.mount();
}
