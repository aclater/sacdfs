<img src="https://raw.githubusercontent.com/aclater/sacdfs/main/deploy/icon.svg" alt="" width="96">

[sacdfs](../README.md) › Documentation

# Documentation

Every sacdfs document, grouped by what you want to do. Start with the install guide, then the
user guide.

## Install

- [Install guide](install/README.md): the one-line installer, and a guide per NAS, system and player
- [Linux without a container](install/binary.md) · [Linux packages](install/packages.md) ·
  [Docker or Podman](install/docker-podman.md) · [NAS over SSH](install/nas-binary.md)
- [Windows](install/windows.md) · [macOS](install/macos.md) ·
  [Linux clients of the shares](install/linux-client.md)
- [Unraid and TrueNAS app-store entries](install/store-apps.md)
- [Beta testing on a NAS](beta-testing.md)

## Use

- [User guide](user-guide.md): tags, MusicBrainz lookup, sidecar files, mounting at boot,
  `sacdfs verify`, troubleshooting
- [Clients](clients.md): players and music servers tested, with their settings
- [Deployment reference](deployment.md): containers, shares, memory limits, every setting and why

## Reference

- [Options](options.md): every option of `sacdfs`
- [What the mount shows](layout.md)
- [Limits](limits.md)

## Internals

- [Design](design.md): architecture and decisions, with the reasons
- [Performance](performance.md): measurements by release
- [PCM quality](pcm-quality.md): how good the WAV conversion is
- [Development](development.md): leak hunting, memory profiling, skipped and real-disc tests

## Project

- [Releasing](release.md) · [release test plan](test-plan.md)
- [Changelog](../CHANGELOG.md) · [contributing](../CONTRIBUTING.md) · [security](../SECURITY.md)

## See also

- [README](../README.md)
- [Releases](https://github.com/aclater/sacdfs/releases)

Part of the [DSD Toolbox](https://github.com/aclater/dsd-toolbox).
