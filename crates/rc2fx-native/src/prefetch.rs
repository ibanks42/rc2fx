//! Software prefetch for the callback's own work (`Front::warm` in
//! pipeline.rs).
//!
//! RØDE's processing runs between `Pipeline::begin` and `Pipeline::end` and
//! pushes what they use out of the cache. The mixer's Cortex-A53 executes in
//! order: a load that misses stops it until the line arrives, so the cold
//! lines of a phase are waited for one after another. A prefetch only starts
//! a line loading. Each phase therefore first asks for what it is about to
//! touch, and then does its work.
//!
//! A prefetch reads and writes nothing and cannot fault, so it changes timing
//! only. Even so, every address asked for lies inside an object the phase
//! holds and is about to use. On other architectures these compile to
//! nothing. Prefetching is always on.
const LINE: usize = 64;
/// How much of each end of a large value `ends` asks for.
const END: usize = 256;

#[inline(always)]
fn line(address: *const u8, write: bool) {
    #[cfg(target_arch = "aarch64")]
    // SAFETY: a hint. It accesses no memory, raises no fault for any address
    // and leaves the flags and the stack alone.
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
/// How many lines `bytes` bytes can lie on when they start on a multiple of
/// `align`.
const fn lines(bytes: usize, align: usize) -> usize {
    let align = if align < LINE { align } else { LINE };
    (LINE - align + bytes).div_ceil(LINE)
}
/// Where in those bytes the `index`th line is asked for: a line further on
/// each time, and never past the last byte.
const fn offset(index: usize, bytes: usize) -> usize {
    let offset = index * LINE;
    if offset < bytes { offset } else { bytes - 1 }
}
/// Every line that `bytes` bytes at `start` can lie on: one address per line,
/// none outside the bytes. Sizes are constants at almost every call, where
/// this is a run of `prfm`.
#[inline(always)]
fn span(start: *const u8, bytes: usize, align: usize, write: bool) {
    if bytes == 0 {
        return;
    }
    for index in 0..lines(bytes, align) {
        line(start.wrapping_add(offset(index, bytes)), write);
    }
}
/// Asks for `count` values at `pointer`, to read them.
#[inline(always)]
pub fn read<T>(pointer: *const T, count: usize) {
    span(
        pointer.cast(),
        count * size_of::<T>(),
        align_of::<T>(),
        false,
    );
}
/// As `read`, for values about to be written.
#[inline(always)]
pub fn write<T>(pointer: *const T, count: usize) {
    span(
        pointer.cast(),
        count * size_of::<T>(),
        align_of::<T>(),
        true,
    );
}
/// Asks for the first and the last `END` bytes of one value, or all of a
/// small one. For rc2fx-dsp's processor and settings, which are far larger
/// than a block uses and whose layout is not this crate's to know: today
/// what every block uses of them lies at their two ends.
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

    /// The addresses `span` asks for: every line of the value and no address
    /// outside it, wherever its alignment lets it start.
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
        // One channel's block is seven lines, or eight when it starts inside
        // one; a value on a line of its own is asked for once.
        assert_eq!(lines(448, 4), 8);
        assert_eq!(lines(448, 64), 7);
        assert_eq!(lines(8, 8), 1);
        // The calls themselves, on values of each shape.
        let block = [0.0f32; 112];
        read(block.as_ptr(), block.len());
        write(block.as_ptr(), block.len());
        read(block.as_ptr(), 0);
        ends(&raw const block, false);
        let large = [0u8; 4096];
        ends(&raw const large, true);
    }
}
