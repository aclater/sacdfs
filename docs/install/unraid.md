# sacdfs on Unraid

**Not yet validated.** Nobody has run sacdfs on Unraid and reported
back, so this guide has no steps yet.

sacdfs runs there only if its container may use `/dev/fuse` and the capability
`SYS_ADMIN` (and `DAC_READ_SEARCH` for NFS); whether Unraid allows that is not known.

**Compose plugin: untested on Unraid.** The Compose file is
[deploy/nas-compose.yaml](../../deploy/nas-compose.yaml), the one file for every NAS container
manager. It was run with Docker Compose and Podman on other systems (see
[docker-podman.md](docker-podman.md) and [asustor.md](asustor.md)), not on Unraid. In the Compose
plugin, add a stack from the file (the menu names were not seen) and change the one marked line to
the path of your images. Unraid's own SMB and NFS
use ports 445 and 2049, which is why the file publishes 10445 and 12049. Every setting is in the
[deployment guide](../deployment.md).

Reports are welcome: open an issue that mentions
[#82](https://github.com/aclater/sacdfs/issues/82), with the system's version, how the
container was started, and what worked.
