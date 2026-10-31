# Security policy

## Supported versions

Security fixes go into the latest release and `main`. There are no maintained release
branches.

| Version | Supported |
| --- | --- |
| latest release | yes |
| `main` | yes |
| anything older | no |

## Reporting a vulnerability

Please don't open a public issue, pull request or discussion for a vulnerability. Report it
privately through GitHub's private vulnerability reporting:

**<https://github.com/aclater/sacdfs/security/advisories/new>**

(or the **Report a vulnerability** button on the repository's
[Security tab](https://github.com/aclater/sacdfs/security)). Only the maintainer sees the
report.

Please include the sacdfs version (`sacdfs --version`), your OS and kernel version, the
command line, what happens and, if possible, the input that triggers it. Don't send
copyrighted disc contents: a synthetic image, or the few sectors involved, is enough.

## What to expect

sacdfs is maintained by one person, so these are targets, not guarantees: an
acknowledgement within 7 days, an assessment within 14 days, and a fix for a confirmed issue
within 90 days, released as a new version and published as a GitHub security advisory,
crediting you unless you'd rather not be named. Please keep the details private until then.

## Scope

sacdfs treats every disc image, and everything in it (TOCs, text, audio sectors), as
attacker-controlled. In scope:

- a crash, hang, or unbounded memory or CPU use of the mount caused by an image;
- reading data outside the image files under SOURCE, or showing one image's data as
  another's;
- anything that lets a user of the mount write to it or to the source.

Parsing and DST decoding happen in [scarletbook](https://github.com/aclater/scarletbook-rs);
a problem there is reported to that project, but a report here is welcome too.

Out of scope: `--allow-other` exposes the mount to every local user, by design; and a user
who can write to SOURCE controls what the mount shows.
