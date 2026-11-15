// SPDX-License-Identifier: GPL-3.0-or-later

//! Starting and stopping the mount: the core from the options, the WinFsp volume, and
//! WinFsp's service framework, which runs it in a console (stopped by Ctrl+C or Ctrl+Break),
//! as a Windows service, or under WinFsp.Launcher, and stops it cleanly in each.

use std::io;
use std::path::Path;
use std::time::SystemTime;

use sacdfs::serve::{self, Background};
use sacdfs::vfs::Volume;

use winfsp::host::{FileSystemHost, FileSystemParams, FineGuard, VolumeParams};
use winfsp::service::FileSystemServiceBuilder;

use crate::options::{Args, MountPoint};
use crate::paths::Change;
use crate::winfs::WinFs;

/// How often folders that the core re-reads are looked at for changes, in milliseconds.
const NOTIFY_EVERY_MS: u32 = 1000;

/// How long an image file nobody reads stays open (see `start`).
const IMAGE_IDLE: std::time::Duration = std::time::Duration::from_secs(10);

/// A running mount: what stopping it needs.
struct Mounted {
    host: FileSystemHost<WinFs, FineGuard>,
    background: Background,
    reads: std::sync::Arc<crate::winfs::Reads>,
}
pub fn main(args: Args) -> Result<(), String> {
    let missing = "WinFsp is not installed (winfsp-x64.dll not found in WinFsp's folder): install \
                   WinFsp 2.1 or later, from https://winfsp.dev/rel/ or with `winget install WinFsp.WinFsp`";
    if !load_winfsp() {
        return Err(missing.to_owned());
    }
    let init = winfsp::winfsp_init().map_err(|_| missing.to_owned())?;
    let failure = std::sync::Mutex::new(None::<String>);
    let mut service = FileSystemServiceBuilder::<Mounted>::new()
        .with_start(|| {
            start(&args).map_err(|e| {
                log::error!("{e}");
                *failure.lock().unwrap_or_else(|p| p.into_inner()) = Some(e.to_string());
                winfsp::FspError::WIN32(e.raw_os_error().map_or(31, |n| n as u32))
            })
        })
        .with_stop(|mounted| {
            if let Some(m) = mounted {
                stop(m);
            }
            Ok(())
        })
        .build("sacdfs", init)
        .map_err(|e| format!("WinFsp service: {e:?}"))?;
    service.start().map_err(|e| format!("WinFsp service: {e:?}"))?;
    let result = service.join();
    if let Some(e) = failure.lock().unwrap_or_else(|p| p.into_inner()).take() {
        return Err(e);
    }
    result.map_err(|e| format!("WinFsp service: {e:?}"))
}

fn invalid(msg: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg)
}

/// Mounts SOURCE as the options say, in the steps of `sacdfs::serve`.
fn start(args: &Args) -> io::Result<Mounted> {
    let (options, limits) = (&args.options, &args.limits);
    let mountpoint = MountPoint::parse(&args.mountpoint);
    if let MountPoint::Folder(f) = &mountpoint
        && f.exists()
    {
        return Err(invalid(format!(
            "{}: the folder exists; give a drive letter (X:) or a folder that does not exist yet",
            f.display()
        )));
    }
    let pcm = options.check()?;
    let source = args.source.canonicalize().map_err(|e| serve::with_path(e, &args.source))?;
    let shown = display_path(&source);
    // A folder mount inside SOURCE would be read back through itself when SOURCE is re-read
    // (#56 on Linux): refused.
    if let MountPoint::Folder(f) = &mountpoint {
        let at = serve::canonical_prefix(&std::path::absolute(f)?);
        if at.starts_with(&source) {
            return Err(invalid(format!(
                "{}: inside SOURCE {shown}; mount outside it, or on a drive letter",
                display_path(&at)
            )));
        }
    }
    // --index-cache, else %LOCALAPPDATA%\sacdfs (the core's default on Windows).
    let cache_dir = limits.index_cache_dir(None)?;
    let root = options.scan(&source, None)?;
    log::info!("{shown}: {} image files; sidecar files: {}", root.image_count(), options.sidecars.describe());
    let sizing = options.sizing(&root);
    if let Some(dir) = &cache_dir {
        serve::open_index_cache(dir);
    }
    let parts = serve::Parts {
        source: &source,
        root,
        pcm,
        sizing: &sizing,
        exclude: None,
        cache_dir: cache_dir.as_deref(),
        open_files: (limits.max_open_images, sacdfs::vfs::sidecar_slots::DEFAULT_CAP),
    };
    let (tree, engine) = serve::build(options, parts)?;
    // Windows refuses to rename or delete a folder while a file inside it is open: image files
    // left idle are closed after seconds here, not minutes, so that the library can be
    // reorganised while it is mounted. Reopening an image costs one open, no reading.
    sacdfs::handles::global().configure(limits.max_open_images, IMAGE_IDLE);
    let threads = sizing.threads;
    let volume = Volume::new(tree.clone(), engine.clone(), serve::request_threads(threads), 0, 0)?
        .with_request_timeout(limits.request_timeout())?;
    let sid = user_sid()?;
    let everyone = crate::options::allow_other(args.allow_other, args.no_allow_other, &sid);
    if everyone {
        log::info!("other users may read the mount (--no-allow-other turns it off)");
    }
    let fs = WinFs::new(volume, security_descriptor(&sid, everyone)?);
    let reads = fs.reads();

    let mut vp = VolumeParams::new();
    vp.sector_size(4096)
        .sectors_per_allocation_unit(1)
        .max_component_length(crate::names::MAX_UNITS as u16)
        .volume_creation_time(filetime_now())
        .volume_serial_number(0x5ACD_0001)
        .case_sensitive_search(false)
        .case_preserved_names(true)
        .unicode_on_disk(true)
        .persistent_acls(true)
        .read_only_volume(true)
        // Every cleanup (a program's last close of a file), so that its read-ahead goes.
        .post_cleanup_when_modified_only(false)
        .pass_query_directory_filename(true)
        .filesystem_name("sacdfs");
    if args.no_kernel_cache {
        vp.file_info_timeout(0);
    } else {
        // Contents, attributes, listings and security kept by Windows until the file system
        // says they changed (the change notifications of `WinFs`): an infinite FileInfoTimeout
        // also turns on the cache manager for file data.
        vp.file_info_timeout(u32::MAX)
            .dir_info_timeout(u32::MAX)
            .security_timeout(u32::MAX)
            .volume_info_timeout(1000);
    }
    let params = FileSystemParams { use_dir_info_by_name: true, ..FileSystemParams::default_params(vp) };
    let mut host =
        FileSystemHost::<WinFs, FineGuard>::new_with_timer::<Vec<Change>, NOTIFY_EVERY_MS>(params, fs)
            .map_err(|e| io::Error::other(format!("WinFsp: {e:?}")))?;
    let at = match &mountpoint {
        MountPoint::Drive(d) => d.clone(),
        MountPoint::Folder(f) => f.to_string_lossy().into_owned(),
    };
    host.mount(&at).map_err(|e| io::Error::other(format!("cannot mount on {at}: {e:?}")))?;
    let dispatchers = u32::try_from(threads.clamp(2, 16)).unwrap_or(4);
    host.start_with_threads(dispatchers)
        .map_err(|e| io::Error::other(format!("WinFsp dispatcher: {e:?}")))?;
    let background = serve::start_background(options, sizing, &tree, &engine, cache_dir.as_deref())?;
    log::info!("mounted {shown} on {at}");
    Ok(Mounted { host, background, reads })
}

/// Unmounts, then saves what is kept across mounts.
fn stop(m: &mut Mounted) {
    log::info!("stopping: unmounting");
    m.host.stop();
    m.host.unmount();
    let (reads, bytes, core) = m.reads.get();
    log::info!("reads answered: {reads}, {bytes} bytes; {core} read requests to the core");
    m.background.stop();
    log::info!("unmounted");
}

/// A path as people write it: without the `\\?\` that `canonicalize` puts in front.
pub fn display_path(p: &Path) -> String {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\UNC\") {
        Some(rest) => format!(r"\\{rest}"),
        None => s.strip_prefix(r"\\?\").unwrap_or(&s).to_owned(),
    }
}

fn filetime_now() -> u64 {
    const EPOCH_1601: u64 = 116_444_736_000_000_000;
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(EPOCH_1601, |d| EPOCH_1601 + d.as_secs() * 10_000_000 + u64::from(d.subsec_nanos()) / 100)
}

/// The SID of the user sacdfs-win runs as, in SDDL's string form.
fn user_sid() -> io::Result<String> {
    use windows::Win32::Foundation::{CloseHandle, HANDLE, HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows::Win32::Security::{GetTokenInformation, TOKEN_QUERY, TOKEN_USER, TokenUser};
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    let err = |e: windows::core::Error| io::Error::other(e.to_string());
    // SAFETY: plain Win32 calls; the buffer is sized by the first call and outlives its use,
    // the token is closed and the string freed with LocalFree as documented.
    unsafe {
        let mut token = HANDLE::default();
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).map_err(err)?;
        let mut len = 0u32;
        let _ = GetTokenInformation(token, TokenUser, None, 0, &mut len);
        let mut buf = vec![0u64; (len as usize).div_ceil(8)];
        let got = GetTokenInformation(token, TokenUser, Some(buf.as_mut_ptr().cast()), len, &mut len);
        let _ = CloseHandle(token);
        got.map_err(err)?;
        let user = &*buf.as_ptr().cast::<TOKEN_USER>();
        let mut s = windows::core::PWSTR::null();
        ConvertSidToStringSidW(user.User.Sid, &mut s).map_err(err)?;
        let sid = s.to_string().map_err(|e| io::Error::other(e.to_string()));
        let _ = LocalFree(Some(HLOCAL(s.0.cast())));
        sid
    }
}

/// The self-relative security descriptor of every file and folder. The user who mounted owns
/// them and may read, list and traverse; SYSTEM (the search indexer, Defender) and
/// Administrators may do what a read-only volume allows. With `everyone`, all users may read,
/// list and traverse too, so that another user or the SMB server can serve the files.
///
/// *Why not everyone by default:* a drive letter is seen only in the session that mounted it,
/// but the volume's device path reaches it from any session; its descriptor is what keeps other
/// users out.
fn security_descriptor(sid: &str, everyone: bool) -> io::Result<Vec<u8>> {
    use windows::Win32::Foundation::{HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{
        ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
    };
    use windows::Win32::Security::PSECURITY_DESCRIPTOR;
    let others = if everyone { "(A;;FRFX;;;WD)" } else { "" };
    let sddl = format!("O:{sid}G:{sid}D:P(A;;FA;;;SY)(A;;FA;;;BA)(A;;FRFX;;;{sid}){others}");
    let wide: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
    let mut sd = PSECURITY_DESCRIPTOR::default();
    let mut len = 0u32;
    // SAFETY: `wide` is NUL-terminated; on success `sd` points at `len` bytes that LocalFree frees.
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            windows::core::PCWSTR(wide.as_ptr()),
            SDDL_REVISION_1,
            &mut sd,
            Some(&mut len),
        )
        .map_err(|e| io::Error::other(format!("security descriptor {sddl}: {e}")))?;
        let bytes = std::slice::from_raw_parts(sd.0.cast::<u8>(), len as usize).to_vec();
        let _ = LocalFree(Some(HLOCAL(sd.0)));
        Ok(bytes)
    }
}

/// Loads winfsp-x64.dll from WinFsp's install folder (the registry's `InstallDir`, under
/// HKLM), so that `winfsp_init` and the delay-loaded calls, which ask for it by name, find it
/// loaded. Without it sacdfs-win doesn't start: a DLL of that name found next to the program or
/// on the search path, where another user may have put one, is never loaded.
fn load_winfsp() -> bool {
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
        let Some(len) = buf.iter().position(|&c| c == 0).filter(|&n| n > 0) else { continue };
        let mut dir = String::from_utf16_lossy(&buf[..len]);
        // A full path only: a relative one would be looked for on the search path.
        if !std::path::Path::new(&dir).is_absolute() {
            continue;
        }
        if !dir.ends_with('\\') {
            dir.push('\\');
        }
        let dll = HSTRING::from(format!("{dir}bin\\winfsp-x64.dll"));
        // SAFETY: a plain library load by full path.
        if unsafe { LoadLibraryW(&dll) }.is_ok() {
            return true;
        }
    }
    false
}
