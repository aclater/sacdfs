# sacdfs on QNAP

**Not validated: the binary does not run on the one QNAP system tried, and the container route
is untested.** What was checked is on QuTScloud 5.2.9 (build 20261023, kernel 5.10) in a VM, not
on QNAP hardware, and not on QTS or QuTS hero. Those may differ.

- **Release binary: fails.** QuTScloud 5.2.9 has glibc 2.21; the binary needs glibc 2.28 or later
  ([binary.md](binary.md)). Running it stops with:

  ```
  ./sacdfs: /lib/libc.so.6: weak version `GLIBC_2.28' not found (required by ./sacdfs)
  ./sacdfs: relocation error: ./sacdfs: symbol statx, version GLIBC_2.28 not defined in file libc.so.6 with link time reference
  ```

  FUSE itself was there (`/dev/fuse` present, `fuse` in `/proc/filesystems`), so the glibc version
  is the only thing that stopped it. Tracked in [#244](https://github.com/aclater/sacdfs/issues/244).
- **Container: not tested.** The VM had no Docker, and Container Station could not be installed
  there. sacdfs's container needs `/dev/fuse` and the capability `SYS_ADMIN` (and
  `DAC_READ_SEARCH` for NFS). The general container guide is [docker-podman.md](docker-podman.md),
  and every setting is in the [deployment guide](../deployment.md).

Reports from real QNAP hardware are welcome: open an issue that mentions
[#82](https://github.com/aclater/sacdfs/issues/82), with the system's version, how sacdfs was
started, and what worked.
