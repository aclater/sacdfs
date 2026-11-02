# Roon

**Tested** with Roon Server 2.x (its official image) as an app on the same NAS as sacdfs, over
SMB, on sacdfs 0.2.0: Roon imported the tracks, folder art and a playlist, picked up an
upgrade by itself, and kept playing through a sacdfs restart. Details:
[clients](../clients.md#roon).

## sacdfs settings

| Setting | Value | Why |
|---|---|---|
| `SACDFS_FORMATS` | `dsf` | with both formats every track shows twice; Roon plays DSD and converts it itself |
| `SACDFS_FLATTEN` | `1` | each disc's `Stereo` and `Multichannel` folders sit under the album |
| `SMB_USER`, `SMB_PASSWORD` | yours | Roon asks for a user name and password |

Leave `SACDFS_CACHE_MB` at its default (32).

## Add the share

Roon's "Add network share" has no port field: it needs SMB on port 445. On a NAS whose own SMB
service holds 445 (TrueNAS), give sacdfs an address on a Docker network of its own. Add to the
sacdfs app's YAML ([truenas-scale.md](truenas-scale.md)), with a subnet nothing else uses:

```yaml
services:
  sacdfs:
    # ... as in truenas-scale.md, plus:
    networks:
      sacd:
        ipv4_address: 172.29.42.10
networks:
  sacd:
    ipam:
      config:
        - subnet: 172.29.42.0/24
```

1. Give the **Roon** app `cap_add: [SYS_ADMIN, DAC_READ_SEARCH]` and
   `security_opt: [apparmor=unconfined]`: Roon mounts network shares itself, inside its
   container. Without them it logs `could not mount ... tried SMB versions 3, 2.1, 2.0, 1.0`.
2. In Roon: Settings, Storage, Add Folder, **Add network share**: `\\172.29.42.10\sacd`, with
   the `SMB_USER` name and password.

## Check it works

Roon's log shows `[roon/cifs] Connected`, and the albums appear as Roon imports them. Roon's
audio analysis then reads every track in full: expect a burst of CPU load on the NAS, or lower
the analysis speed in Roon's settings.

**Discs you also have as extracted files:** Roon merges the two copies into one album and may
play either. Remove the extracts to be sure you play from sacdfs.

Not validated: Roon on another machine than the NAS.
