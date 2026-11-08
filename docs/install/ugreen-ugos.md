# sacdfs on UGREEN UGOS Pro

**Not yet validated.** Nobody has run sacdfs on UGREEN UGOS Pro and reported
back, so this guide has no steps yet.

sacdfs runs there only if its container may use `/dev/fuse` and the capability
`SYS_ADMIN` (and `DAC_READ_SEARCH` for NFS); whether UGREEN UGOS Pro allows that is not known.

**Docker (Compose): untested on UGOS Pro.** The Compose file is
[deploy/nas-compose.yaml](../../deploy/nas-compose.yaml), the one file for every NAS container
manager. It was run with Docker Compose and Podman on other systems (see
[docker-podman.md](docker-podman.md) and [asustor.md](asustor.md)); the UGOS VM has no Docker, so
UGOS's Docker app was not tried. In the Docker app, create a project from the file (the menu names were not seen) and change the
one marked line to the path of your images. The
file publishes ports 10445 and 12049 because UGOS's own file services use 445 and 2049. Every
setting is in the [deployment guide](../deployment.md).

A kernel-only check, not UGOS: Debian 12 userland on UGREEN's 6.18.15 kernel, in a VM,
mounted and served sacdfs 0.4.1-rc.3 correctly (the release binary, with NFSv3, NFSv4 and SMB
loopback mounts). It says nothing about UGOS Pro itself.

Reports are welcome: open an issue that mentions
[#82](https://github.com/aclater/sacdfs/issues/82), with the system's version, how the
container was started, and what worked.
