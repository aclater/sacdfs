# Source of this corpus

This directory is a verbatim copy of the `corpus/` directory of
[scarletbook-rs](https://github.com/aclater/scarletbook-rs) at tag `v0.3.0`
(commit `e776f68a8da0d231b6db6c2dd9795146dd32c899`), which carries the conformance corpus of
the sacd-spec specifications (sacd-spec `fa602f9`, 136 cases). It is dedicated to the public
domain under CC0 1.0; see [LICENSE](LICENSE). Relative links in [README.md](README.md) point
into that repository.

sacdfs uses it in `tests/corpus.rs` and `tests/mount.rs`: every `ok` track of every case must
read back, through sacdfs, with the per-channel SHA-256 given in `manifest.json`.
