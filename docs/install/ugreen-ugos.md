# sacdfs on UGREEN UGOS Pro

**Not yet validated on UGOS Pro.** Nobody has run sacdfs on UGOS Pro itself and reported back.

Most UGOS Pro owners would use its Docker app. sacdfs's container needs `/dev/fuse` and the
capability `SYS_ADMIN` (and `DAC_READ_SEARCH` for NFS), and whether UGOS Pro's Docker app
allows that is not known. **Docker (Compose): untested on UGOS Pro**, whose VM has no Docker.
The Compose file for every NAS container manager is
[deploy/nas-compose.yaml](../../deploy/nas-compose.yaml). It was run with Docker Compose on
ASUSTOR ADM ([asustor.md](asustor.md#compose)) and on Podman. In the Docker app, create a project
from the file (the menu names were not seen) and change the one marked line to the path of your
images. The file publishes ports 10445 and 12049 because UGOS's own file services use 445 and 2049.
The general container guide is [docker-podman.md](docker-podman.md), and every setting is in the
[deployment guide](../deployment.md).

**A kernel-only check, not UGOS:** Debian 12 on UGREEN's 6.18.15 kernel, in a VM, without the
UGOS apps, web interface or Docker. With the 0.5.0 release, steps 1 to 9 of [the NAS steps](nas-binary.md), as written with `/volume1`, read all 22 files of two
test images identical to the reference. Earlier, 0.4.1-rc.3 served NFSv3, NFSv4 and SMB
loopback mounts correctly there. This says only that UGREEN's kernel runs sacdfs. It says
nothing about UGOS Pro itself.

Reports are welcome: open an issue that mentions
[#82](https://github.com/aclater/sacdfs/issues/82), with the system's version, how the
container was started, and what worked.
