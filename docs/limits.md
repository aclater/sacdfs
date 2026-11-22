# Limits

What sacdfs does not do, and how it behaves at the edges.

- Read-only. Runs on Linux and, since 0.6.5, Windows 10 and 11 x64
  ([windows.md](install/windows.md)); on macOS since 0.6.5, as a release build mounted
  through the Mac's own NFS client ([macos.md](install/macos.md)). Other systems read the files over SMB or NFS from a NAS or Linux
  computer that runs sacdfs ([deployment](deployment.md)). [Platforms](../README.md#platforms).
- DSF and WAV output only: no DSDIFF (DFF), no FLAC, no DoP, no 44.1/48 kHz or 16-bit PCM.
- New or removed images and sidecar files show up without a remount, within a few seconds
  (a folder is read again when its modification time in `SOURCE` changes). A new image
  inside a folder that was a single image's chain under `--flatten` needs a remount.
- A DSF file's header read decodes the track's first quarter second or so (a WAV file's
  decodes nothing); a program that reads every audio file (fingerprinting, loudness scans)
  converts every WAV file, so use `--format dsf` for those.
- A damaged image shows up as missing directories or files, filled audio (with a warning in
  the log) or an I/O error on one file. It never stops the mount.


