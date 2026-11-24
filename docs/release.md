[sacdfs](../README.md) › [Documentation](README.md) › Releasing

# Releasing

How a sacdfs release is made, from the checks before the tag to the assets after it. The
process shared by the DSD Toolbox projects is in its
[CI/CD guide](https://github.com/dsd-toolbox/dsd-toolbox/blob/main/docs/ci-cd.md).

## Steps

The owner tags releases; everything after the tag is automatic (`.github/workflows/release.yml`).
Version tags (`v*`) are immutable, so a tag that went wrong is followed by a new version,
never moved.

1. **Before:** `main` is green, the scarletbook pin is the one meant for the release, and
   the docs describe the release (README, docs/).
2. **Test:** run the [release test plan](test-plan.md) on a release candidate; its exit criteria
   decide whether the final version is tagged.
3. **Dry run:** Actions, "release", "Run workflow" on `main` (or
   `gh workflow run release.yml -R dsd-toolbox/sacdfs --ref main`). It builds and tests every
   archive, package and the image, runs the container share test, and publishes nothing; its summary
   lists the assets and checksums a tag would publish. Pull requests that change the release
   files run it too.
4. **Release pull request:** bump `version` in `Cargo.toml` (and `Cargo.lock`, with
   `cargo update -p sacdfs --offline`), and rename `## [Unreleased]` in CHANGELOG.md to
   `## [X.Y.Z] - YYYY-MM-DD`, with a new empty `## [Unreleased]` above it. That section
   becomes the release notes. At the end of the file, point `[Unreleased]` at
   `compare/vX.Y.Z...HEAD` and add `[X.Y.Z]: .../compare/v<previous>...vX.Y.Z`, so every
   heading links to its changes. Date the heading with the day the tag will be pushed (UTC).
   In the same pull request, point the install instructions at the new version: the guides
   in `docs/install/`, `deploy/nas-compose.yaml`, the Unraid template
   (`deploy/unraid/sacdfs.xml`, `<Repository>`) and the TrueNAS catalog app
   (`deploy/truenas/sacdfs/ix_values.yaml`, `images.image.tag`, and `app_version` in
   `app.yaml`). The required check "docs version drift" (`scripts/check-version-drift.py`)
   fails while any of them names another version. Merge it once its checks pass.
5. **Tag** the merge commit on `main`: `git tag -a vX.Y.Z -m "sacdfs X.Y.Z" && git push origin vX.Y.Z`.
   The workflow stops before building anything if the tag isn't `v` plus Cargo.toml's
   version, if CHANGELOG.md has no section for it, or if the tagged commit is on neither `main`
   nor a `release/*` branch (`.github/scripts/check-tag-reachable.sh`). A patch release cut
   from an earlier tag instead of `main` is pushed as a branch `release/X.Y.Z` first and tagged
   there.
   **Signing:** the signing jobs read their secrets from the environment `release` (only `v*`
   tags may use it, and its reviewer approves each run): `macos-sign` signs and notarises both
   macOS archives (`MACOS_CERT_P12_BASE64`, `MACOS_CERT_PASSWORD`, `MACOS_SIGN_IDENTITY`,
   `MACOS_NOTARY_KEY_ID`, `MACOS_NOTARY_ISSUER`, `MACOS_NOTARY_KEY_P8_BASE64`), and, when the
   repository variable `WINDOWS_SIGNING` is `required`, `windows-sign` signs `sacdfs-win.exe`
   through SSL.com eSigner (`ES_USERNAME`, `ES_PASSWORD`, `ES_CREDENTIAL_ID`,
   `ES_TOTP_SECRET`). The run waits for that approval; nothing else does. A missing secret
   stops the release before anything is published. `WINDOWS_SIGNING` is `off` (the zip is
   published unsigned) until the certificate is in place; any value other than `off` or
   `required` stops the run in preflight. Dry runs never sign: their macOS archives are named
   `-unsigned`.
6. **After:** the release has its archives, packages and `install.sh`, each with `.sha256` and
   `.intoto.jsonl`, and `SHA256SUMS`; `ghcr.io/aclater/sacdfs:X.Y.Z` and `:latest` exist; check one asset with
   `gh attestation verify`; the workflow's last image step has verified the image index, each
   architecture's image by its digest and each `-<arch>` tag (#379). A version with a hyphen
   (`0.2.0-rc.1`) is marked as a pre-release and doesn't move `:latest`.
7. **TrueNAS catalog:** the catalog's own update pull requests take the release. Once it is
   published there, set `version` in `deploy/truenas/sacdfs/app.yaml` to the number the
   catalog published it under (the catalog numbers its app itself;
   [store-apps.md](install/store-apps.md#checked-with-the-stores-own-tooling)), and pin the
   image digest in `ix_values.yaml`.
8. **Roll-up:** update [RELEASES.md](https://github.com/dsd-toolbox/dsd-toolbox/blob/main/RELEASES.md)
   in dsd-toolbox/dsd-toolbox (the minor version's summary and release list).

## macOS archives of an earlier release

Every release run builds and tests both macOS
archives; the release gets them only when the repository variable `SACDFS_MACOS_RELEASE` is
`true`. To add them later to a release whose run held them back, while the run's artifacts
last (7 days): `gh workflow run release.yml -R dsd-toolbox/sacdfs --ref main -f macos_run=RUN_ID
-f tag=vX.Y.Z`. The job `macos-publish` checks that the run is the tag's successful release
run and that the release has no macOS assets yet, verifies the checksums, attests the two
archives, uploads them with their `.sha256` and `.intoto.jsonl`, appends their two lines to
`SHA256SUMS` (the other lines stay byte for byte) and adds the macOS lines to the notes. It
builds nothing and leaves the tag and the other assets alone. It needs
docs/install/macos.md on `main` to describe the archives, since the notes link to it. It runs
alone in the concurrency group `release-assets-<tag>`, and stops before replacing
`SHA256SUMS` if the file changed since it read it. A run that
fails after its first upload leaves the release half done, and a new run stops at "already has
macOS assets": delete the macOS assets it uploaded (`gh release delete-asset`), put back
`SHA256SUMS` from the run's log if it was replaced, and run it again.

## Linux packages of an earlier release (#460)

Every release run packs and tests the `.rpm`
and `.deb` packages and the source RPM (`packaging/`). To add them to a release made before
that: `gh workflow run release.yml -R dsd-toolbox/sacdfs --ref main -f packages_tag=vX.Y.Z`. The run
checks the release's x86_64 and aarch64 archives against `SHA256SUMS` and their attestations
(releases up to 0.7.2 were attested under the `aclater` account, which GitHub does not move with a
repository, so those are verified with `--owner aclater` and that account's workflow as the signer),
packs them and the tag's source, runs the package and SRPM tests, then (job
`packages-publish`) attests the five files, uploads them with their `.sha256` and
`.intoto.jsonl`, appends their lines to `SHA256SUMS` (the other lines stay byte for byte) and
adds the package lines to the notes. It builds no binary and leaves the tag and the other
assets alone. A run that fails after its first upload leaves the release half done, and a new
run stops at "already has packages": delete the package assets it uploaded
(`gh release delete-asset`), put back `SHA256SUMS` from the run's log if it was replaced, and
run it again.

## crates.io (#155)

On a tag of a final version (no hyphen, so not `0.4.0-rc.1`), the
`crates-publish` job runs after the GitHub release: it asks the crates.io API whether that
version is there already, and if so logs it and stops; otherwise it publishes with
crates.io trusted publishing (`rust-lang/crates-io-auth-action`, no stored token). The crate
holds only what `cargo install` and `cargo build` need (`include` in Cargo.toml): check
with `cargo package --list` when adding files that the build reads. Pre-releases are not
published to crates.io.

## The first publish, once, by the owner

Trusted publishing can only be set up on a
crate that already exists on crates.io, so sacdfs 0.4.0 is published by hand:

1. Tag v0.4.0 as above and let the release workflow finish. Its `crates-publish` job fails,
   saying sacdfs is not on crates.io yet; that is expected.
2. From a clean checkout of the tag: `cargo publish --locked`, with a crates.io token
   (scope `publish-new`, short expiry), entered with `cargo login` or `CARGO_REGISTRY_TOKEN`.
   Revoke the token afterwards.
3. On crates.io, the crate's Settings, "Trusted Publishing": add GitHub, repository
   `dsd-toolbox/sacdfs`, workflow `release.yml`, no environment.
4. Re-run the failed `crates-publish` job: it finds 0.4.0 on crates.io and succeeds without
   publishing. From 0.4.1 on, the job publishes by itself.

<!-- docs-audit:begin -->
## After the release: run the docs audit

After the release: run the docs audit. Do it once the tag, the packages and the release notes are public, and before the release is announced.

1. Run the `docs-audit` workflow from the Actions tab of the repository (Run workflow). It runs the docs lint, the link check with external URLs and the docs facts check, and fails if any of them does.
2. Open the checklist it prints: every page that mentions a version, a command-line option or a platform, to compare with the release just published. Fix each mismatch in a docs-only change, or write down why it is right.
3. Record the result in the release notes or the release checklist: the workflow run, and how many checklist items were fixed.

Every project also runs this audit each month (the `docs-monthly` workflow in the DSD Toolbox repository), so a missed audit shows up within a month.
<!-- docs-audit:end -->

## See also

- [Release test plan](test-plan.md)
- [Changelog](../CHANGELOG.md)
- [Contributing](../CONTRIBUTING.md)
- [Install guide](install/README.md)

Part of the [DSD Toolbox](https://github.com/dsd-toolbox/dsd-toolbox).
