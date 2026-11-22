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
//! - Other clients are answered while one reads its replies slowly.
//! - While a new connection waits for a place, an idle one is closed to make room; while none
//!   waits, idle connections stay open.
//! - A folder is listed page by page from the cookie of the last entry read, by READDIR and
//!   READDIRPLUS; a cookie that is no entry's is refused; `.` and `..` are looked up.

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
use sacdfs::nfs::proto::tcp::{Limits, NFSTcpListener};
use sacdfs::nfs::{Callers, NfsFront, handle};
use sacdfs::tree::{Output, SourceDir, Tree};
use sacdfs::vfs::Volume;

const UID: u32 = 4321;

/// RPC program numbers.
const NFS: u32 = 100_003;
const MOUNT: u32 = 100_005;

/// The procedures called, by their numbers in RFC 1813 (NFS) and its appendix I (MOUNT).
const NULL: u32 = 0;
const GETATTR: u32 = 1;
const LOOKUP: u32 = 3;
const READ: u32 = 6;
const READDIR: u32 = 16;
const READDIRPLUS: u32 = 17;
const FSSTAT: u32 = 18;
const MNT: u32 = 1;

/// nfsstat3 values.
const NFS3_OK: u32 = 0;
const NFS3ERR_STALE: u32 = 70;
const NFS3ERR_BADHANDLE: u32 = 10001;
const NFS3ERR_BAD_COOKIE: u32 = 10003;

/// auth_stat values.
const AUTH_BADCRED: u32 = 1;
const AUTH_TOOWEAK: u32 = 5;

/// A fattr3 on the wire, in bytes, and where its `size` starts.
const FATTR3_LEN: usize = 84;
const FATTR3_SIZE_AT: usize = 20;

/// A server over `source` on 127.0.0.1, on a runtime of its own; dropped, it stops.
struct Server {
    addr: SocketAddr,
    volume: Arc<Volume>,
    _runtime: tokio::runtime::Runtime,
}

fn serve(source: &Path) -> Server {
    serve_with(source, Limits::default())
}

/// [`serve`] with `limits`.
fn serve_with(source: &Path, limits: Limits) -> Server {
    serve_engine(source, limits, Engine::new(2, 16 << 20).unwrap())
}

/// [`serve`] with `limits` and `engine`.
fn serve_engine(source: &Path, limits: Limits, engine: Engine) -> Server {
    let source = source.canonicalize().unwrap();
    let tree =
        Arc::new(Tree::with_options(SourceDir::scan(&source).unwrap(), Tuning::default(), Output::default()));
    let engine = Arc::new(engine);
    let volume = Arc::new(Volume::new(tree, engine, 4, UID, UID).unwrap());
    let front = NfsFront::new(volume.clone(), handle::volume_id(&source), Callers::User(UID));
    let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().unwrap();
    let listener = runtime
        .block_on(NFSTcpListener::bind("127.0.0.1:0".parse().unwrap(), front))
        .unwrap()
        .with_limits(limits);
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
            self.skip(FATTR3_LEN);
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
        let (a, mut r) = self.call(Cred::Sys(UID), MOUNT, MNT, Xdr::default().opaque(b"/"));
        assert_eq!(a, Answer::Accepted(0));
        assert_eq!(r.u32(), 0, "MNT3_OK");
        r.opaque()
    }

    /// GETATTR: the status, and the size if NFS3_OK.
    fn getattr(&mut self, fh: &[u8]) -> (u32, u64) {
        let (a, mut r) = self.call(Cred::Sys(UID), NFS, GETATTR, Xdr::default().opaque(fh));
        assert_eq!(a, Answer::Accepted(0));
        let st = r.u32();
        if st != NFS3_OK {
            return (st, 0);
        }
        r.skip(FATTR3_SIZE_AT);
        (st, r.u64())
    }

    fn lookup(&mut self, dir: &[u8], name: &str) -> Vec<u8> {
        let (a, mut r) =
            self.call(Cred::Sys(UID), NFS, LOOKUP, Xdr::default().opaque(dir).opaque(name.as_bytes()));
        assert_eq!(a, Answer::Accepted(0));
        assert_eq!(r.u32(), NFS3_OK, "LOOKUP {name}");
        r.opaque()
    }

    /// READDIR of `dir` after the entry whose cookie is `cookie` (0: from the start), for a
    /// reply of about `count` bytes: the status, each entry's name and cookie, and `eof`.
    fn readdir(&mut self, dir: &[u8], cookie: u64, count: u32) -> (u32, Vec<(String, u64)>, bool) {
        let args = Xdr::default().opaque(dir).u64(cookie).u64(0).u32(count);
        let (a, mut r) = self.call(Cred::Sys(UID), NFS, READDIR, args);
        assert_eq!(a, Answer::Accepted(0));
        let st = r.u32();
        r.post_op_attr();
        if st != NFS3_OK {
            return (st, Vec::new(), false);
        }
        r.skip(8); // the cookie verifier
        let mut entries = Vec::new();
        while r.u32() == 1 {
            r.u64(); // the file ID
            let name = String::from_utf8(r.opaque()).unwrap();
            entries.push((name, r.u64()));
        }
        (st, entries, r.u32() == 1)
    }

    /// READDIRPLUS, as [`Client::readdir`], for a reply of about `maxcount` bytes; each entry
    /// also has its attributes and its handle, which are checked to be there.
    fn readdirplus(&mut self, dir: &[u8], cookie: u64, maxcount: u32) -> (u32, Vec<(String, u64)>, bool) {
        let args = Xdr::default().opaque(dir).u64(cookie).u64(0).u32(4096).u32(maxcount);
        let (a, mut r) = self.call(Cred::Sys(UID), NFS, READDIRPLUS, args);
        assert_eq!(a, Answer::Accepted(0));
        let st = r.u32();
        r.post_op_attr();
        if st != NFS3_OK {
            return (st, Vec::new(), false);
        }
        r.skip(8); // the cookie verifier
        let mut entries = Vec::new();
        while r.u32() == 1 {
            r.u64(); // the file ID
            let name = String::from_utf8(r.opaque()).unwrap();
            let cookie = r.u64();
            assert_eq!(r.u32(), 1, "{name}: attributes");
            r.skip(FATTR3_LEN);
            assert_eq!(r.u32(), 1, "{name}: a handle");
            assert_eq!(r.opaque().len(), handle::LEN);
            entries.push((name, cookie));
        }
        (st, entries, r.u32() == 1)
    }

    /// READ: the status and the bytes.
    fn read(&mut self, fh: &[u8], offset: u64, count: u32) -> (u32, Vec<u8>) {
        let (a, mut r) =
            self.call(Cred::Sys(UID), NFS, READ, Xdr::default().opaque(fh).u64(offset).u32(count));
        assert_eq!(a, Answer::Accepted(0));
        let st = r.u32();
        r.post_op_attr();
        if st != NFS3_OK {
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
    let (st, entries, _) = c.readdir(&stereo, 0, 4096);
    assert_eq!(st, NFS3_OK, "READDIR");
    let (name, _) = entries.first().expect("an entry");
    c.lookup(&stereo, name)
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
    assert_eq!(st, NFS3_OK);
    let (st, before) = c.read(&file, 4096, 65536);
    assert_eq!(st, NFS3_OK);
    assert_eq!(before.len() as u64, 65536.min(size - 4096));
    drop(c);
    drop(s);

    // A new server over the same SOURCE: the old handles, without a lookup first.
    let s = serve(src.path());
    let mut c = Client::new(s.addr);
    assert_eq!(c.getattr(&root).0, NFS3_OK, "the root's handle after a restart");
    assert_eq!(c.getattr(&file), (NFS3_OK, size), "the file's handle after a restart");
    assert_eq!(c.read(&file, 4096, 65536), (NFS3_OK, before), "the same bytes after a restart");
    drop(c);
    drop(s);

    // Another SOURCE: stale.
    let other = tempfile::tempdir().unwrap();
    std::fs::copy(common::corpus().join("images").join(IMAGE), other.path().join(IMAGE)).unwrap();
    let s = serve(other.path());
    let mut c = Client::new(s.addr);
    assert_eq!(c.getattr(&file).0, NFS3ERR_STALE);
    // Not a handle at all.
    assert_eq!(c.getattr(&[0u8; 16]).0, NFS3ERR_BADHANDLE);
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
    assert_eq!(c.getattr(&root).0, NFS3_OK, "a folder's handle stays");
    assert_eq!(c.getattr(&file).0, NFS3ERR_STALE, "a changed image's file is stale");
}

#[test]
fn only_the_user_is_answered() {
    let src = source();
    let s = serve(src.path());
    let mut c = Client::new(s.addr);
    let root = c.mount();
    let getattr = |c: &mut Client, cred| c.call(cred, NFS, GETATTR, Xdr::default().opaque(&root)).0;
    // Another uid, and AUTH_NONE.
    assert_eq!(getattr(&mut c, Cred::Sys(UID + 1)), Answer::Denied(AUTH_BADCRED));
    assert_eq!(getattr(&mut c, Cred::None), Answer::Denied(AUTH_TOOWEAK));
    let mnt = c.call(Cred::Sys(UID + 1), MOUNT, MNT, Xdr::default().opaque(b"/")).0;
    assert_eq!(mnt, Answer::Denied(AUTH_BADCRED));
    // Root, and anyone's ping.
    assert_eq!(getattr(&mut c, Cred::Sys(0)), Answer::Accepted(0));
    assert_eq!(c.call(Cred::None, NFS, NULL, Xdr::default()).0, Answer::Accepted(0));
}

#[test]
fn fsstat_is_the_volume_size() {
    let src = source();
    let s = serve(src.path());
    let mut c = Client::new(s.addr);
    let root = c.mount();
    let (a, mut r) = c.call(Cred::Sys(UID), NFS, FSSTAT, Xdr::default().opaque(&root));
    assert_eq!(a, Answer::Accepted(0));
    assert_eq!(r.u32(), NFS3_OK);
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

/// A SOURCE of `n` images, each in a folder of its own, so that the root lists `n` folders.
fn source_of(n: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..n {
        let folder = dir.path().join(format!("Disc {i}"));
        std::fs::create_dir(&folder).unwrap();
        std::fs::copy(common::corpus().join("images").join(IMAGE), folder.join(IMAGE)).unwrap();
    }
    dir
}

/// Lists `dir` with `list` (cookie to status, entries, eof), a page at a time, from each
/// page's last cookie: the names in the order read, and the number of pages.
fn all_pages(mut list: impl FnMut(u64) -> (u32, Vec<(String, u64)>, bool)) -> (Vec<String>, usize) {
    let (mut names, mut cookie, mut pages) = (Vec::new(), 0, 0);
    loop {
        let (st, entries, eof) = list(cookie);
        assert_eq!(st, NFS3_OK);
        pages += 1;
        assert!(pages < 100, "the listing does not end");
        if let Some((_, last)) = entries.last() {
            cookie = *last;
        } else {
            assert!(eof, "a page without entries that is not the last");
        }
        names.extend(entries.into_iter().map(|(name, _)| name));
        if eof {
            return (names, pages);
        }
    }
}

#[test]
fn a_folder_is_listed_page_by_page() {
    let src = source_of(5);
    let s = serve(src.path());
    let mut c = Client::new(s.addr);
    let root = c.mount();
    let (st, whole, eof) = c.readdir(&root, 0, 65536);
    assert_eq!(st, NFS3_OK);
    assert!(eof);
    let whole: Vec<String> = whole.into_iter().map(|(name, _)| name).collect();
    assert_eq!(whole.len(), 5, "{whole:?}");

    // Replies too small for more than an entry or two: READDIR, then READDIRPLUS.
    let (names, pages) = all_pages(|cookie| c.readdir(&root, cookie, 300));
    assert_eq!(names, whole);
    assert!(pages > 2, "{pages} pages");
    let (names, pages) = all_pages(|cookie| c.readdirplus(&root, cookie, 500));
    assert_eq!(names, whole);
    assert!(pages > 2, "{pages} pages");

    // A cookie that is no entry's.
    assert_eq!(c.readdir(&root, 0xDEAD_BEEF, 4096).0, NFS3ERR_BAD_COOKIE);
    assert_eq!(c.readdirplus(&root, 0xDEAD_BEEF, 4096).0, NFS3ERR_BAD_COOKIE);
}

#[test]
fn dot_and_dot_dot_are_looked_up() {
    let src = source();
    let s = serve(src.path());
    let mut c = Client::new(s.addr);
    let root = c.mount();
    let dir = c.lookup(&root, IMAGE.trim_end_matches(".iso"));
    assert_eq!(c.lookup(&dir, "."), dir);
    assert_eq!(c.lookup(&dir, ".."), root);
    assert_eq!(c.lookup(&root, ".."), root, "the root is its own parent");
}

/// The record of an NFS call from `UID`, as [`Client::call`] writes it.
fn call_record(xid: u32, proc_: u32, args: &Xdr) -> Vec<u8> {
    let body = Xdr::default().u32(0).opaque(b"test").u32(UID).u32(UID).u32(0);
    let mut x = Xdr::default().u32(xid).u32(0).u32(2).u32(NFS).u32(3).u32(proc_).u32(1).opaque(&body.0);
    x = x.u32(0).opaque(&[]);
    x.0.extend_from_slice(&args.0);
    let mut rec = ((x.0.len() as u32) | 1 << 31).to_be_bytes().to_vec();
    rec.extend_from_slice(&x.0);
    rec
}

#[test]
fn other_clients_are_answered_while_one_reads_slowly() {
    let src = source();
    // Few calls answered at once in all: before, the calls of one connection whose replies were
    // not read held them all.
    let s = serve_with(src.path(), Limits { all_in_flight: 4, ..Limits::default() });
    let mut c = Client::new(s.addr);
    let root = c.mount();
    let file = first_track(&mut c, &root, IMAGE);
    let (_, size) = c.getattr(&file);
    // Far more reply bytes than the sockets' buffers hold, never read.
    let calls = (256 << 20) / size.max(1) + 64;
    let mut stuck = TcpStream::connect(s.addr).unwrap();
    let mut batch = Vec::new();
    for xid in 0..calls as u32 {
        batch.extend(call_record(xid + 1000, READ, &Xdr::default().opaque(&file).u64(0).u32(1 << 20)));
    }
    std::thread::spawn(move || {
        let _ = stuck.write_all(&batch);
        std::thread::sleep(Duration::from_secs(60));
        drop(stuck);
    });
    std::thread::sleep(Duration::from_secs(1));
    // Another client is answered at once, again and again.
    let mut other = Client::new(s.addr);
    let start = std::time::Instant::now();
    for _ in 0..20 {
        assert_eq!(other.read(&file, 0, 4096).0, NFS3_OK);
    }
    assert!(start.elapsed() < Duration::from_secs(5), "answers took {:?}", start.elapsed());
}

#[test]
fn an_idle_connection_makes_room_for_a_new_one() {
    let src = source();
    let idle = Duration::from_millis(200);
    let s = serve_with(src.path(), Limits { max_connections: 2, idle_when_full: idle, ..Limits::default() });
    let mut a = Client::new(s.addr);
    let mut b = Client::new(s.addr);
    a.mount();
    b.mount();
    // No one waits: idle connections stay.
    std::thread::sleep(idle * 5);
    a.mount();
    b.mount();
    // A third waits for a place, and gets one once a connection has been idle long enough.
    let start = std::time::Instant::now();
    let mut c = Client::new(s.addr);
    c.mount();
    assert!(start.elapsed() < Duration::from_secs(5), "waited {:?}", start.elapsed());
    // One of the first two was closed for it, the other is still open.
    let closed = |x: &mut Client| {
        x.s.set_read_timeout(Some(Duration::from_millis(500))).unwrap();
        let mut byte = [0u8; 1];
        match x.s.read(&mut byte) {
            Ok(n) => n == 0,
            Err(e) => e.kind() == std::io::ErrorKind::ConnectionReset,
        }
    };
    assert_eq!([closed(&mut a), closed(&mut b)].iter().filter(|c| **c).count(), 1);
}

/// Real discs: every stereo DSF file of the images read whole over one connection, with 1, 4
/// and 16 READs of 1 MiB in flight, as an NFS client's read-ahead sends them; prints the rate
/// of each, for comparing changes to how calls are answered per connection.
#[test]
#[ignore = "needs real disc images: set SACDFS_REAL_IMAGES"]
fn real_discs_pipelined_reads() {
    let (dir, images) = common::real_images_or_skip!();
    let threads = std::thread::available_parallelism().map_or(4, |n| n.get().min(8));
    for depth in [1usize, 4, 16] {
        // A new server each time: nothing cached.
        let s = serve_engine(&dir, Limits::default(), Engine::new(threads, 64 << 20).unwrap());
        let mut c = Client::new(s.addr);
        let root = c.mount();
        let mut files = Vec::new();
        for image in &images {
            let rel = image.strip_prefix(&dir).unwrap().with_extension("");
            let mut fh = root.clone();
            for part in rel.components() {
                fh = c.lookup(&fh, &part.as_os_str().to_string_lossy());
            }
            let stereo = c.lookup(&fh, "Stereo");
            let (_, entries, _) = c.readdir(&stereo, 0, 1 << 16);
            for (name, _) in entries.iter().filter(|(n, _)| n.ends_with(".dsf")) {
                let f = c.lookup(&stereo, name);
                let size = c.getattr(&f).1;
                files.push((f, size));
            }
        }
        let started = std::time::Instant::now();
        let mut total = 0u64;
        let mut xid = 1_000_000u32;
        for (fh, size) in &files {
            let mut next = 0u64;
            let mut out = 0usize;
            loop {
                while out < depth && next < *size {
                    xid += 1;
                    let rec = call_record(xid, READ, &Xdr::default().opaque(fh).u64(next).u32(1 << 20));
                    c.s.write_all(&rec).unwrap();
                    next += 1 << 20;
                    out += 1;
                }
                if out == 0 {
                    break;
                }
                let r = c.record().expect("a reply");
                total += r.len() as u64;
                out -= 1;
            }
        }
        let secs = started.elapsed().as_secs_f64();
        println!(
            "{depth} READs in flight: {} files, {:.0} MB in {secs:.1} s, {:.1} MB/s",
            files.len(),
            total as f64 / 1e6,
            total as f64 / 1e6 / secs
        );
    }
}
