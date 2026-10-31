# Source of this corpus

This directory is a verbatim copy of the `corpus/` directory of
[scarletbook-rs](https://github.com/aclater/scarletbook-rs) at tag `v0.2.0`
(commit `fce832565d87d73eb8680c92ba82d1147e754cca`), which carries the conformance corpus of
the sacd-spec specifications. It is dedicated to the public domain under CC0 1.0; see
[LICENSE](LICENSE). Relative links in [README.md](README.md) point into that repository.

sacdfs uses it in `tests/corpus.rs` and `tests/mount.rs`: every `ok` track of every case must
read back, through sacdfs, with the per-channel SHA-256 given in `manifest.json`.
