# sacdfs

A read-only FUSE filesystem that shows Super Audio CD disc images as DSF files, decoded on
demand by [scarletbook](https://github.com/aclater/scarletbook-rs).

```sh
sacdfs ~/music/sacd-images /mnt/sacd
ls /mnt/sacd/"Some Disc"/Stereo
```

Work in progress: see [docs/design.md](docs/design.md).

## Licence

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.
