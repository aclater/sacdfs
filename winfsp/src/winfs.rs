// SPDX-License-Identifier: GPL-3.0-or-later

//! The WinFsp file system over the core (#419, spike).
//!
//! WinFsp asks by path; the core answers by inode number. A path is walked from the root one
//! name at a time through each folder's listing, which is kept per folder with each name's
//! Windows form and case-folded key (see [`crate::names`]). Every core request is answered
//! through a reply that sends on a one-shot channel the WinFsp callback waits on: WinFsp's
//! callbacks must answer before they return (docs/design.md, "Threads").

use std::collections::HashMap;
use std::ffi::c_void;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sacdfs::disc::Tuning;
use sacdfs::engine::Engine;
use sacdfs::pcm::PcmConfig;
use sacdfs::tree::{Output, ROOT, SourceDir, Tree};
use sacdfs::vfs::{self, Attr, DataReply, EntryAttr, Kind, Reply, Volume};
use sacdfs::watchdog::Fail;

use winfsp::filesystem::{
    DirBuffer, DirInfo, DirMarker, FileInfo, FileSecurity, FileSystemContext, OpenFileInfo, VolumeInfo,
    WideNameInfo,
};
use winfsp::host::{FileSystemHost, FineGuard, VolumeParams};
use winfsp::{FspError, U16CStr};

// NTSTATUS values the file system answers with.
const STATUS_OBJECT_NAME_NOT_FOUND: i32 = 0xC000_0034_u32 as i32;
const STATUS_OBJECT_PATH_NOT_FOUND: i32 = 0xC000_003A_u32 as i32;
const STATUS_INSUFFICIENT_RESOURCES: i32 = 0xC000_009A_u32 as i32;
const STATUS_NOT_A_DIRECTORY: i32 = 0xC000_0103_u32 as i32;
const STATUS_FILE_IS_A_DIRECTORY: i32 = 0xC000_00BA_u32 as i32;
const STATUS_MEDIA_WRITE_PROTECTED: i32 = 0xC000_00A2_u32 as i32;
const STATUS_ACCESS_DENIED: i32 = 0xC000_0022_u32 as i32;
const STATUS_FILE_INVALID: i32 = 0xC000_0098_u32 as i32;
const STATUS_INVALID_HANDLE: i32 = 0xC000_0008_u32 as i32;
const STATUS_UNEXPECTED_IO_ERROR: i32 = 0xC000_00E9_u32 as i32;
const STATUS_END_OF_FILE: i32 = 0xC000_0011_u32 as i32;

// CreateOptions bits.
const FILE_DIRECTORY_FILE: u32 = 0x0000_0001;
const FILE_NON_DIRECTORY_FILE: u32 = 0x0000_0040;

// File attributes.
const FILE_ATTRIBUTE_READONLY: u32 = 0x0000_0001;
const FILE_ATTRIBUTE_DIRECTORY: u32 = 0x0000_0010;

/// Owner and group: administrators and SYSTEM may do anything WinFsp allows on a read-only
/// volume; everyone may read, list and traverse.
const SDDL: &str = "O:BAG:BAD:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FRFX;;;WD)";

fn nt(status: i32) -> FspError {
    FspError::NTSTATUS(status)
}

/// The NTSTATUS for a core error.
fn status(e: vfs::Error) -> FspError {
    match e {
        vfs::Error::NotFound => nt(STATUS_OBJECT_NAME_NOT_FOUND),
        vfs::Error::NoMemory => nt(STATUS_INSUFFICIENT_RESOURCES),
        vfs::Error::NotDir => nt(STATUS_NOT_A_DIRECTORY),
        vfs::Error::IsDir => nt(STATUS_FILE_IS_A_DIRECTORY),
        vfs::Error::ReadOnly => nt(STATUS_MEDIA_WRITE_PROTECTED),
        vfs::Error::Access => nt(STATUS_ACCESS_DENIED),
        vfs::Error::Stale => nt(STATUS_FILE_INVALID),
        vfs::Error::BadHandle => nt(STATUS_INVALID_HANDLE),
        vfs::Error::Io => nt(STATUS_UNEXPECTED_IO_ERROR),
        // A source file's own error. On Windows `raw_os_error` is a Win32 error code; the core's
        // own `Os` codes (EMFILE for the sidecar cap) are errno values: see #419.
        vfs::Error::Os(n) => FspError::WIN32(n as u32),
    }
}

/// A reply the calling WinFsp thread waits for.
struct Wait<T>(SyncSender<Result<T, vfs::Error>>);

impl<T: Send + 'static> Fail for Wait<T> {
    fn fail(self, err: vfs::Error) {
        let _ = self.0.send(Err(err));
    }
}

impl<T: Send + 'static> Reply<T> for Wait<T> {
    fn reply(self, r: Result<T, vfs::Error>) {
        let _ = self.0.send(r);
    }
}

/// A read's reply: the bytes, copied.
struct WaitData(SyncSender<Result<Vec<u8>, vfs::Error>>);

impl Fail for WaitData {
    fn fail(self, err: vfs::Error) {
        let _ = self.0.send(Err(err));
    }
}

impl DataReply for WaitData {
    fn data(self, data: &[u8]) {
        let _ = self.0.send(Ok(data.to_vec()));
    }
}

/// Runs a core request and waits for its answer. A reply dropped unanswered (its job never
/// ran) is `Io`.
fn call<T: Send + 'static>(f: impl FnOnce(Wait<T>)) -> Result<T, vfs::Error> {
    let (tx, rx) = sync_channel(1);
    f(Wait(tx));
    rx.recv().unwrap_or(Err(vfs::Error::Io))
}

/// 100-nanosecond intervals since 1601, from a time.
fn filetime(t: SystemTime) -> u64 {
    const EPOCH_1601: u64 = 116_444_736_000_000_000;
    match t.duration_since(UNIX_EPOCH) {
        Ok(d) => EPOCH_1601 + d.as_secs() * 10_000_000 + u64::from(d.subsec_nanos()) / 100,
        Err(_) => EPOCH_1601,
    }
}

fn fill(info: &mut FileInfo, a: &Attr) {
    info.file_attributes = match a.kind {
        Kind::Dir => FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_READONLY,
        Kind::File => FILE_ATTRIBUTE_READONLY,
    };
    info.reparse_tag = 0;
    info.file_size = a.size;
    info.allocation_size = a.size.div_ceil(4096) * 4096;
    info.creation_time = filetime(a.crtime);
    info.last_access_time = filetime(a.atime);
    info.last_write_time = filetime(a.mtime);
    info.change_time = filetime(a.ctime);
    info.index_number = a.ino;
    info.hard_links = 0;
    info.ea_size = 0;
}

/// One entry of a folder, as Windows sees it.
#[derive(Debug, Clone)]
struct Entry {
    name: String,
    entry: EntryAttr,
}

/// A folder's entries, and its names by case-folded key.
#[derive(Debug, Default)]
struct Folder {
    entries: Vec<Entry>,
    by_key: HashMap<String, usize>,
}

/// An open file or folder.
#[derive(Debug)]
pub struct Open {
    ino: u64,
    kind: Kind,
    size: u64,
    /// The core's handle, for a file.
    fh: Option<u64>,
    dir: DirBuffer,
}

/// The file system.
pub struct WinFs {
    volume: Volume,
    security: Vec<u8>,
    folders: Mutex<HashMap<u64, Arc<Folder>>>,
}

impl WinFs {
    fn attr(&self, ino: u64) -> Result<Attr, vfs::Error> {
        call(|r| self.volume.getattr(ino, None, r)).map(|(_, a)| a)
    }

    /// The folder `ino`'s listing; read anew when `fresh`.
    fn folder(&self, ino: u64, fresh: bool) -> Result<Arc<Folder>, vfs::Error> {
        if !fresh && let Some(f) = self.folders.lock().unwrap_or_else(|p| p.into_inner()).get(&ino) {
            return Ok(f.clone());
        }
        let listing = call(|r| self.volume.readdir_plus(ino, 0, r))?;
        let mut folder = Folder::default();
        for (_, name, entry) in listing {
            if name == "." || name == ".." {
                continue;
            }
            let name = crate::names::to_windows(&name.to_string_lossy());
            if crate::names::too_long(&name) {
                log::warn!("inode {ino}: a name of more than 255 UTF-16 units is not shown: {name}");
                continue;
            }
            let key = crate::names::fold(&name);
            if folder.by_key.contains_key(&key) {
                log::warn!("inode {ino}: {name} differs from another name only in case: not shown");
                continue;
            }
            folder.by_key.insert(key, folder.entries.len());
            folder.entries.push(Entry { name, entry });
        }
        let folder = Arc::new(folder);
        self.folders.lock().unwrap_or_else(|p| p.into_inner()).insert(ino, folder.clone());
        Ok(folder)
    }

    /// The entry named `name` (any case) in folder `ino`; the folder is read again if the name
    /// isn't in the listing kept, as it may have appeared since.
    fn child(&self, ino: u64, name: &str) -> Result<Option<Entry>, vfs::Error> {
        let key = crate::names::fold(name);
        for fresh in [false, true] {
            let folder = self.folder(ino, fresh)?;
            if let Some(&k) = folder.by_key.get(&key) {
                return Ok(Some(folder.entries[k].clone()));
            }
        }
        Ok(None)
    }

    /// The inode, kind and stored-case path of `path`.
    fn resolve(&self, path: &U16CStr) -> winfsp::Result<(u64, Kind, Vec<u16>)> {
        let path = path.to_string_lossy();
        let parts: Vec<&str> = path.split('\\').filter(|p| !p.is_empty()).collect();
        let (mut ino, mut kind) = (ROOT, Kind::Dir);
        let mut normal = String::new();
        for (i, part) in parts.iter().enumerate() {
            let last = i + 1 == parts.len();
            if kind != Kind::Dir {
                return Err(nt(STATUS_OBJECT_PATH_NOT_FOUND));
            }
            let missing = if last { STATUS_OBJECT_NAME_NOT_FOUND } else { STATUS_OBJECT_PATH_NOT_FOUND };
            let e = self.child(ino, part).map_err(status)?.ok_or_else(|| nt(missing))?;
            normal.push('\\');
            normal.push_str(&e.name);
            ino = e.entry.attr.ino;
            kind = e.entry.attr.kind;
        }
        if normal.is_empty() {
            normal.push('\\');
        }
        Ok((ino, kind, normal.encode_utf16().collect()))
    }

    fn copy_security(&self, out: Option<&mut [c_void]>) {
        if let Some(out) = out
            && out.len() >= self.security.len()
        {
            // SAFETY: `out` holds at least `security.len()` bytes; the regions don't overlap.
            unsafe {
                std::ptr::copy_nonoverlapping(
                    self.security.as_ptr(),
                    out.as_mut_ptr().cast::<u8>(),
                    self.security.len(),
                );
            }
        }
    }
}

impl FileSystemContext for WinFs {
    type FileContext = Open;

    fn get_security_by_name(
        &self,
        file_name: &U16CStr,
        security_descriptor: Option<&mut [c_void]>,
        _reparse_point_resolver: impl FnOnce(&U16CStr) -> Option<FileSecurity>,
    ) -> winfsp::Result<FileSecurity> {
        let (_, kind, _) = self.resolve(file_name)?;
        self.copy_security(security_descriptor);
        let attributes = match kind {
            Kind::Dir => FILE_ATTRIBUTE_DIRECTORY | FILE_ATTRIBUTE_READONLY,
            Kind::File => FILE_ATTRIBUTE_READONLY,
        };
        Ok(FileSecurity { reparse: false, sz_security_descriptor: self.security.len() as u64, attributes })
    }

    fn open(
        &self,
        file_name: &U16CStr,
        create_options: u32,
        _granted_access: u32,
        file_info: &mut OpenFileInfo,
    ) -> winfsp::Result<Self::FileContext> {
        let (ino, kind, normal) = self.resolve(file_name)?;
        match kind {
            Kind::Dir if create_options & FILE_NON_DIRECTORY_FILE != 0 => {
                return Err(nt(STATUS_FILE_IS_A_DIRECTORY));
            }
            Kind::File if create_options & FILE_DIRECTORY_FILE != 0 => {
                return Err(nt(STATUS_NOT_A_DIRECTORY));
            }
            _ => {}
        }
        let attr = self.attr(ino).map_err(status)?;
        let fh = match kind {
            Kind::File => Some(call(|r| self.volume.open(ino, r)).map_err(status)?),
            Kind::Dir => None,
        };
        fill(file_info.as_mut(), &attr);
        file_info.set_normalized_name(&normal, None);
        Ok(Open { ino, kind, size: attr.size, fh, dir: DirBuffer::new() })
    }

    /// Nothing is created: the volume is read-only.
    #[allow(clippy::too_many_arguments)]
    fn create(
        &self,
        _file_name: &U16CStr,
        _create_options: u32,
        _granted_access: u32,
        _file_attributes: u32,
        _security_descriptor: Option<&[c_void]>,
        _allocation_size: u64,
        _extra_buffer: Option<&[u8]>,
        _extra_buffer_is_reparse_point: bool,
        _file_info: &mut OpenFileInfo,
    ) -> winfsp::Result<Self::FileContext> {
        Err(nt(STATUS_MEDIA_WRITE_PROTECTED))
    }

    fn close(&self, context: Self::FileContext) {
        if let Some(fh) = context.fh {
            self.volume.release(fh);
        }
    }

    fn get_file_info(&self, context: &Self::FileContext, file_info: &mut FileInfo) -> winfsp::Result<()> {
        let attr = call(|r| self.volume.getattr(context.ino, context.fh, r)).map_err(status)?.1;
        fill(file_info, &attr);
        Ok(())
    }

    fn get_security(
        &self,
        _context: &Self::FileContext,
        security_descriptor: Option<&mut [c_void]>,
    ) -> winfsp::Result<u64> {
        self.copy_security(security_descriptor);
        Ok(self.security.len() as u64)
    }

    fn read_directory(
        &self,
        context: &Self::FileContext,
        _pattern: Option<&U16CStr>,
        marker: DirMarker,
        buffer: &mut [u8],
    ) -> winfsp::Result<u32> {
        if context.kind != Kind::Dir {
            return Err(nt(STATUS_NOT_A_DIRECTORY));
        }
        if let Ok(lock) = context.dir.acquire(marker.is_none(), None) {
            let folder = self.folder(context.ino, true).map_err(status)?;
            if context.ino != ROOT {
                let me = self.attr(context.ino).map_err(status)?;
                for dot in [".", ".."] {
                    let mut di: DirInfo<255> = DirInfo::new();
                    di.set_name(dot)?;
                    fill(di.file_info_mut(), &me);
                    lock.write(&mut di)?;
                }
            }
            for e in &folder.entries {
                let mut di: DirInfo<255> = DirInfo::new();
                di.set_name(&e.name)?;
                fill(di.file_info_mut(), &e.entry.attr);
                lock.write(&mut di)?;
            }
        }
        Ok(context.dir.read(marker, buffer))
    }

    fn read(&self, context: &Self::FileContext, buffer: &mut [u8], offset: u64) -> winfsp::Result<u32> {
        let fh = context.fh.ok_or_else(|| nt(STATUS_FILE_IS_A_DIRECTORY))?;
        // End of file is a status, never an empty read (#22).
        if offset >= context.size {
            return Err(nt(STATUS_END_OF_FILE));
        }
        let want = buffer.len().min((context.size - offset) as usize);
        let mut got = 0;
        while got < want {
            let size = (want - got).min(vfs::MAX_READ as usize) as u32;
            let (tx, rx) = sync_channel(1);
            self.volume.read(context.ino, fh, offset + got as u64, size, WaitData(tx));
            let data = rx.recv().unwrap_or(Err(vfs::Error::Io)).map_err(status)?;
            if data.is_empty() {
                break;
            }
            buffer[got..got + data.len()].copy_from_slice(&data);
            got += data.len();
        }
        if got == 0 {
            return Err(nt(STATUS_END_OF_FILE));
        }
        Ok(got as u32)
    }

    fn get_volume_info(&self, out_volume_info: &mut VolumeInfo) -> winfsp::Result<()> {
        let s = self.volume.statfs();
        out_volume_info.total_size = s.blocks.saturating_mul(u64::from(s.frsize));
        out_volume_info.free_size = 0;
        out_volume_info.set_volume_label("sacdfs");
        Ok(())
    }
}

/// The self-relative security descriptor of every file and folder.
fn security_descriptor() -> std::io::Result<Vec<u8>> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::PSECURITY_DESCRIPTOR;
    let sddl: Vec<u16> = SDDL.encode_utf16().chain(Some(0)).collect();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    let mut len = 0u32;
    // SAFETY: `sddl` is NUL-terminated; on success `sd` points at `len` bytes that LocalFree frees.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            windows::core::PCWSTR(sddl.as_ptr()),
            SDDL_REVISION_1,
            &mut sd,
            Some(&mut len),
        )
        .map_err(|e| std::io::Error::other(e.to_string()))?;
        let bytes = std::slice::from_raw_parts(sd.0.cast::<u8>(), len as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        Ok(bytes)
    }
}

/// Loads winfsp-x64.dll from WinFsp's install folder (the registry's `InstallDir`), so that
/// `winfsp_init`, which asks for it by name, finds it loaded. Best effort: without it, the
/// DLL is looked for next to the program and on the search path.
fn load_winfsp() {
    use windows::Win32::System::LibraryLoader::LoadLibraryW;
    use windows::Win32::System::Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_SZ, RegGetValueW};
    use windows::core::{HSTRING, w};
    for key in [w!("SOFTWARE\\WOW6432Node\\WinFsp"), w!("SOFTWARE\\WinFsp")] {
        let mut buf = [0u16; 520];
        let mut size = (buf.len() * 2) as u32;
        // SAFETY: `buf` holds `size` bytes; the value is a NUL-terminated string.
        let found = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key,
                w!("InstallDir"),
                RRF_RT_REG_SZ,
                None,
                Some(buf.as_mut_ptr().cast()),
                Some(&mut size),
            )
        };
        if found.is_err() {
            continue;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
        let mut dir = String::from_utf16_lossy(&buf[..len]);
        if !dir.ends_with('\\') {
            dir.push('\\');
        }
        let dll = HSTRING::from(format!("{dir}bin\\winfsp-x64.dll"));
        // SAFETY: a plain library load by full path.
        if unsafe { LoadLibraryW(&dll) }.is_ok() {
            return;
        }
    }
}

struct Args {
    source: PathBuf,
    mountpoint: String,
    threads: usize,
    cache_mb: usize,
    kernel_cache: bool,
    pcm_rate: sacdfs::pcm::PcmRate,
}

fn args() -> Result<Args, String> {
    let mut it = std::env::args_os().skip(1);
    let mut pos = Vec::new();
    let (mut threads, mut cache_mb, mut kernel_cache) = (0usize, 256usize, false);
    let mut pcm_rate = sacdfs::pcm::PcmRate::default();
    while let Some(a) = it.next() {
        match a.to_str() {
            Some("--threads") => {
                threads = it.next().and_then(|v| v.to_str()?.parse().ok()).ok_or("--threads N")?
            }
            Some("--cache-mb") => {
                cache_mb = it.next().and_then(|v| v.to_str()?.parse().ok()).ok_or("--cache-mb N")?
            }
            Some("--kernel-cache") => kernel_cache = true,
            Some("--pcm-rate") => {
                use sacdfs::pcm::PcmRate;
                pcm_rate = match it.next().and_then(|v| v.into_string().ok()).as_deref() {
                    Some("88200") => PcmRate::R88200,
                    Some("176400") => PcmRate::R176400,
                    Some("352800") => PcmRate::R352800,
                    _ => return Err("--pcm-rate 88200, 176400 or 352800".into()),
                }
            }
            _ => pos.push(a),
        }
    }
    if pos.len() != 2 {
        return Err(
            "usage: sacdfs-win SOURCE MOUNTPOINT [--threads N] [--cache-mb N] [--kernel-cache] [--pcm-rate HZ]".into()
        );
    }
    let threads =
        if threads == 0 { std::thread::available_parallelism().map_or(4, |n| n.get()) } else { threads };
    Ok(Args {
        source: PathBuf::from(&pos[0]),
        mountpoint: pos[1].to_string_lossy().into_owned(),
        threads,
        cache_mb,
        kernel_cache,
        pcm_rate,
    })
}

pub fn main() -> ExitCode {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sacdfs-win: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), String> {
    let args = args()?;
    load_winfsp();
    winfsp::winfsp_init().map_err(|_| "WinFsp is not installed (winfsp-x64.dll not found)".to_owned())?;
    let source = args.source.canonicalize().map_err(|e| format!("{}: {e}", args.source.display()))?;
    sacdfs::dsf::set_mc_album_suffix(true);
    let root = SourceDir::scan(&source).map_err(|e| format!("{}: {e}", source.display()))?;
    log::info!("{}: {} image files", source.display(), root.image_count());
    let pcm = PcmConfig { rate: args.pcm_rate, ..PcmConfig::default() };
    let engine = Arc::new(
        Engine::with_pcm(args.threads, args.cache_mb << 20, pcm).map_err(|e| format!("engine: {e}"))?,
    );
    let output = Output { samples_per_frame: args.pcm_rate.samples_per_frame() as u32, ..Output::default() };
    let tree = Tree::with_options(root, Tuning::default(), output).with_pcm(&pcm);
    let tree = Arc::new(if source.is_dir() { tree.watching(&Default::default()) } else { tree });
    let volume =
        Volume::new(tree, engine, args.threads.clamp(4, 16), 0, 0).map_err(|e| format!("volume: {e}"))?;
    let fs = WinFs {
        volume,
        security: security_descriptor().map_err(|e| format!("security: {e}"))?,
        folders: Mutex::default(),
    };

    let mut vp = VolumeParams::new();
    vp.sector_size(4096)
        .sectors_per_allocation_unit(1)
        .max_component_length(crate::names::MAX_UNITS as u16)
        .volume_creation_time(filetime(SystemTime::now()))
        .volume_serial_number(0x5ACD_0001)
        .case_sensitive_search(false)
        .case_preserved_names(true)
        .unicode_on_disk(true)
        .persistent_acls(true)
        .read_only_volume(true)
        .post_cleanup_when_modified_only(true)
        .file_info_timeout(if args.kernel_cache { u32::MAX } else { 1000 })
        .filesystem_name("sacdfs");
    let mut host = FileSystemHost::<WinFs, FineGuard>::new(vp, fs).map_err(|e| format!("WinFsp: {e:?}"))?;
    host.mount(&args.mountpoint).map_err(|e| format!("{}: mount: {e:?}", args.mountpoint))?;
    let threads = u32::try_from(args.threads.clamp(2, 64)).unwrap_or(4);
    host.start_with_threads(threads).map_err(|e| format!("WinFsp dispatcher: {e:?}"))?;
    log::info!("{} mounted at {}", source.display(), args.mountpoint);
    // Runs until the process is stopped; WinFsp removes the drive when it ends.
    loop {
        std::thread::sleep(Duration::from_secs(3600));
    }
}
