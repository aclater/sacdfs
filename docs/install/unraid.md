# sacdfs on Unraid

**Not yet validated.** Nobody has run sacdfs on Unraid and reported
back, so this guide has no steps yet.

sacdfs runs there only if its container may use `/dev/fuse` and the capability
`SYS_ADMIN` (and `DAC_READ_SEARCH` for NFS); whether Unraid allows that is not known. The general
container guide is [docker-podman.md](docker-podman.md), and every setting is in the
[deployment guide](../deployment.md).

Reports are welcome: open an issue that mentions
[#82](https://github.com/aclater/sacdfs/issues/82), with the system's version, how the
container was started, and what worked.
