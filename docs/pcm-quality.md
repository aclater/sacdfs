# PCM quality

How good sacdfs's DSD-to-PCM converter (`sacdfs::pcm`, phase 1 of #11) is, how that was
measured, and why the filters are what they are. The decisions about the output (rates,
cutoffs, gain) were made by the owner, with their reasons, in #11 and #5; this page records
how they were met. Throughput is in [performance.md](performance.md#pcm-conversion), the
design of the converter in [design.md](design.md#pcm-conversion-phase-1-the-converter).

## Targets and results

Measured by `tests/pcm.rs` and `src/pcm.rs`'s unit tests (asserted in CI, figures printed
with `cargo test --test pcm -- --nocapture`), on every rate and cutoff unless noted.

| Target (#5) | Result |
|---|---|
| Ripple ≤ ±0.01 dB to 20 kHz | ±0.00001 dB, every configuration |
| Ripple ≤ ±0.1 dB to the cutoff | ±0.00002 dB to 40 kHz, ±0.00003 dB to 30 kHz and to 80 kHz |
| Stopband ≥ 105 dB from 48 kHz (40 kHz filter) | 114.9 dB (worst at 48.0 kHz) |
| ... from 36 kHz (30 kHz filter) | 115.4 dB (worst at 36.0 kHz) |
| ... wide: from 88.2 kHz (176.4 kHz out), 100 kHz (352.8 kHz out) | 115.5 dB, 116.4 dB |
| Out-of-band tones (60, 100, 120, 200 kHz at -12 dBFS) rejected | 110 to 233 dB, or down to the DSD's own content at the alias frequency (below) |
| Level: 100 % modulation = full scale, 0 dB gain | 1 kHz at -6 dBFS: +0.00000 dB; all-ones DSD = +1.0 exactly |
| DC | -149 dBFS or lower on a -6 dBFS tone (wide filter), -174 dBFS or lower otherwise; DSD silence = exact zero |
| THD+N of 1 kHz at -6 dBFS limited by the source | -91.51 dB, equal to the DSD source's own -91.51 dB (in 20 Hz-20 kHz), every configuration |
| Converter adds nothing above the 24-bit step | arithmetic error against a plain f64 evaluation: below 0.00001 of a step; what remains is the 24-bit rounding itself |
| Time alignment: offset 0 | phase of the fitted 1 kHz tone: offset below 0.0001 samples; impulse response symmetric about lag 0 (to 1e-7) |
| Clipping clamped and counted | +9 dB on a -6 dBFS tone: 4 679 of 9 408 samples clipped, all at the rails, all counted |
| Determinism | every frame alone = whole-area conversion, 1 to 6 channels, every rate and cutoff, gains 0 and 9 dB; proptest of random frame windows; pinned output hashes identical on SSE2, AVX2 and AVX-512 builds and on CI |

A full-scale step (all zeros to all ones, -1.0 to +1.0) overshoots by 8.9 % of the step with
the 40 kHz and wide filters (peak 1.178 of full scale) and 5.7 to 8.8 % with the 30 kHz
filter: that is the headroom a full-scale square wave would need, and it clips, counted.
Programme on SACD peaks around -6 to -3 dBFS (50 % modulation is the reference level), so
no clips at 0 dB: none on the local discs.

### How it was measured

- **The test signal.** A 5th-order, 1-bit sigma-delta modulator in test code
  (`tests/pcm.rs`, `Modulator`), of the class SACD uses: E. Janssen's thesis, *Look-ahead
  sigma-delta modulation and its application to Super Audio CD* (TU Eindhoven, 2010,
  doi:10.6100/IR691188), uses a 5th-order loop filter from a Butterworth prototype with a
  corner near 100 kHz and resonators at 12 and 20 kHz as its standard example. Ours has the
  same order and ingredients, designed as a noise transfer function (zeros at DC, 10.8 and
  18.1 kHz; Butterworth poles with a 112 kHz corner; largest NTF gain 1.5) in error-feedback
  form, whose signal transfer function is exactly 1. It is a plain modulator, not a
  look-ahead one, so its noise and distortion (THD+N -91.5 dB at -6 dBFS) are higher than a
  mastering modulator's. It makes sines, steps and silence; impulses come from flipping one
  bit of silent DSD (the converter is linear in the ±1 bits).
- **Exact response.** The composite impulse response at 2.8224 MHz is measured from the
  converter itself, one flipped bit per phase of an output sample; ripple and stopband are
  its spectrum, every 25 Hz in the pass band and every 50 Hz from the stop frequency to
  1.4112 MHz. It includes everything the converter does: the integer tables, f64 sums, all
  stages.
- **Spectra.** One 2^20-point FFT (0.37 s of audio, 2.7 Hz bins) under a 4-term
  Blackman-Harris window, in steady state. THD+N is the power in 20 Hz-20 kHz outside the
  tone's ±8 bins, against the tone; the same analysis of the DSD bitstream itself gives the
  source's THD+N over the same samples. Janssen's chapter 3 and appendix A discuss these
  measurements (SINAD in steady state, coherent and power averaging); with a deterministic
  signal and one long FFT no averaging was needed.
- **Out-of-band tones.** A tone above the stop frequency must not reach the PCM, directly or
  folded below the output's Nyquist frequency. Where the fold lands in the pass band, the
  modulator itself has put content there (1-bit modulators produce spurs from strong
  ultrasonic tones): a 120 kHz tone at 88.2 kHz output appears at 31.8 kHz 99.7 dB down, and
  the DSD bitstream itself carries exactly that, 99.7 dB down, at 31.8 kHz. The test
  therefore requires 105 dB, or no more than the DSD's own content there. With the wide
  filter at 176.4 kHz the folds land where DSD's shaped noise is high (above 40 kHz), so
  nothing can be measured; its exact stopband is the evidence.
- **Arithmetic.** The same stages evaluated straight from their definitions in plain f64,
  from the same coefficients, on random DSD (the largest intermediate values).

## Black-box comparison on real discs

Three tracks of the local test discs, never named: a 3-minute track of a plain DSD stereo
disc, a 4-minute track of a DST stereo disc, and a 10-minute track of a DST 5-channel
area (two of its channels are silent on this track and are left out). Each track was
converted by sacdfs (`examples/pcm_track.rs`, frame by frame as the filesystem will, the
track's own DSF from the same code fed to the other tools), by FFmpeg 8.1 (`ffmpeg -i
track.dsf -ar RATE -c:a pcm_f32le`: its DSD decoder to 352.8 kHz, then its resampler), and
by pysacd 0.2.2 (`pysacd convert -f wav -r RATE`). 60 seconds from the middle of each
track were aligned (whole samples by cross-correlation, then the fraction of a sample that
minimises the residual), level-matched by least squares, band-limited to 20 Hz-20 kHz, and
subtracted. sacdfs's output is taken before rounding (f32), the others as they come
(FFmpeg f32, pysacd 24-bit).

Residual in 20 Hz-20 kHz, after alignment and level matching (per channel; the 5-channel
track's range over its three active channels):

| Track | Compared with | Rate | Residual (dBFS) | Residual re signal | Level difference |
|---|---|---|---|---|---|
| plain DSD stereo | FFmpeg | 88.2 kHz | -124.8, -123.5 | -102 dB | < 0.0001 dB |
| | FFmpeg | 176.4 kHz | -125.0, -123.9 | -102 dB | < 0.0001 dB |
| | pysacd | 88.2 kHz | -133.8, -133.5 | -111 to -112 dB | < 0.0001 dB |
| | pysacd (60 kHz filter) | 176.4 kHz | -134.2, -134.0 | -111 to -113 dB | < 0.0001 dB |
| DST stereo | FFmpeg | 88.2 kHz | -128.8, -128.1 | -110 dB | < 0.0001 dB |
| | FFmpeg | 176.4 kHz | -128.7, -128.1 | -110 dB | < 0.0001 dB |
| | pysacd | 88.2 kHz | -134.1, -134.1 | -115 to -116 dB | < 0.0001 dB |
| | pysacd (60 kHz filter) | 176.4 kHz | -134.3, -134.2 | -116 dB | < 0.0001 dB |
| DST 5 channels | FFmpeg | 88.2 kHz | -133.7 to -137.5 | -83 to -102 dB | < 0.0001 dB |
| | FFmpeg | 176.4 kHz | -135.3 to -143.2 | -89 to -103 dB | < 0.0001 dB |
| | pysacd | 88.2 kHz | -134.3 to -134.9 | -80 to -102 dB | < 0.0001 dB |
| | pysacd (60 kHz filter) | 176.4 kHz | -134.4 to -135.0 | -81 to -103 dB | < 0.0001 dB |

sacdfs's wide filter gives the same figures against both (to within 0.5 dB): in the audio
band all its filters agree. For scale: the same method finds -140 dBFS between sacdfs's own
40 kHz and wide outputs (its floor: their pass-band ripples differ by a few 0.00001 dB), and
-155.7 dBFS between sacdfs's 24-bit output and its f32 output (the 24-bit rounding). The
"re signal" figures of the quiet 5-channel channels (signal -54 dBFS) are low only because
the absolute residual sits at that floor.

**Method notes.** The band powers come from Welch averages (2^16-point Blackman-Harris
segments, power averaging as in Janssen's appendix A); a rectangular brick-wall over the
whole segment leaked the converters' very different ultrasonic content into the band at
about -110 dBFS, which an early run mistook for differences. The whole-sample lag is found on
the audio band only, for the same reason.

**Timing.** sacdfs's PCM is on the DSD timeline (tested). FFmpeg's is 1.27 samples away
from it at 88.2 kHz and 2.53 at 176.4 kHz; pysacd's 0.27 samples at 88.2 kHz and 0.47 at
176.4 kHz, from its even-length first stage and its rounded delay compensation (see
design.md).
Neither is wrong for playback; it only means a WAV from them is not sample-aligned with the
DSF.

**Level.** The same to within 0.0001 dB: all three map 100 % modulation to full scale.

**In 20 Hz-20 kHz.** Against pysacd, -134 dBFS: within 6 dB of the method's floor, as
expected of the same family of filters (#9: the pysacd comparison is a regression check).
Against FFmpeg, -124 to -129 dBFS on the stereo tracks, 100 to 110 dB below the signal: the
size of a pass-band ripple of about 0.0001 dB, which is what FFmpeg's resampler has. All
three are transparent in the audio band; none of the residuals is near the 24-bit rounding
(-156 dBFS), because the other two are not designed to that precision, which no listener
needs.

**Above 20 kHz** (power in each band, channel 1 of the DSD stereo track, dBFS):

| Band | sacdfs 40 kHz | sacdfs wide | FFmpeg 176.4 kHz | pysacd 176.4 kHz (60 kHz filter) | FFmpeg 88.2 kHz | pysacd 88.2 kHz |
|---|---|---|---|---|---|---|
| 20-30 kHz | -73.9 | -73.9 | -73.9 | -73.9 | -73.9 | -73.9 |
| 30-40 kHz | -74.9 | -74.9 | -74.9 | -74.9 | -75.2 | -74.9 |
| 40-44 kHz | -73.2 | -72.0 | -72.0 | -72.0 | -76.3 | -73.1 |
| 44-48 kHz | -83.4 | -67.4 | -67.4 | -67.4 | | |
| 48-60 kHz | -156.3 | -52.7 | -52.8 | -52.7 | | |
| 60-88.2 kHz | -154.0 | -36.9 | -37.4 | -41.2 | | |

Below 40 kHz all agree. Above it, FFmpeg's 176.4 kHz output and pysacd's keep DSD's rising
ultrasonic noise (-37 to -53 dBFS in 48-88 kHz); sacdfs's default removes it to the 24-bit
floor, as the owner's choice of Weiss's 30-40 kHz band intends, and the wide filter keeps it
to 80 kHz like the others. At 88.2 kHz FFmpeg's resampler rolls off 3.4 dB by 40-44 kHz;
pysacd's 88.2 kHz stage is the same design as sacdfs's 40 kHz stage 3 and agrees to 0.2 dB.

The same picture on the other two tracks (to within 1 dB in every band).

## The filter designs, and why

All stages are Kaiser-windowed linear-phase low-passes (pysacd's design, `kaiser_lowpass`,
with β = 0.1102 (A - 8.7)) of odd length, so that the cascade is centred exactly on a DSD
sample. Tap counts were chosen per stage with scipy so that each stage alone reaches at
least about 115 dB where it matters, with margin to the next-shorter design that misses
(Kaiser designs are not monotonic in the attenuation parameter near their limit); the
composite is then measured (above). The stopband target is pysacd's 105-110 dB class,
accepted by the owner (#11): a deeper stopband (120 dB) would cost 20-40 % more CPU for a
difference nobody has shown to matter.

### 40 kHz (default) and 30 kHz

| Stage | Rate | Design | Taps (non-zero) | Why |
|---|---|---|---|---|
| 1 | 2.8224 MHz → 352.8 kHz (÷8) | pass 48 kHz, stop 304.8 kHz, A = 120 dB | 95 (12 byte tables) | Only what folds onto 0-48 kHz at 352.8 kHz needs removing (from 352.8 - 48 kHz up); everything else is left to later, cheaper stages. 12 bytes is pysacd's size; 11 bytes reached only 115 dB at the edge of its range. |
| 2 | 352.8 → 176.4 kHz (÷2) | half-band: pass 48, stop 128.4 kHz, A = 120 dB | 39 (21) | Protects 0-48 kHz from folding (from 176.4 - 48 kHz). Symmetric about 88.2 kHz, so a half-band: every other tap is zero. |
| 3 (40 kHz) | 176.4 kHz | half-band: pass 40.2, stop 48 kHz, A = 115 dB | 175 (89) | The band limit. Owner: fully attenuated by about 48 kHz. Putting the transition's centre on 44.1 kHz, a quarter of 176.4 kHz, makes it a half-band at half the cost of a general filter (the pass band ends at 40.2 instead of 40 kHz). 171 taps reached only 115 dB at the edge of their range. |
| 3 (30 kHz) | 176.4 kHz | pass 30, stop 36 kHz, A = 115 dB | 227 | Not a half-band (its centre is 33 kHz). |
| 3 at 88.2 kHz | 176.4 → 88.2 kHz (÷2) | the same filter, every other output | | As pysacd's 88.2 kHz stage. Content between 44.1 and 48 kHz, already in the transition, folds to 40.2-44.1 kHz, above the pass band. |
| 4 (352.8 kHz only) | 176.4 → 352.8 kHz (×2) | the half-band of stage 2 | 39 (20) | The signal is band-limited to 48 kHz after stage 3, so 352.8 kHz output is that, interpolated: about 10 % more work than 176.4 kHz instead of a long filter at 352.8 kHz. Even outputs are stage 3's samples exactly. |

### Wide (80 kHz): 176.4 and 352.8 kHz only

Owner decision (2026-10-31, reasons and sources in #11 and #5): a third cutoff for those
who follow the Philips blind tests, which found 80-100 kHz filters undetectable and a
50 kHz one detectable. 40 kHz stays the default. Not offered at 88.2 kHz, whose band ends
at 44.1 kHz.

| Stage | Rate | Design | Taps | Why |
|---|---|---|---|---|
| 1 | 2.8224 MHz → 352.8 kHz (÷8) | pass 80, stop 252.8 kHz, A = 115 dB | 143 (18 byte tables) | Protects 0-100 kHz from folding (from 352.8 - 100 kHz), which covers both outputs; the narrower transition needs 18 tables instead of 12. |
| 2 at 176.4 kHz | 352.8 → 176.4 kHz (÷2) | pass 80, stop 88.2 kHz, A = 115 dB | 337 | Owner: full stopband by 88.2 kHz, so nothing folds back below the Nyquist frequency. That 8.2 kHz transition at 352.8 kHz is the costliest filter here: wide 176.4 kHz output is about 2.2 times the default's CPU. |
| 2 at 352.8 kHz | 352.8 kHz | pass 80, stop 100 kHz, A = 115 dB | 141 | Owner: stopband from about 100 kHz. |

### Silence and alignment

- Stage 1's eight polyphase sums are made equal (1/8 each), which puts exact zeros at every
  multiple of 352.8 kHz. DSD silence (0x69), and any other byte-periodic balanced pattern,
  then converts to exact zero, so the area edges (padded with silence) are digital silence.
  The change to the response elsewhere is below -120 dB.
- Every stage has an odd length; stage 1's 8 × bytes - 1 taps are centred on the first bit
  of a byte. The output is centred on DSD sample `decimation × n`, measured at an offset
  below 0.0001 samples.

### Arithmetic

Stage 1 adds integer table entries (multiples of 2^-28; the largest sum stays below 2^31):
exact, and the entries' rounding is a fixed error below -170 dB. The later stages accumulate
in f64. In f32, stage 3's rounding alone was 0.34 of a 24-bit step RMS on random DSD, more
than the 24-bit quantisation itself (0.29), and an f32 stage 2 added 0.15; in f64 the
difference from an exact evaluation is below 0.00001 of a step. The output is rounded to
24 bits, ties to even, no dither (24 bits needs none: decision 2 of #11).
