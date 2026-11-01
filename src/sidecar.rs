// SPDX-License-Identifier: GPL-3.0-or-later

//! Sidecar files: folder art, scans and playlists passed through from SOURCE (#79, #75).
//!
//! A source directory that holds images may also hold files that belong with them:
//! `folder.jpg`, `cover.png`, an `Artwork/` or `Scans/` folder, a playlist. These are shown
//! at their relative path, read-only and byte for byte, as their source file is: sacdfs
//! never parses them. Which names count is an allow-list ([`Sidecars`]): extensions, and
//! folder names whose whole content passes.
//!
//! **Untrusted paths.** Every sidecar is opened relative to a descriptor of SOURCE, one
//! path component at a time with `O_NOFOLLOW`, so that nothing is followed out of SOURCE
//! even if the tree changes after the scan (#67). A symbolic link that is itself a sidecar
//! is followed once, when its target, resolved by name, stays inside SOURCE and outside
//! MOUNTPOINT; a link to a link, or a link in the target's path, is not followed. Only
//! regular files are served; opening never blocks on a FIFO or device.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io;
use std::os::fd::OwnedFd;
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime};

use nix::fcntl::{AtFlags, OFlag, openat, readlinkat};
use nix::sys::stat::{FileStat, Mode, SFlag, fstat, fstatat};

/// The default allow-list: images, playlists and two folder names.
pub const DEFAULT: &str = "jpg,jpeg,png,webp,m3u,m3u8,Artwork/,Scans/";

/// Which files and folders of SOURCE are passed through, matched without regard to ASCII
/// case. Names starting with `.` (hidden files, macOS `._` files) never are.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sidecars {
    /// File extensions, lower case, without the dot.
    exts: Vec<String>,
    /// Folder names, lower case.
    folders: Vec<String>,
}

impl Sidecars {
    /// Nothing passed through: the tree shows only what it makes from images.
    pub fn none() -> Sidecars {
        Sidecars::default()
    }

    /// The default allow-list ([`DEFAULT`]).
    pub fn standard() -> Sidecars {
        Sidecars::parse(DEFAULT).unwrap_or_default()
    }

    /// Parses a comma-separated list, applied from left to right to an empty list:
    /// `ext` or `.ext` adds an extension, `Name/` a folder name, `default` the default list,
    /// `-item` removes an item, and `none` empties the list. So `default,cue` widens the
    /// default, `default,-webp` narrows it, and `jpg,png` replaces it.
    pub fn parse(s: &str) -> Result<Sidecars, String> {
        let mut out = Sidecars::none();
        for raw in s.split(',') {
            let item = raw.trim();
            if item.is_empty() {
                continue;
            }
            let (remove, item) = match item.strip_prefix('-') {
                Some(rest) => (true, rest.trim()),
                None => (false, item),
            };
            match item.to_ascii_lowercase().as_str() {
                "none" if remove => return Err("-none: use none to pass nothing".to_owned()),
                "none" => out = Sidecars::none(),
                "default" => {
                    for d in DEFAULT.split(',') {
                        out.apply(d, remove)?;
                    }
                }
                _ => out.apply(item, remove)?,
            }
        }
        Ok(out)
    }

    fn apply(&mut self, item: &str, remove: bool) -> Result<(), String> {
        let bad = || {
            Err(format!(
                "sidecar item {item:?}: use an extension (jpg), a folder name ending in / (Artwork/), \
                 default, none, or -item to remove one"
            ))
        };
        let (list, name) = match item.strip_suffix('/') {
            Some(folder) => (&mut self.folders, folder),
            None => (&mut self.exts, item.strip_prefix('.').unwrap_or(item)),
        };
        if name.is_empty() || name.starts_with('.') || name.contains(['/', '\0']) || name == ".." {
            return bad();
        }
        if name.eq_ignore_ascii_case("iso") {
            return Err("sidecar item iso: images are shown as their audio, not passed through".to_owned());
        }
        let name = name.to_ascii_lowercase();
        if remove {
            list.retain(|n| *n != name);
        } else if !list.contains(&name) {
            list.push(name);
        }
        Ok(())
    }

    /// Whether nothing is passed through.
    pub fn is_empty(&self) -> bool {
        self.exts.is_empty() && self.folders.is_empty()
    }

    /// Whether a file named `name` is passed through.
    pub fn file(&self, name: &OsStr) -> bool {
        let n = name.as_encoded_bytes();
        if n.first() == Some(&b'.') {
            return false;
        }
        let Some(dot) = n.iter().rposition(|&b| b == b'.') else { return false };
        let ext = &n[dot + 1..];
        self.exts.iter().any(|e| e.as_bytes().eq_ignore_ascii_case(ext))
    }

    /// Whether a folder named `name` is passed through with all of its content.
    pub fn folder(&self, name: &OsStr) -> bool {
        let n = name.as_encoded_bytes();
        n.first() != Some(&b'.') && self.folders.iter().any(|f| f.as_bytes().eq_ignore_ascii_case(n))
    }

    /// The list in the form [`Sidecars::parse`] reads (for logs).
    pub fn describe(&self) -> String {
        let mut v: Vec<String> = self.exts.clone();
        v.extend(self.folders.iter().map(|f| format!("{f}/")));
        if v.is_empty() { "none".to_owned() } else { v.join(",") }
    }
}

/// Whether a name is hidden (starts with `.`): never a sidecar, nor inside a sidecar folder.
pub fn hidden(name: &OsStr) -> bool {
    name.as_encoded_bytes().first() == Some(&b'.')
}

/// A sidecar found by the scan: from names alone, nothing is opened.
#[derive(Debug, Clone)]
pub struct Sidecar {
    /// Its name.
    pub name: OsString,
    /// Its path in SOURCE.
    pub path: PathBuf,
    /// What it is.
    pub kind: SidecarKind,
}

impl Sidecar {
    /// Whether it is folder art: an image file (or link to one), not a playlist or a folder.
    /// Folder art is also shown inside the areas of the images beside it (#95).
    pub fn is_art(&self) -> bool {
        matches!(self.kind, SidecarKind::File | SidecarKind::Link) && is_art_name(&self.name)
    }
}

/// The extensions of folder art (#95), lower case.
pub const ART_EXTS: [&str; 4] = ["jpg", "jpeg", "png", "webp"];

/// Whether `name` has the extension of an image, without regard to ASCII case.
pub fn is_art_name(name: &OsStr) -> bool {
    let n = name.as_encoded_bytes();
    n.iter()
        .rposition(|&b| b == b'.')
        .is_some_and(|dot| ART_EXTS.iter().any(|e| e.as_bytes().eq_ignore_ascii_case(&n[dot + 1..])))
}

/// What a sidecar is.
#[derive(Debug, Clone)]
pub enum SidecarKind {
    /// A file.
    File,
    /// A symbolic link, served as the file it points to if that is allowed.
    Link,
    /// A folder passed through with all its content, sorted by name.
    Folder(Vec<Sidecar>),
}

/// The attributes of a passed-through file, from its source file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Meta {
    /// Size in bytes.
    pub size: u64,
    /// Blocks of 512 bytes allocated.
    pub blocks: u64,
    /// Last access.
    pub atime: SystemTime,
    /// Last modification.
    pub mtime: SystemTime,
    /// Last status change.
    pub ctime: SystemTime,
}

fn time(secs: i64, nanos: i64) -> SystemTime {
    let nanos = u32::try_from(nanos.clamp(0, 999_999_999)).unwrap_or(0);
    if secs >= 0 {
        SystemTime::UNIX_EPOCH + Duration::new(secs.unsigned_abs(), nanos)
    } else {
        SystemTime::UNIX_EPOCH - Duration::from_secs(secs.unsigned_abs())
            + Duration::from_nanos(u64::from(nanos))
    }
}

impl Meta {
    fn of(st: &FileStat) -> Meta {
        Meta {
            size: u64::try_from(st.st_size).unwrap_or(0),
            blocks: u64::try_from(st.st_blocks).unwrap_or(0),
            atime: time(st.st_atime, st.st_atime_nsec),
            mtime: time(st.st_mtime, st.st_mtime_nsec),
            ctime: time(st.st_ctime, st.st_ctime_nsec),
        }
    }
}

fn kind(st: &FileStat) -> SFlag {
    SFlag::from_bits_truncate(st.st_mode & SFlag::S_IFMT.bits())
}

/// Opens SOURCE's directory (followed if it is a link: it was given by the user).
pub fn open_root(root: &Path) -> io::Result<OwnedFd> {
    let fd = nix::fcntl::open(root, OFlag::O_PATH | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC, Mode::empty())?;
    Ok(fd)
}

/// The names of a relative path, or an error if it has anything else (`..`, a root).
fn names(rel: &Path) -> io::Result<Vec<&OsStr>> {
    rel.components()
        .map(|c| match c {
            Component::Normal(n) => Ok(n),
            _ => Err(io::Error::new(io::ErrorKind::InvalidInput, "not a plain relative path")),
        })
        .collect()
}

/// Opens directory `rel` of `root`, one name at a time, following no symbolic link.
pub fn open_dir(root: &OwnedFd, rel: &Path) -> io::Result<OwnedFd> {
    let flags = OFlag::O_PATH | OFlag::O_DIRECTORY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC;
    let mut fd = openat(root, ".", flags, Mode::empty())?;
    for name in names(rel)? {
        fd = openat(&fd, name, flags, Mode::empty())?;
    }
    Ok(fd)
}

/// The attributes of directory `rel` of `root` (not followed if a link).
pub fn stat_dir(root: &OwnedFd, rel: &Path) -> io::Result<Meta> {
    let fd = open_dir(root, rel)?;
    Ok(Meta::of(&fstat(&fd)?))
}

/// Where a sidecar's bytes live, relative to SOURCE: its own path, or for a link the path
/// it names, resolved by name inside SOURCE. Refused (`EXDEV`) if that leaves SOURCE or
/// lies under `exclude` (MOUNTPOINT, relative to SOURCE, when it is inside it).
pub fn target(
    root: &OwnedFd,
    root_path: &Path,
    rel: &Path,
    link: bool,
    exclude: Option<&Path>,
) -> io::Result<PathBuf> {
    let escape = || io::Error::from_raw_os_error(libc::EXDEV);
    let out = if link {
        let parent = rel.parent().unwrap_or(Path::new(""));
        let name = rel.file_name().ok_or_else(escape)?;
        let dir = open_dir(root, parent)?;
        let to = PathBuf::from(readlinkat(&dir, name)?);
        if to.is_absolute() {
            parent_free(to.strip_prefix(root_path).map_err(|_| escape())?)?
        } else {
            parent_free(&parent.join(&to))?
        }
    } else {
        parent_free(rel)?
    };
    if out.as_os_str().is_empty() || exclude.is_some_and(|m| out.starts_with(m)) {
        return Err(escape());
    }
    Ok(out)
}

/// `rel` with `.` and `..` resolved by name; an error if it climbs above its start.
fn parent_free(rel: &Path) -> io::Result<PathBuf> {
    let mut out = PathBuf::new();
    for c in rel.components() {
        match c {
            Component::Normal(n) => out.push(n),
            Component::CurDir => {}
            Component::ParentDir if out.pop() => {}
            _ => return Err(io::Error::from_raw_os_error(libc::EXDEV)),
        }
    }
    Ok(out)
}

fn split(rel: &Path) -> io::Result<(&Path, &OsStr)> {
    let name = rel.file_name().ok_or_else(|| io::Error::from(io::ErrorKind::InvalidInput))?;
    Ok((rel.parent().unwrap_or(Path::new("")), name))
}

fn not_regular() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, "not a regular file")
}

/// The attributes of the regular file at `rel` (a [`target`]), following no link.
pub fn stat_file(root: &OwnedFd, rel: &Path) -> io::Result<Meta> {
    let (dir, name) = split(rel)?;
    let dir = open_dir(root, dir)?;
    let st = fstatat(&dir, name, AtFlags::AT_SYMLINK_NOFOLLOW)?;
    if kind(&st) != SFlag::S_IFREG {
        return Err(not_regular());
    }
    Ok(Meta::of(&st))
}

/// Opens the regular file at `rel` (a [`target`]) for reading, following no link, with its
/// attributes now.
pub fn open_file(root: &OwnedFd, rel: &Path) -> io::Result<(File, Meta)> {
    let (dir, name) = split(rel)?;
    let dir = open_dir(root, dir)?;
    // O_NONBLOCK: a FIFO put there since the listing must not hold the open; it is refused
    // below. It means nothing for a regular file.
    let flags = OFlag::O_RDONLY | OFlag::O_NOFOLLOW | OFlag::O_CLOEXEC | OFlag::O_NONBLOCK | OFlag::O_NOCTTY;
    let fd = openat(&dir, name, flags, Mode::empty())?;
    let st = fstat(&fd)?;
    if kind(&st) != SFlag::S_IFREG {
        return Err(not_regular());
    }
    Ok((File::from(fd), Meta::of(&st)))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn allow_list() {
        let d = Sidecars::standard();
        for n in ["folder.jpg", "Cover.JPEG", "a.b.png", "x.webp", "list.m3u", "list.M3U8"] {
            assert!(d.file(OsStr::new(n)), "{n}");
        }
        for n in ["x.iso", "x.dsf", "jpg", ".folder.jpg", "._cover.jpg", "x.jpg.txt", "x."] {
            assert!(!d.file(OsStr::new(n)), "{n}");
        }
        assert!(d.folder(OsStr::new("Artwork")) && d.folder(OsStr::new("scans")));
        assert!(!d.folder(OsStr::new("Art")) && !d.folder(OsStr::new(".Artwork")));
        assert_eq!(d.describe(), "jpg,jpeg,png,webp,m3u,m3u8,artwork/,scans/");
    }

    #[test]
    fn parse_widens_narrows_replaces() {
        let p = |s: &str| Sidecars::parse(s).unwrap();
        assert!(p("none").is_empty() && p("").is_empty());
        assert_eq!(p("default"), Sidecars::standard());
        let wide = p("default,.cue,Covers/");
        assert!(wide.file(OsStr::new("x.cue")) && wide.folder(OsStr::new("covers")));
        let narrow = p("default,-webp,-Scans/");
        assert!(!narrow.file(OsStr::new("x.webp")) && !narrow.folder(OsStr::new("Scans")));
        assert!(narrow.file(OsStr::new("x.jpg")));
        let only = p("png");
        assert!(only.file(OsStr::new("x.png")) && !only.file(OsStr::new("x.jpg")));
        assert!(!only.folder(OsStr::new("Artwork")));
        assert!(p("default,none,m3u").file(OsStr::new("a.m3u")));
        assert_eq!(p("jpg,jpg,JPG").describe(), "jpg");
        for bad in ["iso", ".iso", "/", "a/b", "..", "../", ".hidden/", "-none"] {
            assert!(Sidecars::parse(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn names_resolve_inside_source() {
        assert_eq!(parent_free(Path::new("a/./b/../c")).unwrap(), Path::new("a/c"));
        assert!(parent_free(Path::new("a/../../c")).is_err());
        assert!(parent_free(Path::new("/etc/passwd")).is_err());
    }

    #[test]
    fn links_stay_inside_source() {
        let dir = tempfile::tempdir().unwrap();
        let root_path = dir.path().canonicalize().unwrap();
        let src = root_path.join("src");
        std::fs::create_dir_all(src.join("A/B")).unwrap();
        std::fs::create_dir_all(src.join("mnt")).unwrap();
        std::fs::write(src.join("A/real.jpg"), b"art").unwrap();
        std::fs::write(root_path.join("outside.jpg"), b"secret").unwrap();
        let link = |name: &str, to: &str| std::os::unix::fs::symlink(to, src.join(name)).unwrap();
        link("A/B/up.jpg", "../real.jpg");
        link("A/B/out.jpg", "../../../outside.jpg");
        link("A/B/abs.jpg", src.join("A/real.jpg").to_str().unwrap());
        link("A/B/absout.jpg", root_path.join("outside.jpg").to_str().unwrap());
        link("A/B/mnt.jpg", "../../mnt/x.jpg");
        link("A/B/chain.jpg", "up.jpg");
        link("A/dirlink", "B");
        let root = open_root(&src).unwrap();
        let t = |rel: &str| target(&root, &src, Path::new(rel), true, Some(Path::new("mnt")));
        assert_eq!(t("A/B/up.jpg").unwrap(), Path::new("A/real.jpg"));
        assert_eq!(t("A/B/abs.jpg").unwrap(), Path::new("A/real.jpg"));
        for bad in ["A/B/out.jpg", "A/B/absout.jpg", "A/B/mnt.jpg"] {
            assert_eq!(t(bad).unwrap_err().raw_os_error(), Some(libc::EXDEV), "{bad}");
        }
        // A link to a link resolves to the second link's path, which is not followed.
        let chain = t("A/B/chain.jpg").unwrap();
        assert!(open_file(&root, &chain).is_err());
        // Plain files open; nothing through a linked directory.
        let (_, meta) = open_file(&root, Path::new("A/real.jpg")).unwrap();
        assert_eq!(meta.size, 3);
        assert_eq!(stat_file(&root, Path::new("A/real.jpg")).unwrap(), meta);
        assert!(open_file(&root, Path::new("A/dirlink/up.jpg")).is_err());
        assert!(stat_file(&root, Path::new("A/B/up.jpg")).is_err(), "a link is not a regular file");
        assert!(stat_file(&root, Path::new("A")).is_err());
        assert!(stat_dir(&root, Path::new("A/dirlink")).is_err());
        assert!(stat_dir(&root, Path::new("A/B")).is_ok());
    }

    #[test]
    fn a_fifo_is_refused_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        nix::unistd::mkfifo(&dir.path().join("x.jpg"), Mode::S_IRUSR | Mode::S_IWUSR).unwrap();
        let root = open_root(dir.path()).unwrap();
        assert!(open_file(&root, Path::new("x.jpg")).is_err());
        assert!(stat_file(&root, Path::new("x.jpg")).is_err());
    }
}
