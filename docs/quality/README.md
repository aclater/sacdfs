[sacdfs](../../README.md) › [Documentation](../README.md) › Release quality comparison

# Release quality comparison

Every release from 0.9.0 on publishes `docs/quality/<version>.md`, which measures the WAV view
of that release against the previous one.

- [0.9.0](0.9.0.md): `precise` becomes the default filter

The WAV view is measured with [dsd-measure](https://github.com/dsd-toolbox/dsd-measure) 0.1.2
(commit 3eaec2d, pinned in [suites/sacdfs.toml](../../suites/sacdfs.toml)): the same synthetic
DSD64 signals go through the converter of the new build and of the previous release's tag, at
every output rate, and the table gives SINAD and THD+N of a 1 kHz tone at 0 dB SACD, the noise
floor from 20 Hz to 20 kHz, alias rejection (88.2 and 176.4 kHz only) and a null test against
the previous release on the suite's multitone signal. The converter is the code behind the WAV
files (`sacdfs::pcm`, through the example `dsf_to_wav`): the mount adds the header, tags and
frame cache and leaves the samples as they are, which the release test plan checks against the
mount. DSF, DFF and ISO output is not measured: it is passed through bit for bit, which the
test suite checks. The figures are what the suite reads; they make no claim about audibility.
A change of 0.01 dB or more, or a null that is no longer identical, is flagged, and a
regression blocks the release unless it is accepted and recorded in that version's page and its
CHANGELOG entry.

## See also

- [PCM quality](../pcm-quality.md): the targets and how they are tested
- [Performance](../performance.md): throughput by release

Part of the [DSD Toolbox](https://github.com/dsd-toolbox/dsd-toolbox).
