// SPDX-License-Identifier: GPL-3.0-or-later

//! Runtime CPU feature dispatch for the converter (#23).
//!
//! The hot code (every stage of a channel, the window fill and the sample writing) is
//! compiled three times on x86-64: for the baseline (SSE2, what a generic build gives), for
//! x86-64-v3 (AVX2) and for x86-64-v4 (AVX-512). One is chosen when the process first
//! converts, from the CPU, and can be capped with the `SACDFS_SIMD` environment variable
//! (`baseline`, `v3` or `v4`) to test or measure each path.
//!
//! **No `unsafe` here.** The `multiversion` crate generates the copies with
//! `#[target_feature]` and keeps the `unsafe` call behind its own feature check, so a copy
//! only ever runs on a CPU that has its features, whatever [`Simd`] asks for.
//!
//! **Identical output.** Each output is a fixed sequence of integer and f64 operations, and
//! Rust neither fuses a multiply and an add (even with FMA enabled) nor reorders float sums,
//! so wider vectors change only how many outputs are computed at once, never a result. The
//! golden-hash tests run on every path to hold this.

use std::sync::OnceLock;

use multiversion::multiversion;

/// The instruction sets the converter has code for.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Simd {
    /// What the build targets (SSE2 on a generic x86-64 build); the only one off x86-64.
    Baseline,
    /// x86-64-v3: AVX2, FMA, BMI1/2 (Haswell, Zen 1 and later).
    V3,
    /// x86-64-v4: AVX-512 F, BW, CD, DQ, VL (Skylake-X, Zen 4 and later). Needs a build
    /// with Rust 1.89 or later; older compilers fall back to v3.
    V4,
}

impl Simd {
    /// The name used by `SACDFS_SIMD` and in the log.
    pub fn name(self) -> &'static str {
        match self {
            Simd::Baseline => "baseline",
            Simd::V3 => "x86-64-v3",
            Simd::V4 => "x86-64-v4",
        }
    }

    /// The best this CPU (and this build) supports.
    pub fn detected() -> Simd {
        static DETECTED: OnceLock<Simd> = OnceLock::new();
        *DETECTED.get_or_init(detect)
    }

    /// What converters use by default: [`detected`](Self::detected), capped by the
    /// `SACDFS_SIMD` environment variable if it is set. Read once per process.
    pub fn selected() -> Simd {
        static SELECTED: OnceLock<Simd> = OnceLock::new();
        *SELECTED.get_or_init(|| {
            let best = Simd::detected();
            let chosen = match std::env::var("SACDFS_SIMD") {
                Ok(v) => match Simd::parse(&v) {
                    Some(cap) => cap.min(best),
                    None => {
                        log::warn!(
                            "SACDFS_SIMD={v:?} not understood (baseline, v3 or v4); using {}",
                            best.name()
                        );
                        best
                    }
                },
                Err(_) => best,
            };
            log::debug!("PCM converter: {} code (CPU supports {})", chosen.name(), best.name());
            chosen
        })
    }

    /// Parses a `SACDFS_SIMD` value.
    fn parse(s: &str) -> Option<Simd> {
        match s.trim().to_ascii_lowercase().as_str() {
            "baseline" | "base" | "generic" | "sse2" | "x86-64" => Some(Simd::Baseline),
            "v3" | "x86-64-v3" | "avx2" => Some(Simd::V3),
            "v4" | "x86-64-v4" | "avx512" => Some(Simd::V4),
            _ => None,
        }
    }
}

/// Whether this build has a v4 copy: AVX-512 target features are stable from Rust 1.89, and
/// sacdfs builds with 1.85.
#[rustversion::since(1.89)]
const HAVE_V4: bool = true;
#[rustversion::before(1.89)]
const HAVE_V4: bool = false;

#[cfg(target_arch = "x86_64")]
fn detect() -> Simd {
    use std::arch::is_x86_feature_detected as has;
    let v3 = has!("avx")
        && has!("avx2")
        && has!("bmi1")
        && has!("bmi2")
        && has!("f16c")
        && has!("fma")
        && has!("lzcnt")
        && has!("movbe")
        && has!("popcnt")
        && has!("sse3")
        && has!("ssse3")
        && has!("sse4.1")
        && has!("sse4.2");
    let v4 =
        v3 && has!("avx512f") && has!("avx512bw") && has!("avx512cd") && has!("avx512dq") && has!("avx512vl");
    if v4 && HAVE_V4 {
        Simd::V4
    } else if v3 {
        Simd::V3
    } else {
        Simd::Baseline
    }
}

#[cfg(not(target_arch = "x86_64"))]
fn detect() -> Simd {
    let _ = HAVE_V4;
    Simd::Baseline
}

/// A piece of converter work that is compiled once per instruction set. `run` and all it
/// calls must be `#[inline(always)]` (or small enough to inline), so that they are compiled
/// into each copy with that copy's features.
pub(super) trait Kernel {
    type Out;
    fn run(self) -> Self::Out;
}

/// Runs `k` with the code for `simd` (never more than the CPU has).
#[inline]
pub(super) fn dispatch<K: Kernel>(simd: Simd, k: K) -> K::Out {
    match simd {
        Simd::Baseline => k.run(),
        Simd::V3 => v3(k),
        Simd::V4 => v4(k),
    }
}

// The feature lists are x86-64-v3 and v4 (the psABI levels) without `xsave`, `cmpxchg16b`
// and `lahfsahf`, which compilers before 1.9x do not accept as target features and which the
// converter does not use. Each function also gets a baseline copy, which multiversion runs if
// the CPU lacks the features, so calling it is always safe.
#[multiversion(targets("x86_64+avx+avx2+bmi1+bmi2+f16c+fma+lzcnt+movbe+popcnt+sse3+ssse3+sse4.1+sse4.2"))]
fn v3<K: Kernel>(k: K) -> K::Out {
    k.run()
}

#[rustversion::since(1.89)]
#[multiversion(targets(
    "x86_64+avx+avx2+bmi1+bmi2+f16c+fma+lzcnt+movbe+popcnt+sse3+ssse3+sse4.1+sse4.2+avx512f+avx512bw+avx512cd+avx512dq+avx512vl"
))]
fn v4<K: Kernel>(k: K) -> K::Out {
    k.run()
}

#[rustversion::before(1.89)]
fn v4<K: Kernel>(k: K) -> K::Out {
    v3(k)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse() {
        assert_eq!(Simd::parse("V3"), Some(Simd::V3));
        assert_eq!(Simd::parse(" baseline "), Some(Simd::Baseline));
        assert_eq!(Simd::parse("x86-64-v4"), Some(Simd::V4));
        assert_eq!(Simd::parse("neon"), None);
        assert!(Simd::selected() <= Simd::detected());
    }

    /// `SACDFS_SIMD` caps the choice (CI runs the converter tests once per value).
    #[test]
    fn env_caps_the_choice() {
        let want = std::env::var("SACDFS_SIMD").ok().and_then(|v| Simd::parse(&v));
        let want = want.map_or(Simd::detected(), |cap| cap.min(Simd::detected()));
        eprintln!("SACDFS_SIMD: running {} (CPU: {})", Simd::selected().name(), Simd::detected().name());
        assert_eq!(Simd::selected(), want);
    }
}
