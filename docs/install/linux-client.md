# Linux: mount the sacdfs shares

This connects a Linux computer to a sacdfs server, so your player sees the discs as a normal
folder. About 5 minutes.

Checked on sacdfs 0.6.0 on Fedora 45 (NFS and SMB, by hand). The `/etc/fstab` lines were checked
on 0.4.0. See [Details](#details).

## What you need

- A running sacdfs server: [Docker or Podman](docker-podman.md), or your NAS's guide.
- The server's name or address. Below it is `nas`: replace it with yours.
- A terminal on the Linux computer.

## Steps

1. **Install the mount tools.** Fedora or RHEL:

   ```sh
   sudo dnf install nfs-utils cifs-utils
   ```

   Debian or Ubuntu:

   ```sh
   sudo apt install nfs-common cifs-utils
   ```

2. **Make an empty folder to mount on.**

   ```sh
   sudo mkdir -p /mnt/sacd
   ```

3. **Mount the share.** Use NFS (the first line) or SMB (the second). One is enough.

   ```sh
   sudo mount -t nfs4 -o ro,vers=4.2 nas:/sacd /mnt/sacd
   sudo mount -t cifs //nas/sacd /mnt/sacd -o ro,guest,vers=3.1.1
   ```

   *You should see* nothing: the prompt comes back. If the server has a user name and password
   (`SMB_USER`), use [a password file](#a-password-for-smb) instead of `guest`. If the server uses
   other ports, add [the ports](#on-other-ports). If a music player scans the share, or you copy
   from it, add `,rsize=262144` to the SMB options: a scan then costs the server about a seventh,
   and copies were faster (measured on 0.6.2,
   [why](../deployment.md#linux)).

4. **To mount at every start,** add **one** of these lines to the end of `/etc/fstab` (open it with
   `sudo nano /etc/fstab`), then type `sudo systemctl daemon-reload && sudo mount -a`:

   ```
   nas:/sacd   /mnt/sacd  nfs4  ro,vers=4.2,_netdev,nofail  0 0
   //nas/sacd  /mnt/sacd  cifs  ro,guest,vers=3.1.1,_netdev,nofail,x-systemd.automount  0 0
   ```

## Check it worked

```sh
findmnt /mnt/sacd
ls /mnt/sacd
```

`findmnt` shows `nfs4` or `cifs` and `ro`. `ls` shows one folder per disc. Open a `.dsf` or `.wav`
file in your player.

## If something goes wrong

- **You see** `mount: ... bad option` or `wrong fs type`. **Do this:** the tools of step 1 are missing.
  Install them.
- **You see** `Connection refused` or `No route to host`, or the command waits a long time.
  **Do this:** the server isn't running, or the name is wrong. Try the server's address in place of
  its name, and check that the server is up ([Docker or Podman, step 3](docker-podman.md#steps)).
  If the server uses other ports, see [on other ports](#on-other-ports).
- **You see** `Permission denied` (SMB). **Do this:** the server has a user name and password. Use
  [a password file](#a-password-for-smb).

## Details

### A password for SMB

If the server has an `SMB_USER`, keep the password in a file only root can read:

```sh
printf 'username=music\npassword=YOUR-PASSWORD\n' | sudo tee /root/.sacd-smb >/dev/null
sudo chmod 600 /root/.sacd-smb
sudo mount -t cifs //nas/sacd /mnt/sacd -o ro,credentials=/root/.sacd-smb,vers=3.1.1
```

In `/etc/fstab`, replace `guest` with `credentials=/root/.sacd-smb`.

### On other ports

If the server publishes other ports (as on [TrueNAS](truenas-scale.md), or the Compose file), add them:

```sh
sudo mount -t nfs4 -o ro,vers=4.2,port=12049 nas:/sacd /mnt/sacd
sudo mount -t cifs //nas/sacd /mnt/sacd -o ro,credentials=/root/.sacd-smb,vers=3.1.1,port=10445
```

### The fstab options

systemd turns each line into a mount unit (`systemctl list-units 'mnt-sacd*'`). `nofail` lets the
start finish when the NAS is down. `x-systemd.automount` mounts the share the first time something
opens it. `_netdev` waits for the network.

### What was checked

- 0.6.0 on Fedora 45: NFS 4.2 and SMB 3.1.1 mounted by hand on the server's own computer, 22 of 22
  files identical to the reference. Mounting SMB printed `could not connect to ::1` (the client tries
  IPv6 first), then worked.
- 0.4.0 on a Fedora 44 VM against a container on another host: both shares from `/etc/fstab`, back
  after a reboot, and a reboot with the server down did not hang. Other ports and an SMB user were
  checked against the TrueNAS app.
- Not checked: `x-systemd.automount` with NFS (SMB only), and distributions other than Fedora as clients.

More about clients and other systems: [deployment guide](../deployment.md#mounting-the-shares-on-clients).
