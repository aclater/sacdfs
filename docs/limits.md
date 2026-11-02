# Limits

What sacdfs does not do, and how it behaves at the edges.

- Linux only, and read-only; other systems read the files over SMB or NFS from
  [the container](deployment.md).
- DSF and WAV output only: no DSDIFF (DFF), no FLAC, no DoP, no 44.1/48 kHz or 16-bit PCM.
- New or removed images and sidecar files show up without a remount, within a few seconds
  (a folder is read again when its modification time in `SOURCE` changes). A new image
  inside a folder that was a single image's chain under `--flatten` needs a remount.
- A DSF file's header read decodes the track's first quarter second or so (a WAV file's
  decodes nothing); a program that reads every audio file (fingerprinting, loudness scans)
  converts every WAV file, so use `--format dsf` for those.
- A damaged image shows up as missing directories or files, filled audio (with a warning in
  the log) or an I/O error on one file. It never stops the mount.
