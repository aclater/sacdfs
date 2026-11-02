# Mount the shares on a Linux client

**Validated** on a Fedora 44 VM (kernel 6.19) against sacdfs 0.4.0 in a container on another
host: NFS and SMB mounted by hand and from `/etc/fstab`, both back after a reboot, and a
reboot with the server down finished without hanging. The same mount commands, run on the
server's own host, read every DSF track of five test discs over NFS and over SMB, matching the
reference hashes (93 of 93 each). Mounting with other ports and an SMB user was
validated against the TrueNAS app. Reference:
[deployment guide](../deployment.md#mounting-the-shares-on-clients).

Below, `nas` is your server's name or address; the SMB share is `sacd`, the NFS export `/sacd`.

1. Install the clients: `sudo dnf install nfs-utils cifs-utils` (Fedora, RHEL) or
   `sudo apt install nfs-common cifs-utils` (Debian, Ubuntu).
2. Mount by hand, NFS or SMB:

   ```sh
   sudo mkdir -p /mnt/sacd
   sudo mount -t nfs4 -o ro,vers=4.2 nas:/sacd /mnt/sacd
   # or SMB, guest access:
   sudo mount -t cifs //nas/sacd /mnt/sacd -o ro,guest,vers=3.1.1
   ```

3. If the share has a user (`SMB_USER`), keep the password in a file only root can read:

   ```sh
   printf 'username=music\npassword=YOUR-PASSWORD\n' | sudo tee /root/.sacd-smb >/dev/null
   sudo chmod 600 /root/.sacd-smb
   sudo mount -t cifs //nas/sacd /mnt/sacd -o ro,credentials=/root/.sacd-smb,vers=3.1.1
   ```

4. To mount at boot, add **one** of these lines to `/etc/fstab`, then run
   `sudo systemctl daemon-reload && sudo mount -a`:

   ```
   nas:/sacd   /mnt/sacd  nfs4  ro,vers=4.2,_netdev,nofail  0 0
   //nas/sacd  /mnt/sacd  cifs  ro,guest,vers=3.1.1,_netdev,nofail,x-systemd.automount  0 0
   ```

   systemd turns each line into a mount unit. `nofail` lets the boot finish if the NAS is
   down; `x-systemd.automount` mounts the share the first time something opens it. For a
   user, replace `guest` with `credentials=/root/.sacd-smb`.

## On other ports

If the server publishes other ports (as on [TrueNAS](truenas-scale.md)), add them:

```sh
sudo mount -t nfs4 -o ro,vers=4.2,port=12049 nas:/sacd /mnt/sacd
sudo mount -t cifs //nas/sacd /mnt/sacd -o ro,credentials=/root/.sacd-smb,vers=3.1.1,port=10445
```

## Check it works

```sh
findmnt /mnt/sacd                    # nfs4 or cifs, ro
ls /mnt/sacd                         # your artists or discs
systemctl list-units 'mnt-sacd*'     # with fstab: the .mount (and .automount) unit active
```

Not validated: the `x-systemd.automount` option with NFS (it was run with SMB only), and
other distributions than Fedora as clients.
