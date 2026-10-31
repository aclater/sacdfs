# Contributing to sacdfs

Thanks for helping. Bug reports, test images and pull requests are welcome.

## Reporting a bug

Open an [issue](https://github.com/aclater/sacdfs/issues) with the command line, what you
expected, what happened, and a log: run in the foreground with `sacdfs -f -vv ...`. Don't
attach copyrighted disc images. **Security problems** go through a private report instead:
see [SECURITY.md](SECURITY.md).

## Changing code

1. Start with an issue, so the change can be discussed first.
2. Work on a branch and open a pull request against `main`. The default branch only accepts
   pull requests whose checks pass.
3. Run the checks before you push:

   ```sh
   cargo fmt --all --check
   cargo clippy --all-targets --locked -- -D warnings
   SACDFS_FUSE_TESTS=1 cargo test --locked
   cargo deny check advisories bans licenses sources
   ```

   The mount tests need `/dev/fuse` and `fusermount3` (package `fuse3`); without
   `SACDFS_FUSE_TESTS=1` they are skipped where FUSE is missing. In a container, pass
   `--device /dev/fuse --cap-add SYS_ADMIN`.
4. Update the documentation with the code: README.md for anything a user sees,
   docs/design.md when the design changes, CHANGELOG.md under "Unreleased".

## Ground rules

- **Licences.** sacdfs is GPL-3.0-or-later. Code may be adapted from projects whose licence
  allows it into GPL-3.0-or-later (GPL-2.0-or-later, GPL-3.0, LGPL-2.1-or-later, Apache-2.0,
  MIT, BSD, ...; not GPL-2.0-only), keeping its copyright notices and naming the source in the
  commit and in a comment. Dependencies are checked by `deny.toml`.
- **The clean-room wall.** sacdfs is outside the clean room of scarletbook-rs. Nothing from
  sacdfs (code, issues, design notes) may be offered to scarletbook-rs, and sacdfs code must
  never be pointed to from there. Integration needs go to scarletbook-rs as requirements.
- **No SACD format or DST code from elsewhere.** Disc parsing and DST decoding come only from
  scarletbook's public API; don't adapt them from pysacd, sacd-ripper or any other reader
  (and never from the DST reference implementation). Code for other parts (DSD to PCM
  conversion, file formats, FUSE techniques) may be adapted as above.
- **SACD knowledge lives in scarletbook.** sacdfs uses its public API only. A parsing or
  decoding problem is fixed there.
- **Hostile input.** Every image is untrusted. Nothing an image contains may panic, hang or
  grow memory without bound; `clippy::unwrap_used`, `expect_used` and `panic` are warnings,
  and CI treats warnings as errors.
- **Tests.** New behaviour comes with a test. Test images are synthetic: the corpus in
  `tests/corpus/` (CC0), never audio from a real disc.
- **Commits.** Small commits with a message that says why. Actions in workflows are pinned
  by full commit SHA with a version comment.
