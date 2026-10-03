//! Flush-to-zero for DSP threads.
//!
//! Subnormal floats (decaying filter and reverb tails) are up to ~100×
//! slower on most CPUs. Every thread that processes audio sets
//! flush-to-zero / denormals-are-zero for its duration: the audio thread
//! per callback ([`ScopedFlushDenormals`]), worker threads once at start.
//! The control register is per thread; the guard restores it, so code
//! outside DSP keeps IEEE semantics.

/// Sets FTZ/DAZ (x86-64) or FZ (AArch64) until dropped.
pub struct ScopedFlushDenormals {
    #[allow(dead_code)]
    saved: u64,
}

#[cfg(target_arch = "x86_64")]
mod imp {
    const FTZ: u32 = 1 << 15;
    const DAZ: u32 = 1 << 6;

    pub fn get() -> u64 {
        let mut csr: u32 = 0;
        // SAFETY: stores MXCSR into a local; no other effect.
        unsafe {
            std::arch::asm!("stmxcsr [{}]", in(reg) &mut csr, options(nostack, preserves_flags));
        }
        csr as u64
    }

    pub fn set(v: u64) {
        let csr = v as u32;
        // SAFETY: loads a valid MXCSR value (read from the register and
        // only changed in the FTZ/DAZ bits).
        unsafe {
            std::arch::asm!("ldmxcsr [{}]", in(reg) &csr, options(nostack, preserves_flags));
        }
    }

    pub fn flushing(v: u64) -> u64 {
        v | (FTZ | DAZ) as u64
    }
}

#[cfg(target_arch = "aarch64")]
mod imp {
    const FZ: u64 = 1 << 24;

    pub fn get() -> u64 {
        let v: u64;
        // SAFETY: reads FPCR.
        unsafe { std::arch::asm!("mrs {}, fpcr", out(reg) v, options(nomem, nostack)) };
        v
    }

    pub fn set(v: u64) {
        // SAFETY: writes FPCR with only the FZ bit changed.
        unsafe { std::arch::asm!("msr fpcr, {}", in(reg) v, options(nomem, nostack)) };
    }

    pub fn flushing(v: u64) -> u64 {
        v | FZ
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
mod imp {
    pub fn get() -> u64 {
        0
    }
    pub fn set(_: u64) {}
    pub fn flushing(v: u64) -> u64 {
        v
    }
}

impl ScopedFlushDenormals {
    #[inline]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let saved = imp::get();
        imp::set(imp::flushing(saved));
        Self { saved }
    }
}

impl Drop for ScopedFlushDenormals {
    #[inline]
    fn drop(&mut self) {
        imp::set(self.saved);
    }
}

/// Flush denormals on this thread from now on (worker threads).
pub fn flush_denormals_on_this_thread() {
    imp::set(imp::flushing(imp::get()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subnormals_flush_inside_the_guard_only() {
        let tiny = std::hint::black_box(f32::MIN_POSITIVE);
        let half = std::hint::black_box(0.5f32);
        assert!((tiny * half) > 0.0, "IEEE outside");
        {
            let _g = ScopedFlushDenormals::new();
            if cfg!(any(target_arch = "x86_64", target_arch = "aarch64")) {
                assert_eq!(std::hint::black_box(tiny) * half, 0.0, "flushed inside");
            }
        }
        assert!((std::hint::black_box(tiny) * half) > 0.0, "restored");
    }
}
