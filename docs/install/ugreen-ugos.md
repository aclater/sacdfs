# sacdfs on UGREEN UGOS Pro

**Not yet validated.** Nobody has run sacdfs on UGREEN UGOS Pro and reported
back, so this guide has no steps yet.

sacdfs runs there only if its container may use `/dev/fuse` and the capability
`SYS_ADMIN` (and `DAC_READ_SEARCH` for NFS); whether UGREEN UGOS Pro allows that is not known. The general
container guide is [docker-podman.md](docker-podman.md), and every setting is in the
[deployment guide](../deployment.md).

A kernel-only check, not UGOS: Debian 12 userland on UGREEN's 6.18.15 kernel, in a VM,
mounted and served sacdfs 0.4.1-rc.3 correctly (the release binary, with NFSv3, NFSv4 and SMB
loopback mounts). It says nothing about UGOS Pro itself.

Reports are welcome: open an issue that mentions
[#82](https://github.com/aclater/sacdfs/issues/82), with the system's version, how the
container was started, and what worked.
