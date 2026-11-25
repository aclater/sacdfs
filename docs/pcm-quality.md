[sacdfs](../README.md) › [Documentation](README.md) › PCM quality

# PCM quality

How good the DSD-to-PCM conversion behind sacdfs's WAV files is, and how that was measured.
From #84 to 0.8.0 the converter was scarletbook-rs's `scarletbook-pcm` (0.5.3 to 0.7.0); since
0.9.0 it is the `dsd2pcm` crate, whose `fast` filter gives scarletbook-pcm 0.7.0's output byte
for byte and whose `precise` filter is the default. `sacdfs::pcm` maps sacdfs's options to it
and feeds it the engine's frames.

The targets and results below are those of the `fast`
filter, which `tests/pcm.rs` asserts; the figures for both filters, measured by dsd-measure,
are in [precise and fast](#precise-and-fast). The output decisions (rates,
cutoffs, gain) are the maintainer's, with their reasons, in #11 and #5; the decision to use
scarletbook-pcm until 0.8.0, and the measurements it rested on, are in #84. Throughput is in
[performance.md](performance.md#converter-throughput), the design in
[design.md](design.md#pcm-conversion).

## Precise and fast

`--pcm-filter` selects the decimation filter: `precise` (the default since 0.9.0, except in the 32-bit x86 build, where `fast` is the default because of the CPUs it runs on) or `fast`,
the filter of 0.8.0 and earlier. Measured with dsd-measure on the same synthetic DSD64 signals
(1 kHz tone at 0 dB SACD, 24-bit output, cutoff 40 kHz), against 0.8.0:

| Rate (kHz) | Filter | SINAD (dB) | Noise, 20 Hz-20 kHz (dBFS) | Alias rejection (dB) |
|---:|---|---:|---:|---:|
| 88.2 | 0.8.0 and `fast` | 121.98 | -126.63 | -111.83 |
| 88.2 | `precise` | 141.91 | -148.93 | -159.28 |
| 176.4 | 0.8.0 and `fast` | 122.14 | -126.65 | -138.38 |
| 176.4 | `precise` | 143.79 | -151.45 | -165.11 |
| 352.8 | 0.8.0 and `fast` | 122.14 | -126.65 | not measurable |
| 352.8 | `precise` | 144.89 | -153.53 | not measurable |

The full table, with the wide filter, the null tests, the versions and commits, and the
method is in [quality/0.9.0.md](quality/0.9.0.md). The converter takes about 1.8 times
the CPU time with `precise`; against 0.8.0 on an i9-13900H, a whole-track WAV read with `precise`
was 12 % slower on DST discs and 38 % slower on plain DSD ([performance](performance.md#wav-throughput-by-filter)).
Both are deterministic: the same release, input and options give the same bytes on every CPU.

## What changed in 0.9.0

- **WAV samples.** With the default `precise` filter every WAV file's samples differ from
  0.8.0's; `--pcm-filter fast` gives 0.8.0's samples back, byte for byte. Checked on the test
  corpus (136 images, 910 files in a configuration): with `--pcm-filter fast` all 455 WAV
  files equal the ones of the build before, in the default, `--pcm-rate 88200`, `352800`,
  `--flatten`, and the cutoff and gain variants; with the default filter, 399 to 401 of the
  455 differ and the others are equal.
- **WAV header and channel mask.** Unchanged. sacdfs writes the WAV header itself, with the
  channel mask from the disc's loudspeaker layout (see [layout](layout.md)); dsd2pcm 0.2's
  own change of its WAV channel mask (by DSF channel type) does not reach it. The `fmt `
  chunk of every corpus file, multichannel ones included, equals the one dsd2pcm 0.2's own
  command-line converter writes for the same track, and equals the one of 0.8.0.
- **Against dsd2pcm.** Converting a track's DSF file (as the mount shows it) with dsd2pcm 0.2's
  command-line converter, with the same filter, rate 176.4 kHz and cutoff 40 kHz, gives the
  mount's WAV samples byte for byte, except within one SACD frame of a track's two ends:
  there the mount's samples depend on the neighbouring tracks' audio (gapless), and the
  file converter's on silence. Over the corpus: 455 WAV files per filter, 66 identical, 389
  differing only within that frame, none elsewhere.
- **DSF, DFF and ISO output.** Byte-identical to 0.8.0's, with the same times: the corpus in every configuration, and the real-disc set.
- **Output version.** `WAV_VERSION` moves from 5 to 6, so WAV files show a modification time 7 hours later
  and new NFS generations, as for every change of the WAV bytes ([design](design.md#file-identity-and-nfs-export)).
  The filter is one more part of the options that move the time (`fast` shows a time 7 hours
  later than `precise`). DSF files' and folders' times do not move: the output-version step is
  unchanged, and a test pins the DSF times to 0.8.0's.
- The pinned hashes in `tests/pcm.rs` stay for `fast`; `precise` has its own.

## Why scarletbook-pcm (#84)

The maintainer's decision (2026-11-01, DECISIONS.md): keep scarletbook-rs's clean converter rather
than adopting an existing code base, measure it against sacdfs's own, and switch if it is
within the 15 % offload leeway and meets sacdfs's targets.

- **First measurement (0.5.2):**
  - it was 33 to 46 % slower with the default filter;
  - it missed three targets: the wide filter's stopband edge at 176.4 kHz, the 30 kHz filter's stopband edge, and DSD silence as exact zero.
  - The gaps went upstream as scarletbook-rs#170 to #173.
- **0.5.3 closes all four:**
  - its speed is within 5 % (generic build) and 14 % (x86-64-v3 build) with the default filter, and faster with the others;
  - it meets every target below.
- **So sacdfs's own converter was deleted.** That converter was ported from pysacd, about 1 600 lines.
- *Trade-off:* scarletbook's passband ripple (±0.004 dB) and noise floor (-134 dBFS) are a little worse than those of sacdfs's old converter (±0.00003 dB, -138 dBFS). Both are far inside the targets (the converter's measured figures are listed under Targets and results); no listening test was run. In return the code is maintained once, for sacdfs and pysacd. The WAV files' bytes changed, so their output version moved (`WAV_VERSION` 3: new mtimes and generations, [design](design.md#file-identity-and-nfs-export)).

## Targets and results

Measured by `tests/pcm.rs` on every rate and cutoff, asserted in CI. The figures are printed by
`cargo test --release --test pcm -- --nocapture`.

| Target (#5) | Result |
|---|---|
| Ripple ≤ ±0.01 dB to 20 kHz | ±0.0039 dB (±0.0040 dB wide at 352.8 kHz) |
| Ripple ≤ ±0.1 dB to the cutoff | ±0.0039 dB to 40 kHz, ±0.0037 dB to 30 kHz, ±0.0042 dB to 80 kHz |
| Stopband ≥ 105 dB from 48 kHz (40 kHz filter) | 110.8 to 111.1 dB |
| ... from 36 kHz (30 kHz filter) | 109.6 to 110.2 dB |
| ... wide: from 88.2 kHz (176.4 kHz out, the Nyquist frequency), 100 kHz (352.8 kHz out) | 109.1 to 109.7 dB, 110.2 to 111.0 dB |
| Out-of-band tones (60, 92, 100, 120, 200 kHz at -12 dBFS) rejected | 108.9 to 178 dB, or down to the DSD's own content where the alias lands |
| Level: 100 % modulation = full scale, 0 dB gain | 1 kHz at -6 dBFS: within 0.004 dB (the ripple); all-ones DSD = +1.0, clamped and counted |
| DC | -147 dBFS or lower on a -6 dBFS tone |
| DSD silence = exact zero, area edges included | every rate and cutoff, with and without neighbours |
| THD+N of 1 kHz at -6 dBFS limited by the source | -91.50 dB (-91.49 wide) against the DSD source's -91.51 dB (in 20 Hz-20 kHz) |
| Time alignment: offset 0 | phase of the fitted 1 kHz tone: below 0.0001 samples; impulse response symmetric about lag 0 |
| Clipping clamped and counted | +9 dB on a -6 dBFS tone: every clipped sample at the rails and counted, per frame of a run |
| Determinism | every frame alone = whole-area conversion, 1 to 6 channels, every rate and cutoff, gains 0 and 9 dB; proptest of random frame windows and runs; pinned output hashes |

The noise floor with modulated silence is -133.8 dBFS (40 and 30 kHz filters) and -134.7 dBFS
(wide), in 20 Hz-20 kHz, against the source's -140.2. sacdfs's old converter measured
-137.8 dBFS; the difference is the stopband's last few dB folding DSD's MHz noise into the band.

### How it was measured

- **The test signal.** A 5th-order, 1-bit sigma-delta modulator in test code (`tests/pcm.rs`,
  `Modulator`), of the class SACD uses (E. Janssen's thesis, *Look-ahead sigma-delta modulation
  and its application to Super Audio CD*, TU Eindhoven, 2010, doi:10.6100/IR691188).
  - Its noise transfer function has zeros at DC, 10.8 and 18.1 kHz, Butterworth poles with a 112 kHz corner, and a largest gain of 1.5.
  - It is in error-feedback form, so its signal transfer function is exactly 1.
  - It is a plain modulator, not a look-ahead one: THD+N -91.5 dB at -6 dBFS.
  - Impulses come from flipping one bit.
- **Limit cycles of the test modulator** (checked in #84). The modulator's own bits were examined, ideally decimated, for:
  - silence;
  - 13 DC values from 1e-5 to 1e-2;
  - 1 kHz tones from -80 to -6 dBFS;
  - each with and without TPDF dither at the modulator's input.

  Findings:
  - **No limit cycles.** No exactly periodic bitstream up to 65 536 bits (Reiss and Sandler, AES 116th Conv., 2004).
  - **Idle tones with DC only.** Constant DC inputs give non-periodic idle tones at DC × fs, as Perez Gonzalez and Reiss predict (AES 122nd Conv., 2007).
  - **No tones with silence or sines.**
  - **Noise modulation.** The in-band noise is lowest for exact silence (-144.6 dBFS) and about -130 dBFS for any non-zero input (Reiss and Sandler, AES 115th Conv., 2003).
  - **Effect on the measurements here.** None uses a DC input, so no dither is added.
- **Response.** The composite impulse response at 2.8224 MHz, from the converter's 24-bit output:
  - one flipped bit's difference, averaged over 256 faint modulated backgrounds;
  - at +24 dB gain (+12 dB for the wide filter), which lifts it above the 24-bit rounding;
  - for every phase of an output sample;
  - then a 2^20-point FFT (2.7 Hz bins); the stopband is the worst point from the stop frequency to 1.4112 MHz.
  - The method reproduced sacdfs's old converter's exact (float) response to within 1 dB down to its 115 dB stopband.
- **Spectra.** One 2^20-point FFT under a 4-term Blackman-Harris window, in steady state. THD+N is the power in 20 Hz-20 kHz outside the tone's ±8 bins, against the tone. The same analysis of the DSD bitstream gives the source's THD+N.

## Against the converter it replaced (#84)

Both converters were run on the same DSD: synthetic signals, and 60 s from the middle of every area of the five local test discs (7 areas, 2 to 6 channels).

**Residual (old minus new), in 20 Hz-20 kHz:**

| Filter | Real discs | Against the signal | 1 kHz -6 dBFS |
|---|---|---|---|
| 40 kHz | -130 to -95 dBFS | -86 to -72 dB | -79.5 dBFS (a 0.0026 dB level difference) |
| 30 kHz | -130 to -95 dBFS | -86 to -72 dB | -77.1 dBFS (0.0034 dB) |
| wide | -124 to -94 dBFS | -88 to -71 dB | -78.8 dBFS (0.0027 dB) |

- **In the audio band:** the residual is the new converter's ±0.004 dB ripple, more than 70 dB below the signal.
- **Above 20 kHz:** the transition bands differ (for example 40-48 kHz), where DSD's ultrasonic noise is loud. So the full-band residual is larger: -92 to -82 dBFS with the 40 kHz filter, -67 to -42 dBFS with the wide one.
- **Silent channels** of a real 5-channel area are exactly zero in both.
- **Clips:** none with either.

## The filters

scarletbook-pcm's own documentation describes its designs, which dsd2pcm ported. They are Parks-McClellan equiripple, linear phase, odd length, and exact integer arithmetic. It also publishes its measured response, and records its sources in scarletbook-rs's CLEANROOM.md. sacdfs treats it as a black box: these pages describe what was measured from outside.

| sacdfs option | dsd2pcm (as scarletbook-pcm) | Pass band | Stopband from |
|---|---|---|---|
| `--pcm-cutoff 40` (default) | `Cutoff::Standard` | 40 kHz | 48 kHz |
| `--pcm-cutoff 30` | `Cutoff::Low` | 30 kHz | 36 kHz |
| `--pcm-cutoff wide` (176.4 and 352.8 kHz only) | `Cutoff::Wide` | 80 kHz | 88.2 kHz at 176.4 kHz (its Nyquist frequency), 96 kHz at 352.8 kHz |

At 88.2 kHz the band ends at 44.1 kHz, so the wide filter isn't offered there. Maintainer decision (2026-10-31, #11): the wide option follows the Philips blind tests, which found 80-100 kHz filters undetectable and a 50 kHz one detectable. 40 kHz stays the default.

## See also

- [Performance: converter throughput](performance.md#converter-throughput)
- [Design: PCM conversion](design.md#pcm-conversion)
- [Options](options.md)

Part of the [DSD Toolbox](https://github.com/dsd-toolbox/dsd-toolbox).
