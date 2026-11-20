//! Software cache prefetching for the audio callback (`Front::warm`).
//!
//! RØDE's native callback runs between `Pipeline::begin` and `Pipeline::end`
//! and evicts our state from L1/L2. On the in-order Cortex-A53, cold loads
//! stall sequentially, so each phase issues `prfm` hints up front for the lines
//! it's about to touch. No-op on non-aarch64 targets.
const LINE: usize = 64;
/// Bytes to prefetch from each end of large structs in `ends`.
const END: usize = 256;

#[inline(always)]
fn line(address: *const u8, write: bool) {
    #[cfg(target_arch = "aarch64")]
    // SAFETY: `prfm` is a pure cache hint; it never faults or mutates memory/flags/stack.
    unsafe {
        use std::arch::asm;
        if write {
            asm!("prfm pstl1keep, [{p}]", p = in(reg) address, options(nostack, preserves_flags, readonly));
        } else {
            asm!("prfm pldl1keep, [{p}]", p = in(reg) address, options(nostack, preserves_flags, readonly));
        }
    }
    #[cfg(not(target_arch = "aarch64"))]
    let _ = (address, write);
}
/// Max cache lines spanned by `bytes` starting at an `align`-byte boundary.
const fn lines(bytes: usize, align: usize) -> usize {
    let align = if align < LINE { align } else { LINE };
    (LINE - align + bytes).div_ceil(LINE)
}
/// Byte offset for the `index`th prefetch, clamped to the last byte of the span.
const fn offset(index: usize, bytes: usize) -> usize {
    let offset = index * LINE;
    if offset < bytes { offset } else { bytes - 1 }
}
/// Prefetches every cache line `bytes` at `start` can touch without stepping
/// past the end. Since `bytes` is almost always a compile-time constant, this
/// unrolls to a short run of `prfm` instructions.
#[inline(always)]
fn span(start: *const u8, bytes: usize, align: usize, write: bool) {
    if bytes == 0 {
        return;
    }
    for index in 0..lines(bytes, align) {
        line(start.wrapping_add(offset(index, bytes)), write);
    }
}
#[inline(always)]
pub fn read<T>(pointer: *const T, count: usize) {
    span(
        pointer.cast(),
        count * size_of::<T>(),
        align_of::<T>(),
        false,
    );
}
#[inline(always)]
pub fn write<T>(pointer: *const T, count: usize) {
    span(
        pointer.cast(),
        count * size_of::<T>(),
        align_of::<T>(),
        true,
    );
}
/// Prefetches the first and last `END` bytes of `pointer` (or the whole value
/// if it's small). Used for large `rc2fx-dsp` processor/settings structs whose
/// hot per-block fields live near both ends.
#[inline(always)]
pub fn ends<T>(pointer: *const T, write: bool) {
    let (start, bytes) = (pointer.cast::<u8>(), size_of::<T>());
    if bytes <= 2 * END {
        span(start, bytes, align_of::<T>(), write);
    } else {
        span(start, END, align_of::<T>(), write);
        span(start.wrapping_add(bytes - END), END, 1, write);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_span_asks_for_every_line_of_the_value_and_nothing_outside_it() {
        for (bytes, align) in [(8, 8), (48, 8), (200, 8), (448, 4), (END, 1), (1, 1)] {
            for start in (4096..4096 + LINE).step_by(align) {
                let asked: Vec<usize> = (0..lines(bytes, align))
                    .map(|index| start + offset(index, bytes))
                    .collect();
                assert!(asked.iter().all(|&at| (start..start + bytes).contains(&at)));
                for wanted in start / LINE..=(start + bytes - 1) / LINE {
                    assert!(
                        asked.iter().any(|&at| at / LINE == wanted),
                        "{bytes} bytes at {start}: line {wanted}"
                    );
                }
            }
        }
        // 112-sample f32 block = 448 bytes (7 lines if 64B-aligned, 8 otherwise).
        assert_eq!(lines(448, 4), 8);
        assert_eq!(lines(448, 64), 7);
        assert_eq!(lines(8, 8), 1);
        let block = [0.0f32; 112];
        read(block.as_ptr(), block.len());
        write(block.as_ptr(), block.len());
        read(block.as_ptr(), 0);
        ends(&raw const block, false);
        let large = [0u8; 4096];
        ends(&raw const large, true);
    }
}
