# sacdfs on UGREEN UGOS Pro

**Not yet validated on UGOS Pro.** Nobody has run sacdfs on UGOS Pro itself and reported back.

Most UGOS Pro owners would use its Docker app. sacdfs's container needs `/dev/fuse` and the
capability `SYS_ADMIN` (and `DAC_READ_SEARCH` for NFS), and whether UGOS Pro's Docker app
allows that is not known. The Compose file that worked under ASUSTOR's Docker is in
[asustor.md](asustor.md#compose), the general container guide is
[docker-podman.md](docker-podman.md), and every setting is in the
[deployment guide](../deployment.md).

**A kernel-only check, not UGOS:** Debian 12 on UGREEN's 6.18.15 kernel, in a VM, without the
UGOS apps, web interface or Docker. With the 0.5.0 code (a build of `main` that
differs from the release only in its version number), steps 1, 2 and
4 to 9 of [the NAS steps](nas-binary.md), as written with `/volume1`, read all 22 files of two
test images identical to the reference. Earlier, 0.4.1-rc.3 served NFSv3, NFSv4 and SMB
loopback mounts correctly there. This says only that UGREEN's kernel runs sacdfs. It says
nothing about UGOS Pro itself.

Reports are welcome: open an issue that mentions
[#82](https://github.com/aclater/sacdfs/issues/82), with the system's version, how the
container was started, and what worked.
