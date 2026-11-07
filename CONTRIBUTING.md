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
   pull requests whose checks pass on a branch that is up to date with `main`. If `main` moves
   while yours waits, merge it into your branch (no force-push) and let the checks run again.
3. Run the checks before you push:

   ```sh
   cargo fmt --all --check
   cargo clippy --all-targets --locked -- -D warnings
   SACDFS_FUSE_TESTS=1 cargo test --locked
   cargo deny check advisories bans licenses sources
   ```

   The mount tests need `/dev/fuse` and `fusermount3` (package `fuse3`); without
   `SACDFS_FUSE_TESTS=1` they are skipped where FUSE is missing, each with a `SKIPPED` line in
   the output. In a container, pass
   `--device /dev/fuse --cap-add SYS_ADMIN`. `tests/no_growth.rs` checks that mount cycles
   leave no heap behind. For the long checks (the soak, heap profiles, memcheck), see
   [docs/development.md](docs/development.md).
4. Update the documentation with the code: README.md, docs/options.md and docs/user-guide.md for anything a
   user sees (the options table in docs/options.md must match `sacdfs --help`; CI checks it with
   `.github/scripts/check-readme-options.py`), docs/design.md when the design changes, with
   the reason for each decision, docs/performance.md with new measurements, and CHANGELOG.md
   under "Unreleased".

## Issues: milestones, labels and claiming

Every open issue says which release it is for, how important it is, where it stands, and
what kind of agent should work on it. The issue list then shows what's up without opening
each issue.

- **Milestone:** the release it is planned for (`0.2.0`, `0.3.0`, ...) or `Later`. Umbrella
  issues have no milestone.
- **Priority:**
  - `P1`: must be in its milestone's release;
  - `P2`: planned for it, can slip one release;
  - `P3`: nice to have.
- **Status:**
  - `status: in progress`: claimed and being worked on;
  - `status: in review`: a PR is open;
  - `status: fixed on main`: merged. The issue stays open until the release ships and has
    been verified, then it is closed.
  - `status: needs decision`: waiting on the owner.
- **Type:** `bug`, `enhancement`, `performance`, `documentation`, `plan`, or `tracking`
  (umbrella and design issues that track other work).
- **Model:**
  - `model: opus` for hard work: concurrency, performance, design, unexplained bugs;
  - `model: sonnet` for routine work: well-specified features, tests, docs, CI.

**Claiming.** Before starting on an issue, check that nobody has claimed it. Then:
1. Comment `Claimed by <agent> (<model>)`, and add `status: in progress`.
2. If you stop without a PR, comment that you're releasing it, and remove the label.
3. When you open a PR, switch the label to `status: in review`. The PR says
   `Closes #N` only if it fully resolves the issue, else `Part of #N`.
4. Don't start on an issue that someone else has claimed. Ask on the issue instead.

## Releasing

The owner tags releases; everything after the tag is automatic (`.github/workflows/release.yml`).
Version tags (`v*`) are immutable, so a tag that went wrong is followed by a new version,
never moved.

1. **Before:** `main` is green, the scarletbook pin is the one meant for the release, and
   the docs describe the release (README, docs/).
2. **Test:** run the [release test plan](docs/test-plan.md) on a release candidate; its exit criteria
   decide whether the final version is tagged.
3. **Dry run:** Actions, "release", "Run workflow" on `main` (or
   `gh workflow run release.yml -R aclater/sacdfs --ref main`). It builds and tests both
   binaries and the image, runs the container share test, and publishes nothing; its summary
   lists the assets and checksums a tag would publish. Pull requests that change the release
   files run it too.
4. **Release pull request:** bump `version` in `Cargo.toml` (and `Cargo.lock`, with
   `cargo update -p sacdfs --offline`), and rename `## [Unreleased]` in CHANGELOG.md to
   `## [X.Y.Z] - YYYY-MM-DD`, with a new empty `## [Unreleased]` above it. That section
   becomes the release notes. At the end of the file, point `[Unreleased]` at
   `compare/vX.Y.Z...HEAD` and add `[X.Y.Z]: .../compare/v<previous>...vX.Y.Z`, so every
   heading links to its changes. Merge it once its checks pass.
5. **Tag** the merge commit on `main`: `git tag -a vX.Y.Z -m "sacdfs X.Y.Z" && git push origin vX.Y.Z`.
   The workflow stops before building anything if the tag isn't `v` plus Cargo.toml's
   version, or CHANGELOG.md has no section for it.
6. **After:** the release has both archives with `.sha256`, `.intoto.jsonl` and
   `SHA256SUMS`; `ghcr.io/aclater/sacdfs:X.Y.Z` and `:latest` exist; check one asset with
   `gh attestation verify`. A version with a hyphen (`0.2.0-rc.1`) is marked as a
   pre-release and doesn't move `:latest`.
7. **Roll-up:** update [RELEASES.md](https://github.com/aclater/sacd/blob/main/RELEASES.md)
   in aclater/sacd (the minor version's summary and release list).

**crates.io (#155).** On a tag of a final version (no hyphen, so not `0.4.0-rc.1`), the
`crates-publish` job runs after the GitHub release: it asks the crates.io API whether that
version is there already, and if so logs it and stops; otherwise it publishes with
crates.io trusted publishing (`rust-lang/crates-io-auth-action`, no stored token). The crate
holds only what `cargo install` and `cargo build` need (`include` in Cargo.toml): check
with `cargo package --list` when adding files that the build reads. Pre-releases are not
published to crates.io.

**The first publish, once, by the owner.** Trusted publishing can only be set up on a
crate that already exists on crates.io, so sacdfs 0.4.0 is published by hand:

1. Tag v0.4.0 as above and let the release workflow finish. Its `crates-publish` job fails,
   saying sacdfs is not on crates.io yet; that is expected.
2. From a clean checkout of the tag: `cargo publish --locked`, with a crates.io token
   (scope `publish-new`, short expiry), entered with `cargo login` or `CARGO_REGISTRY_TOKEN`.
   Revoke the token afterwards.
3. On crates.io, the crate's Settings, "Trusted Publishing": add GitHub, repository
   `aclater/sacdfs`, workflow `release.yml`, no environment.
4. Re-run the failed `crates-publish` job: it finds 0.4.0 on crates.io and succeeds without
   publishing. From 0.4.1 on, the job publishes by itself.

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
- **Output versions move with the bytes (#89, #64).** The WAV files' audio is scarletbook-pcm's
  (#84), and scarletbook patch releases may change its bytes without changing its API. So a
  scarletbook bump, or any change, that alters the pinned PCM hashes in `tests/pcm.rs` must also
  bump `WAV_VERSION` in `src/ident.rs` (and the test's `GOLDEN_WAV_VERSION`), and the CHANGELOG
  records it as an output change. Likewise for the DSF files and `DSF_VERSION`, pinned by
  `golden_dsf_corpus` in `tests/corpus.rs`. The PCM pins cover 1, 2 and 6 channels at gains of
  -3, 0 and +6 dB (`golden_output_channels_and_gains`). The version
  moves each file's mtime and NFS generation, so players, taggers and rsync see the change.
- **Hostile input.** Every image is untrusted. Nothing an image contains may panic, hang or
  grow memory without bound; `clippy::unwrap_used`, `expect_used` and `panic` are warnings,
  and CI treats warnings as errors.
- **Tests.** New behaviour comes with a test. A test that can't run here says so with
  `common::skip!`, never a bare `return`, and a skip that a CI job may have is listed in
  `tests/expected-skips.json` with why it's legitimate; CI fails on any other change
  ([docs/development.md](docs/development.md#skipped-tests)). Tests on real discs run by hand with `scripts/real-discs.sh DIR` ([docs/development.md](docs/development.md#real-disc-tests)); they are a release step. Test images in the repository are synthetic: the corpus in
  `tests/corpus/` (CC0), never audio from a real disc. A new parser of anything from outside
  gets a fuzz target or joins one ([fuzz/README.md](fuzz/README.md)), and a crash a fuzzer finds
  becomes a test in `tests/fuzz_regressions.rs`.
- **Commits.** Small commits with a message that says why. Set your git identity in every clone before committing *or merging*: a merge made without one takes the machine's default, which can leak a private host name. CI's "no private names" check (`scripts/check-private-names.sh`) fails on private details in commit identities and tracked files. Actions in workflows are pinned
  by full commit SHA with a version comment.

- **CI speed.** A pull request's checks finish in about 4 min, and a new push cancels the superseded run (`concurrency`; main runs are never cancelled). Rust jobs restore a cargo cache that only main writes (`Swatinem/rust-cache`, `save-if`). Every required check runs on every pull request: none is path-filtered, because a skipped required check stays pending and blocks the merge. Keep new jobs short, and put slow, rarely-failing ones on main, tags or a schedule. The informational `benchmark` job (about 2 min, [docs/performance.md](docs/performance.md#the-ci-benchmark)) is path-filtered, because it is never required.
