# Security policy

## Supported versions

Security fixes go into the latest release and `main`. There are no maintained release
branches: if you're on an older release, the fix is to upgrade.

| Version | Supported |
| --- | --- |
| latest release ([releases](https://github.com/aclater/sacdfs/releases)) | yes |
| `main` | yes |
| anything older | no |

## Reporting a vulnerability

Please don't open a public issue, pull request or discussion for a vulnerability. Report it
privately through GitHub's private vulnerability reporting:

**<https://github.com/aclater/sacdfs/security/advisories/new>**

(or the **Report a vulnerability** button on the repository's
[Security tab](https://github.com/aclater/sacdfs/security)). Only the maintainer sees the
report.

Please include:

- the sacdfs version (`sacdfs --version`), your OS and kernel version, and how sacdfs was
  installed (installer, container, binary, built from source);
- the command line or the configuration;
- the input that triggers it, if there is one. Don't send copyrighted disc contents: a
  synthetic image, or the few sectors involved, is enough;
- what happens (a crash, a hang, memory or CPU use, data shown that shouldn't be, a file
  written where it shouldn't be) and, if you know, why.

If you aren't sure whether a problem is a security problem, report it privately anyway: a
private report can be made public later, and a public one can't be made private. Reports found
with a fuzzer, a scanner or an AI tool are welcome when they come with a way to reproduce the
problem.

If you can't use GitHub's private reporting, open an issue that says only that you have a
security report and need a private way to send it. Put no details in the issue.

## What to expect

sacdfs is maintained by one person, so these are targets, not guarantees:

- **acknowledgement** within 7 days;
- **assessment** (confirmed or not, and how severe) within 14 days;
- **a fix** for a confirmed issue within 90 days, usually much sooner, released as a new
  version with the fix noted in its release notes.

You are told when the report is confirmed or declined, when a fix is ready and when it is
released.

## Coordinated disclosure

Please keep the details private until a fixed release is out, or until 90 days after your
report, whichever comes first; if a fix needs longer, we'll agree a date with you. If the
problem is already public or is being exploited, say so in the report: the fix and the advisory
then go out as soon as they can.

Once it's fixed, the issue is published as a GitHub security advisory (with a CVE when it
warrants one), saying which versions are affected and which release fixes it, and crediting
you unless you'd rather not be named.

## Scope

sacdfs treats every disc image, and everything in it (TOCs, text, audio sectors), as
attacker-controlled. In scope:

- a crash, hang, or unbounded memory or CPU use of the mount caused by an image;
- reading data outside the image files under SOURCE, or showing one image's data as
  another's;
- anything that lets a user of the mount write to it or to the source;
- text from an image or from a tag lookup that reaches a shell or drives a terminal;
- a response to `sacdfs tags fetch` (from MusicBrainz, or from anyone on the network path)
  that does more than set tags;
- the installer, the container image and the service files: gaining more privileges than the
  documentation describes, or running code that didn't come from the release;
- a way round the checks on a release's files (see below).

Parsing and DST decoding happen in [scarletbook](https://github.com/aclater/scarletbook-rs);
a problem there is reported to that project, but a report here is welcome too.

Out of scope: `--allow-other` exposes the mount to every local user, by design; and a user
who can write to SOURCE controls what the mount shows.

## Checking a release

Every release file has a SHA-256 checksum and a signed build provenance attestation, and the
installer checks the checksum of what it downloads. To check a file yourself, see
[docs/install/binary.md](docs/install/binary.md) (archives),
[docs/install/README.md](docs/install/README.md) (the installer) and
[docs/deployment.md](docs/deployment.md) (the container image).

## How a report is handled

This is what the maintainer, and any contributor or agent working on sacdfs, does with a
vulnerability, whether it came from a report or was found while working on the code.

1. **Keep it private from the first moment.** Whoever finds it opens a draft advisory (the
   link above; a maintainer can use **Security**, **Advisories**, **New draft security
   advisory**). Nothing about it goes into an issue, a pull request, a discussion, a commit
   message, a branch name, the project board or the log of a public CI run. The issue scheme
   in [CONTRIBUTING.md](CONTRIBUTING.md) (milestones, labels, claiming) doesn't apply; the
   draft advisory is the record.
2. **Assess it.** Reproduce it, work out which versions are affected and how severe it is
   (the advisory's CVSS score), and tell the reporter. A problem that turns out to have no
   security impact is closed as an advisory and becomes a normal issue. The maintainer makes
   that call, not the finder.
3. **Fix it in private.** Work in the advisory's temporary private fork. GitHub Actions don't
   run there, so run the checks from CONTRIBUTING.md on your machine and note the results in
   the advisory. The fix comes with a test that fails without it; an input that triggers the
   problem joins the corpus or the fuzz regressions in the same change.
4. **If the cause is in scarletbook,** say so in the advisory and stop there: don't open
   anything against scarletbook-rs yourself. The maintainer takes it to that project as a
   private statement of the behaviour and the input, with no sacdfs code (the clean-room
   rule). sacdfs's pin moves once scarletbook's fixed release is out.
5. **Release.** Merge the private fork, tag and release as usual. The changelog has a
   "Security" entry that names the advisory and says what to upgrade to, without a recipe
   for the attack.
6. **Publish the advisory** once the release is out, with the affected and fixed versions, a
   CVE if it warrants one (requested through the advisory) and credit to the reporter.
   Follow-up hardening can then be discussed in public issues.
