# Contributing to sacdfs

Thanks for helping. Bug reports, test images and pull requests are welcome.

## Reporting a bug

Open an [issue](https://github.com/aclater/sacdfs/issues) with the command line, what you
expected, what happened, and a log: run in the foreground with `sacdfs -f -vv ...`. Don't
attach copyrighted disc images. **Security problems** go through a private report instead:
see [SECURITY.md](SECURITY.md).

That applies to contributors and agents too. If you find a vulnerability while working on the
code, don't open an issue or a pull request for it and don't describe it in a commit message:
open a draft advisory and follow
[How a report is handled](SECURITY.md#how-a-report-is-handled).

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

   The other [code acceptance rules](#code-acceptance-rules) have their own checks; for
   changes outside Rust, `git ls-files -z '*.sh' deploy/sacdfs-ctl | xargs -0 shellcheck`,
   `ruff check` and `sh .github/scripts/check-copies.sh`.

   The mount tests need `/dev/fuse` and `fusermount3` (package `fuse3`); without
   `SACDFS_FUSE_TESTS=1` they are skipped where FUSE is missing, each with a `SKIPPED` line in
   the output. In a container, pass
   `--device /dev/fuse --cap-add SYS_ADMIN`. `tests/no_growth.rs` checks that mount cycles
   leave no heap behind. For the long checks (the soak, heap profiles, memcheck), see
   [docs/development.md](docs/development.md).
4. Update the documentation with the code: README.md, docs/options.md and docs/user-guide.md for anything a
   user sees (the options table in docs/options.md must match `sacdfs --help`; CI checks it with
   `.github/scripts/check-readme-options.py`; a changed option also changes the manual pages,
   which are generated: `cargo run --features man,nfs -- --generate-man-pages man`, and CI
   checks them with `.github/scripts/check-man.py`), docs/design.md when the design changes, with
   the reason for each decision, docs/performance.md with new measurements, and CHANGELOG.md
   under "Unreleased".

## Code acceptance rules

A pull request is merged only when it keeps these rules. Each is checked in CI.

| Rule | Check |
|---|---|
| Every `unsafe` block and `unsafe impl` has a `// SAFETY:` comment. | `undocumented_unsafe_blocks = "deny"` in Cargo.toml and winfsp/Cargo.toml (clippy jobs) |
| Every long option of the mount command is an `-o` option or listed in `NOT_MOUNT_OPTIONS`. | unit test `the_lists_match_the_command_line` (ci, tests) |
| The manual pages in man/ are current, and `sacdfs(1)` lists the options of docs/options.md. | `.github/scripts/check-man.py` (ci, readme options) |
| Every shell script passes ShellCheck in the dialect it declares; a disable says why. | `shellcheck` over `git ls-files '*.sh' deploy/sacdfs-ctl` (ci, fmt + clippy) |
| Python passes `ruff check` at its default rules (ruff.toml). | ruff 0.17.3, pinned by hash in .github/requirements/ruff.txt (ci, fmt + clippy) |
| Every Cargo.lock is built `--locked` and checked by cargo-deny. | deny.yml (core, fuzz), windows.yml (winfsp); fuzz/run.sh stops when fuzz/Cargo.lock is stale |
| winfsp/ is formatted. | `cargo fmt --manifest-path winfsp/Cargo.toml --check` (ci, fmt + clippy) |
| The minimum Rust version builds the `nfs` feature too. | `cargo check --all-targets --locked --features nfs` (msrv) |
| The docs build without warnings, with and without `nfs`. | `RUSTDOCFLAGS=-D warnings cargo doc --no-deps --locked [--features nfs]` (ci, sacdfs-nfs) |
| Copies that must be equal are equal: one `RUST_STABLE`; `entry_unsafe` and `path_unsafe` identical in install.sh and sacdfs-ctl. | .github/scripts/check-copies.sh (ci, no private names) |
| Shipped container definitions drop all capabilities, set no-new-privileges, and are never privileged. | .github/scripts/check-copies.sh (ci, no private names) |
| No function over 200 code lines. | `too_many_lines`, threshold in clippy.toml (clippy jobs) |
| The core's test code is linted on Windows. | `cargo clippy --lib --profile test --locked -- -D warnings` (windows) |
| No lossy `as` cast in verify's untrusted-file readers. | `#![warn(clippy::cast_possible_truncation)]` in src/verify/container.rs, src/verify/id3.rs and src/tags_cli.rs |
| No needless collect, large enum variant, large stack array, large future or large value passed by value. | `needless_collect`, `large_enum_variant`, `large_stack_arrays`, `large_futures`, `large_types_passed_by_value` (clippy jobs) |

Clippy warnings fail CI (`-D warnings`). Silence a lint at one place with
`#[expect(lint, reason = "...")]`.

## Issues: milestones, labels and claiming

Every open issue says which release it is for, how important it is, where it stands, and
what kind of agent should work on it. The issue list then shows what's up without opening
each issue.

- **Milestone:** the release it is planned for (`0.2.0`, `0.3.0`, ...), `Docs` for
  documentation-only work (it merges to `main` without a release), or `Later`. Umbrella
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

The release steps, the repair runs for an earlier release and crates.io publishing are in
[docs/release.md](docs/release.md).

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

- **CI speed.** A pull request's required checks finish in about 6 min, and a new push cancels the superseded run (`concurrency`; main runs are never cancelled). Rust jobs restore a cargo cache that only main writes (`Swatinem/rust-cache`, `save-if`); a job that main never runs reads another job's cache with `shared-key` and saves none, so pull requests don't fill the cache space. A pull request that changes only Markdown files (other than `docs/options.md`) skips the build and test jobs. A required check must still report, and a workflow skipped by a `paths` filter leaves its checks pending, so the workflows with required checks call `.github/workflows/changes.yml` and skip the jobs by `if` (a skipped job reports success), or the steps for a matrix job, whose skipped job would not expand its name. The same `changes.yml` also answers whether a pull request touches Rust or CI files (`rust`: coverage, `nfs export`), the container image's files (`container`) or `.github` (`workflows`: CodeQL's `actions` analysis); a pull request that touches none of them reports those checks as skipped, which counts as success. Concurrency groups are per workflow and ref for pull requests, which a new push cancels, and per run for main, tags and schedules, which are never cancelled. The macOS and Windows workflows (`core-portable`, `sacdfs-nfs`, `windows`) run on a pull request only when platform-specific code changes, and always on main pushes, release tags and a nightly schedule, as do the container and `nfs` checks. Only workflows with no required check use `paths` or `paths-ignore`, such as the informational `benchmark` job (about 2 min, [docs/performance.md](docs/performance.md#the-ci-benchmark)). Keep new jobs short, and put slow, rarely-failing ones on main, tags or a schedule.
