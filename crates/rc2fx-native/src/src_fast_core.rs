//! Bit-exact `SRC_LINEAR` for mono converters of libsamplerate 0.2.2, as built
//! in the 1.7.7 image. Kept free of crate dependencies so that
//! `tools/src_fast_check` can run this file against the real library.
//!
//! The vendor gives each USB channel its own mono converter and passes every
//! channel the same ratio and frame counts in a cycle. In `linear_vari_process`
//! no control flow depends on a sample: the position, ratio and frame counts
//! alone fix which input frames feed each output frame. So converters in the
//! same state share one [`Plan`], and only the interpolation runs per channel.
//! That removes the per-frame divide and two `lrint` calls from all channels
//! but the first.
//!
//! Every operation follows the library's disassembly, including the fused
//! multiply-add its compiler chose. `prepare` declines anything but the plain
//! mono case, and the caller then runs the real library.
//!
//! [`interpolate_scalar`] is the reference interpolation. On AArch64
//! [`interpolate`] runs a NEON form of it, four frames at a time, which applies
//! the same IEEE operations to each lane and so writes the same bits. The tests
//! below compare the two, and `tools/src_fast_check` compares both with the
//! library.
use std::ffi::{c_int, c_long};

/// The verified library's file size, and its `.text`: offset, shared by the
/// file and the mapping, and FNV-1a 64.
pub const FILE_SIZE: i64 = 1_497_272;
pub const TEXT_OFFSET: usize = 0x11f0;
pub const TEXT_SIZE: usize = 0x4e64;
pub const TEXT_FNV: u64 = 0xbf48_b5bf_76f3_ed46;
/// `linear_state_vt` and its first entry, `linear_vari_process`.
pub const LINEAR_VT_OFFSET: usize = 0x17_d0f8;
pub const LINEAR_VARI_OFFSET: usize = 0x5700;

/// Largest frame count one plan holds. The vendor uses about 112.
pub const MAX_FRAMES: usize = 1024;
const MIN_RATIO: f64 = 1.0 / 256.0;
const MAX_RATIO: f64 = 256.0;
const MIN_RATIO_DIFF: f64 = 1e-20;

/// `SRC_STATE`.
#[repr(C)]
pub struct State {
    pub vt: *const usize,
    pub last_ratio: f64,
    pub last_position: f64,
    pub error: c_int,
    pub channels: c_int,
    pub mode: c_int,
    pub callback: usize,
    pub user_data: usize,
    pub saved_frames: c_long,
    pub saved_data: usize,
    pub private_data: *mut Linear,
}

/// `LINEAR_DATA`.
#[repr(C)]
pub struct Linear {
    pub magic: c_int,
    pub dirty: u8,
    pub in_count: c_long,
    pub in_used: c_long,
    pub out_count: c_long,
    pub out_gen: c_long,
    pub last_value: *mut f32,
}

/// `SRC_DATA`.
#[repr(C)]
pub struct Data {
    pub data_in: *const f32,
    pub data_out: *mut f32,
    pub input_frames: c_long,
    pub output_frames: c_long,
    pub input_frames_used: c_long,
    pub output_frames_gen: c_long,
    pub end_of_input: c_int,
    pub src_ratio: f64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct Key {
    last_ratio: u64,
    last_position: u64,
    target: u64,
    input_frames: c_long,
    output_frames: c_long,
}

/// One cycle's schedule, built for the first channel and reused while the
/// next converter's state and request match exactly.
pub struct Plan {
    key: Option<Key>,
    /// Outputs that interpolate from the saved last sample.
    first: usize,
    count: usize,
    used: [u16; MAX_FRAMES],
    fraction: [f64; MAX_FRAMES],
    in_used: c_long,
    last_position: f64,
    last_ratio: f64,
    builds: u64,
}

impl Plan {
    pub const fn new() -> Self {
        Self {
            key: None,
            first: 0,
            count: 0,
            used: [0; MAX_FRAMES],
            fraction: [0.0; MAX_FRAMES],
            in_used: 0,
            last_position: 0.0,
            last_ratio: 0.0,
            builds: 0,
        }
    }
    /// How many schedules this plan has built.
    pub fn builds(&self) -> u64 {
        self.builds
    }
    pub fn in_used(&self) -> c_long {
        self.in_used
    }
    pub fn last_position(&self) -> f64 {
        self.last_position
    }
    pub fn last_ratio(&self) -> f64 {
        self.last_ratio
    }
    pub fn count(&self) -> usize {
        self.count
    }

    /// The fraction and input frame of each output frame, and how many of the
    /// frames start from the saved last sample. The clamps hold whatever the
    /// fields are, so the interpolation loops index without a check per frame
    /// and cannot panic.
    #[inline(always)]
    fn schedule(&self) -> (&[f64], &[u16], usize) {
        let count = self.count.min(MAX_FRAMES);
        (
            &self.fraction[..count],
            &self.used[..count],
            self.first.min(count),
        )
    }

    /// Fills the schedule. False when the request needs the real library.
    fn build(
        &mut self,
        last_ratio: f64,
        last_position: f64,
        target: f64,
        frames: (i64, i64),
    ) -> bool {
        self.builds += 1;
        let (in_count, out_count) = frames;
        let in_frames = in_count as f64;
        let varying = (last_ratio - target).abs() > MIN_RATIO_DIFF;
        let step = |generated: usize, ratio: f64| {
            if varying {
                generated as f64 * (target - last_ratio) / out_count as f64 + last_ratio
            } else {
                ratio
            }
        };
        let total = out_count as usize;
        let mut ratio = last_ratio;
        let mut index = last_position;
        let mut generated = 0;
        // Samples before the first input frame.
        while index < 1.0 && generated < total {
            if 1.0 + index >= in_frames {
                break;
            }
            ratio = step(generated, ratio);
            self.used[generated] = 0;
            self.fraction[generated] = index;
            generated += 1;
            index += 1.0 / ratio;
        }
        self.first = generated;
        let (mut rem, whole) = split(index);
        let mut used = whole;
        index = rem;
        while generated < total && used as f64 + index < in_frames {
            ratio = step(generated, ratio);
            // The library would read before or past its input here.
            if used < 1 || used >= in_count {
                return false;
            }
            self.used[generated] = used as u16;
            self.fraction[generated] = index;
            generated += 1;
            index += 1.0 / ratio;
            let whole;
            (rem, whole) = split(index);
            used += whole;
            index = rem;
        }
        if used < 0 {
            return false;
        }
        if used > in_count {
            index += (used - in_count) as f64;
            used = in_count;
        }
        self.count = generated;
        self.in_used = used;
        self.last_position = index;
        self.last_ratio = ratio;
        true
    }
}

impl Default for Plan {
    fn default() -> Self {
        Self::new()
    }
}

/// `fmod_one` and the frame count it removes.
#[inline]
fn split(value: f64) -> (f64, i64) {
    let mut rem = value - lrint(value) as f64;
    if rem < 0.0 {
        rem += 1.0;
    }
    (rem, lrint(value - rem))
}

/// Round to nearest, ties to even: `lrint` while [`round_to_nearest`] holds.
#[inline]
fn lrint(value: f64) -> i64 {
    value.round_ties_even() as i64
}

/// True while the thread rounds to nearest, which `lrint` depends on.
#[inline]
fn round_to_nearest() -> bool {
    #[cfg(target_arch = "aarch64")]
    {
        let control: u64;
        unsafe { std::arch::asm!("mrs {}, fpcr", out(reg) control, options(nomem, nostack)) };
        control & (3 << 22) == 0
    }
    #[cfg(not(target_arch = "aarch64"))]
    {
        true
    }
}

/// Makes `plan` current for this call. False leaves the call to the real
/// library. Reads the state only.
///
/// # Safety
/// `state` and `data` must be null or valid; `linear_vt` must be the address of
/// the verified library's `linear_state_vt`, and `linear_vari` its first entry.
pub unsafe fn prepare(
    plan: &mut Plan,
    state: *const State,
    data: *const Data,
    linear_vt: usize,
    linear_vari: usize,
) -> bool {
    if state.is_null() || data.is_null() {
        return false;
    }
    let (state, data) = unsafe { (&*state, &*data) };
    if state.vt as usize != linear_vt
        || unsafe { *state.vt } != linear_vari
        || state.channels != 1
        || state.mode != 0
        || state.private_data.is_null()
        || data.data_in.is_null()
        || data.data_out.is_null()
    {
        return false;
    }
    let linear = unsafe { &*state.private_data };
    let frames = (data.input_frames, data.output_frames);
    let limit = MAX_FRAMES as c_long;
    if linear.dirty == 0
        || linear.last_value.is_null()
        || !(1..=limit).contains(&frames.0)
        || !(1..=limit).contains(&frames.1)
        || !(MIN_RATIO..=MAX_RATIO).contains(&data.src_ratio)
        || !(MIN_RATIO..=MAX_RATIO).contains(&state.last_ratio)
        || !state.last_position.is_finite()
        || state.last_position < 0.0
        || !round_to_nearest()
    {
        return false;
    }
    let (input, output) = (data.data_in as usize, data.data_out as usize);
    let overlap = if input < output {
        input + frames.0 as usize * 4 > output
    } else {
        output + frames.1 as usize * 4 > input
    };
    if overlap {
        return false;
    }
    let key = Key {
        last_ratio: state.last_ratio.to_bits(),
        last_position: state.last_position.to_bits(),
        target: data.src_ratio.to_bits(),
        input_frames: frames.0,
        output_frames: frames.1,
    };
    if plan.key == Some(key) {
        return true;
    }
    plan.key = None;
    if !plan.build(
        state.last_ratio,
        state.last_position,
        data.src_ratio,
        frames,
    ) {
        return false;
    }
    plan.key = Some(key);
    true
}

/// One output frame: `before + fraction * (after - before)` in double
/// precision, with the fused multiply-add the library uses.
#[inline(always)]
fn blend(fraction: f64, before: f32, after: f32) -> f32 {
    let before = f64::from(before);
    fraction.mul_add(f64::from(after) - before, before) as f32
}

/// Writes the frames that lie between the saved last sample and the first
/// input frame, one per entry of `fraction`.
///
/// # Safety
/// As for [`interpolate`], with `output` holding `fraction.len()` frames.
#[inline(always)]
unsafe fn interpolate_head(
    fraction: &[f64],
    state: *const State,
    input: *const f32,
    output: *mut f32,
) {
    unsafe {
        let last = *(*(*state).private_data).last_value;
        let first = *input;
        for (frame, fraction) in fraction.iter().enumerate() {
            *output.add(frame) = blend(*fraction, last, first);
        }
    }
}

/// Writes the plan's output frames to `output`.
///
/// # Safety
/// `plan` must be current for `state` and `input`, and `output` must hold
/// `plan.count()` frames.
#[inline]
pub unsafe fn interpolate(plan: &Plan, state: *const State, input: *const f32, output: *mut f32) {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        interpolate_neon(plan, state, input, output)
    }
    #[cfg(not(target_arch = "aarch64"))]
    unsafe {
        interpolate_scalar(plan, state, input, output)
    }
}

/// The reference interpolation, one frame at a time. Other architectures run
/// it; on AArch64 only the tests and `tools/src_fast_check` do.
///
/// # Safety
/// As for [`interpolate`].
#[cfg_attr(target_arch = "aarch64", allow(dead_code))]
#[inline]
pub unsafe fn interpolate_scalar(
    plan: &Plan,
    state: *const State,
    input: *const f32,
    output: *mut f32,
) {
    let (fraction, used, head) = plan.schedule();
    unsafe {
        interpolate_head(&fraction[..head], state, input, output);
        let frames = fraction[head..].iter().zip(&used[head..]);
        for (offset, (fraction, used)) in frames.enumerate() {
            let used = usize::from(*used);
            let value = blend(*fraction, *input.add(used - 1), *input.add(used));
            *output.add(head + offset) = value;
        }
    }
}

/// True when the four output frames from `frame` read four consecutive input
/// frames, so that two vector loads cover them. Each of the three steps is
/// compared: steps of 0, 1 and 2 also add up to 3.
#[cfg(any(test, target_arch = "aarch64"))]
#[inline(always)]
fn consecutive(used: &[u16], frame: usize) -> bool {
    match used.get(frame..frame + 4) {
        Some(&[first, second, third, fourth]) => {
            let first = u32::from(first);
            u32::from(second) == first + 1
                && u32::from(third) == first + 2
                && u32::from(fourth) == first + 3
        }
        _ => false,
    }
}

/// [`interpolate_scalar`] with NEON. Four frames that read consecutive input
/// frames go through one vector block; any other frame goes through the scalar
/// code, one at a time. At the vendor's ratios the input index repeats or
/// skips two or three times in a call of about 112 frames, so almost every
/// frame takes the vector block.
///
/// The vector block runs the scalar frame's instructions lane by lane: widen
/// (`fcvtl`), subtract, fused multiply-add with `before` as the addend
/// (`fmla`), narrow (`fcvtn`). Each has the scalar instruction's rounding, NaN
/// and flush-to-zero behaviour, so the bits match. It reads input frames
/// `used - 1 ..= used + 3` and writes four output frames, which are the frames
/// the scalar code reads and writes.
///
/// # Safety
/// As for [`interpolate`].
#[cfg(target_arch = "aarch64")]
#[inline]
unsafe fn interpolate_neon(plan: &Plan, state: *const State, input: *const f32, output: *mut f32) {
    use std::arch::aarch64::{
        vcvt_f32_f64, vcvt_f64_f32, vcvt_high_f32_f64, vcvt_high_f64_f32, vfmaq_f64, vget_low_f32,
        vld1q_f32, vld1q_f64, vst1q_f32, vsubq_f64,
    };
    let (fraction, used, head) = plan.schedule();
    unsafe {
        interpolate_head(&fraction[..head], state, input, output);
        let mut frame = head;
        while frame < used.len() {
            let index = usize::from(used[frame]);
            if consecutive(used, frame) {
                let before = vld1q_f32(input.add(index - 1));
                let after = vld1q_f32(input.add(index));
                let before_low = vcvt_f64_f32(vget_low_f32(before));
                let before_high = vcvt_high_f64_f32(before);
                let after_low = vcvt_f64_f32(vget_low_f32(after));
                let after_high = vcvt_high_f64_f32(after);
                // `consecutive` found four frames from `frame`, and `fraction`
                // is as long as `used`.
                let fraction_low = vld1q_f64(fraction.as_ptr().add(frame));
                let fraction_high = vld1q_f64(fraction.as_ptr().add(frame + 2));
                // `before` is the addend, as in `blend`, so that a NaN there
                // takes the same precedence. The order of the two factors does
                // not matter: no plan holds a NaN fraction.
                let low = vfmaq_f64(before_low, fraction_low, vsubq_f64(after_low, before_low));
                let high = vfmaq_f64(
                    before_high,
                    fraction_high,
                    vsubq_f64(after_high, before_high),
                );
                vst1q_f32(
                    output.add(frame),
                    vcvt_high_f32_f64(vcvt_f32_f64(low), high),
                );
                frame += 4;
            } else {
                let value = blend(fraction[frame], *input.add(index - 1), *input.add(index));
                *output.add(frame) = value;
                frame += 1;
            }
        }
    }
}

/// Leaves the converter and `data` as the library would.
///
/// # Safety
/// As for [`interpolate`], and `state` and `data` must be writable.
#[inline]
pub unsafe fn commit(plan: &Plan, state: *mut State, data: *mut Data) {
    unsafe {
        let linear = &mut *(*state).private_data;
        let data = &mut *data;
        linear.in_count = data.input_frames;
        linear.out_count = data.output_frames;
        linear.in_used = plan.in_used;
        linear.out_gen = plan.count as c_long;
        (*state).last_position = plan.last_position;
        (*state).last_ratio = plan.last_ratio;
        if plan.in_used > 0 {
            *linear.last_value = *data.data_in.add(plan.in_used as usize - 1);
        }
        data.input_frames_used = plan.in_used;
        data.output_frames_gen = plan.count as c_long;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The interpolation as it was before the vector block and the slices,
    /// kept word for word. Every test compares against it.
    // The index loops are the preserved original shape; rewriting them to
    // iterators (clippy 1.100's needless_range_loop) would change the thing
    // the tests check the fast path against.
    #[allow(clippy::needless_range_loop)]
    unsafe fn original(plan: &Plan, state: *const State, input: *const f32, output: *mut f32) {
        unsafe {
            let last = f64::from(*(*(*state).private_data).last_value);
            let first = f64::from(*input);
            for frame in 0..plan.first {
                let value = plan.fraction[frame].mul_add(first - last, last);
                *output.add(frame) = value as f32;
            }
            for frame in plan.first..plan.count {
                let used = plan.used[frame] as usize;
                let before = f64::from(*input.add(used - 1));
                let after = f64::from(*input.add(used));
                let value = plan.fraction[frame].mul_add(after - before, before);
                *output.add(frame) = value as f32;
            }
        }
    }

    type Path = unsafe fn(&Plan, *const State, *const f32, *mut f32);

    /// Everything that must write what `original` writes.
    const PATHS: &[(&str, Path)] = &[
        ("interpolate_scalar", interpolate_scalar),
        ("interpolate", interpolate),
        #[cfg(target_arch = "aarch64")]
        ("interpolate_neon", interpolate_neon),
    ];

    struct Random(u64);
    impl Random {
        fn next(&mut self) -> u64 {
            let mut x = self.0;
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            self.0 = x;
            x.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }
        fn range(&mut self, low: usize, high: usize) -> usize {
            low + (self.next() % (high - low + 1) as u64) as usize
        }
        fn unit(&mut self) -> f64 {
            (self.next() >> 11) as f64 / (1u64 << 53) as f64
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum Samples {
        Audio,
        AnyBits,
        Tiny,
        Silence,
        Huge,
        Special,
    }
    const KINDS: [Samples; 6] = [
        Samples::Audio,
        Samples::AnyBits,
        Samples::Tiny,
        Samples::Silence,
        Samples::Huge,
        Samples::Special,
    ];

    /// Signed zeros, infinities, quiet and signalling NaNs with several
    /// payloads, the largest and smallest normal and subnormal values.
    const SPECIAL: [u32; 22] = [
        0x0000_0000,
        0x8000_0000,
        0x7f80_0000,
        0xff80_0000,
        0x7fc0_0000,
        0xffc0_0000,
        0x7fc0_0001,
        0xffd5_5555,
        0x7fff_ffff,
        0x7f80_0001,
        0xff80_0001,
        0x7fa5_5555,
        0xffbf_ffff,
        0x7f7f_ffff,
        0xff7f_ffff,
        0x0080_0000,
        0x8080_0000,
        0x0000_0001,
        0x8000_0001,
        0x007f_ffff,
        0x3f80_0000,
        0xbf80_0000,
    ];

    fn sample(random: &mut Random, kind: Samples) -> f32 {
        let bits = random.next() as u32;
        match kind {
            Samples::Audio => (random.unit() * 2.0 - 1.0) as f32,
            Samples::AnyBits => f32::from_bits(bits),
            // Subnormals and zeros of either sign.
            Samples::Tiny => f32::from_bits(bits & 0x807f_ffff),
            Samples::Silence => f32::from_bits(bits & 0x8000_0000),
            // From 2^125 to the largest finite value, either sign.
            Samples::Huge => {
                f32::from_bits((bits & 0x8000_0000) | (0x7e00_0000 + (bits >> 8) % 0x0180_0000))
            }
            Samples::Special if bits & 1 == 0 => (random.unit() * 2.0 - 1.0) as f32,
            Samples::Special => f32::from_bits(SPECIAL[random.range(0, SPECIAL.len() - 1)]),
        }
    }

    fn fill(random: &mut Random, kind: Samples, samples: &mut [f32]) {
        for value in samples {
            *value = sample(random, kind);
        }
    }

    /// Stands in for the library's `linear_state_vt`.
    static VTABLE: [usize; 2] = [LINEAR_VARI_OFFSET, 0];

    /// A mono converter that has run before, as `prepare` requires. Its two
    /// small allocations are left to the end of the test run.
    fn converter(position: f64, ratio: f64) -> State {
        let last_value = Box::into_raw(Box::new(0f32));
        let linear = Box::into_raw(Box::new(Linear {
            magic: 0,
            dirty: 1,
            in_count: 0,
            in_used: 0,
            out_count: 0,
            out_gen: 0,
            last_value,
        }));
        State {
            vt: VTABLE.as_ptr(),
            last_ratio: ratio,
            last_position: position,
            error: 0,
            channels: 1,
            mode: 0,
            callback: 0,
            user_data: 0,
            saved_frames: 0,
            saved_data: 0,
            private_data: linear,
        }
    }

    fn last_value(state: &State) -> *mut f32 {
        unsafe { (*state.private_data).last_value }
    }

    const CANARY: u32 = 0x5a5a_a5a5;
    const SLACK: usize = 16;

    /// The output of `original` and of one other path, each followed by
    /// canaries.
    struct Outputs {
        expected: Vec<f32>,
        actual: Vec<f32>,
    }

    impl Outputs {
        fn new() -> Self {
            Self {
                expected: vec![0.0; MAX_FRAMES + SLACK],
                actual: vec![0.0; MAX_FRAMES + SLACK],
            }
        }

        /// Runs `original` for a current plan and returns its frames with the
        /// canaries that follow them.
        fn expect(&mut self, plan: &Plan, state: &State, input: &[f32]) -> &[f32] {
            let span = plan.count + SLACK;
            self.expected[..span].fill(f32::from_bits(CANARY));
            unsafe { original(plan, state, input.as_ptr(), self.expected.as_mut_ptr()) };
            let canaries = &self.expected[plan.count..span];
            assert!(canaries.iter().all(|value| value.to_bits() == CANARY));
            &self.expected[..span]
        }

        /// Runs every path on a current plan. Each must write the bits that
        /// `original` writes and leave the frames after them alone.
        fn compare(
            &mut self,
            plan: &Plan,
            state: &State,
            input: &[f32],
            context: &dyn Fn() -> String,
        ) {
            let span = self.expect(plan, state, input).len();
            for (name, path) in PATHS {
                self.actual[..span].fill(f32::from_bits(CANARY));
                unsafe { path(plan, state, input.as_ptr(), self.actual.as_mut_ptr()) };
                self.same(plan, &self.actual[..span], name, context);
            }
        }

        fn same(&self, plan: &Plan, actual: &[f32], name: &str, context: &dyn Fn() -> String) {
            let expected = &self.expected[..actual.len()];
            let differs = |frame: &usize| expected[*frame].to_bits() != actual[*frame].to_bits();
            if let Some(frame) = (0..actual.len()).find(differs) {
                let low = frame.saturating_sub(4);
                let high = (frame + 5).min(plan.count);
                panic!(
                    "{name} differs from the original at frame {frame} of {} (head {}): \
                     {:#010x} for {:#010x}; input frames {:?}, fractions {:?}; {}",
                    plan.count,
                    plan.first,
                    actual[frame].to_bits(),
                    expected[frame].to_bits(),
                    &plan.used[low..high.max(low)],
                    &plan.fraction[low..high.max(low)],
                    context()
                );
            }
        }
    }

    /// Runs `body` once for each setting of flush-to-zero and default-NaN that
    /// AArch64 offers, and once elsewhere. The vendor's threads may run with
    /// either bit set.
    fn each_float_mode(mut body: impl FnMut(&str, u64)) {
        #[cfg(target_arch = "aarch64")]
        {
            struct Restore(u64);
            impl Drop for Restore {
                fn drop(&mut self) {
                    unsafe { std::arch::asm!("msr fpcr, {}", in(reg) self.0) };
                }
            }
            let saved: u64;
            unsafe { std::arch::asm!("mrs {}, fpcr", out(reg) saved) };
            let _restore = Restore(saved);
            let modes = [
                ("default", 0u64),
                ("FZ", 1 << 24),
                ("DN", 1 << 25),
                ("FZ+DN", 3 << 24),
            ];
            for (index, (name, bits)) in modes.into_iter().enumerate() {
                let control = saved & !(3 << 24) | bits;
                let held: u64;
                unsafe {
                    std::arch::asm!("msr fpcr, {}", in(reg) control);
                    std::arch::asm!("mrs {}, fpcr", out(reg) held);
                }
                assert_eq!(held, control, "the processor did not take {name}");
                body(name, index as u64);
            }
        }
        #[cfg(not(target_arch = "aarch64"))]
        body("default", 0);
    }

    /// How many of the plan's frames the NEON loop sends through its vector
    /// block: the same walk, without the arithmetic.
    fn vector_frames(plan: &Plan) -> usize {
        let (_, used, head) = plan.schedule();
        let (mut frame, mut vector) = (head, 0);
        while frame < used.len() {
            if consecutive(used, frame) {
                frame += 4;
                vector += 4;
            } else {
                frame += 1;
            }
        }
        vector
    }

    /// What `Plan::build` promises the interpolation. Every frame after the
    /// head reads input frames inside the block. Every fraction is a number
    /// from 0 to 1: the vector block's multiply-add takes its two factors in
    /// the other order, which chooses a different NaN only when both factors
    /// are NaN.
    fn assert_built(plan: &Plan, frames: usize, wanted: usize) {
        assert!(plan.first <= plan.count && plan.count <= wanted.min(MAX_FRAMES));
        for frame in 0..plan.count {
            let fraction = plan.fraction[frame];
            assert!(
                (0.0..=1.0).contains(&fraction),
                "fraction {fraction:e} at frame {frame}"
            );
            let used = usize::from(plan.used[frame]);
            assert!(
                frame < plan.first || (1..frames).contains(&used),
                "input frame {used} of {frames} at frame {frame}"
            );
        }
    }

    #[derive(Clone, Copy, Debug)]
    enum Shape {
        /// One ratio per cycle on the vendor's grid, about 112 frames.
        Vendor,
        /// Any ratio and any frame counts the fast path accepts.
        Wide,
    }

    #[derive(Default, Debug)]
    struct Totals {
        calls: u64,
        declined: u64,
        frames: u64,
        vector: u64,
    }

    fn block(random: &mut Random) -> usize {
        match random.range(0, 3) {
            0 => random.range(1, 9),
            1 => random.range(1, 128),
            _ => random.range(1, MAX_FRAMES),
        }
    }

    /// Feeds `channels` converters in step, as the vendor does: every cycle
    /// gives each of them the same request and its own samples, and each
    /// carries its state to the next cycle. The ratio and the block sizes
    /// change from cycle to cycle, and now and then the converters restart.
    fn stream(
        seed: u64,
        channels: usize,
        kind: Samples,
        shape: Shape,
        cycles: usize,
        totals: &mut Totals,
    ) {
        let mut random = Random(seed | 1);
        let mut converters: Vec<State> = (0..channels).map(|_| converter(0.0, 1.0)).collect();
        let mut plan = Box::new(Plan::new());
        let mut outputs = Outputs::new();
        let mut input = vec![0f32; MAX_FRAMES];
        let mut output = vec![0f32; MAX_FRAMES];
        let mut ratio = 1.0f64;
        let mut step = random.range(0, 800) as i64 - 400;
        let nominal = [112, 96, 49, 196][random.range(0, 3)];
        for cycle in 0..cycles {
            let (frames, wanted) = match shape {
                Shape::Vendor => {
                    if random.range(0, 3) == 0 {
                        step = (step + random.range(0, 4) as i64 - 2).clamp(-400, 400);
                    }
                    ratio = f64::from((step as f64 / 16384.0 + 1.0) as f32);
                    let frames = nominal + random.range(0, 1);
                    let wanted = (frames as f64 * ratio).ceil() as usize + random.range(0, 2);
                    (frames, wanted)
                }
                Shape::Wide => {
                    match random.range(0, 8) {
                        0 => ratio = (random.unit() * 16.0 - 8.0).exp2(),
                        1 => ratio *= 1.0 + (random.unit() - 0.5) * 1e-15,
                        2 => ratio *= 1.0 + (random.unit() - 0.5) * 1e-3,
                        3 => ratio = 1.0,
                        4 => ratio = (random.unit() * 0.25 - 0.125).exp2(),
                        _ => {}
                    }
                    ratio = ratio.clamp(MIN_RATIO, MAX_RATIO);
                    (block(&mut random), block(&mut random))
                }
            };
            if random.range(0, 63) == 0 {
                let position = [0.0, random.unit(), random.unit() * 3.0][random.range(0, 2)];
                for state in &mut converters {
                    state.last_position = position;
                    state.last_ratio = ratio;
                    unsafe { *last_value(state) = 0.0 };
                }
            }
            let builds = plan.builds();
            let mut accepted = 0;
            for (channel, state) in converters.iter_mut().enumerate() {
                fill(&mut random, kind, &mut input[..frames]);
                let mut data = Data {
                    data_in: input.as_ptr(),
                    data_out: output.as_mut_ptr(),
                    input_frames: frames as c_long,
                    output_frames: wanted as c_long,
                    input_frames_used: 0,
                    output_frames_gen: 0,
                    end_of_input: 0,
                    src_ratio: ratio,
                };
                totals.calls += 1;
                let table = VTABLE.as_ptr() as usize;
                if !unsafe { prepare(&mut plan, state, &data, table, LINEAR_VARI_OFFSET) } {
                    totals.declined += 1;
                    continue;
                }
                accepted += 1;
                assert_built(&plan, frames, wanted);
                let context = || {
                    format!(
                        "seed {seed:#x}, {channels} channels, {kind:?}, {shape:?}, cycle {cycle}, \
                         channel {channel}, ratio {ratio:e}, {frames} frames in, {wanted} out"
                    )
                };
                outputs.compare(&plan, state, &input[..frames], &context);
                unsafe { commit(&plan, state, &mut data) };
                totals.frames += plan.count as u64;
                totals.vector += vector_frames(&plan) as u64;
            }
            // The converters stay in one state, so one schedule serves a cycle,
            // and the next cycle too when it repeats the state and the request.
            if accepted > 0 {
                assert!(plan.builds() - builds <= 1);
                assert_eq!(accepted, channels);
            }
        }
    }

    fn streams(shape: Shape, cycles: usize) -> Totals {
        let mut totals = Totals::default();
        each_float_mode(|_, mode| {
            for channels in 2..=20 {
                for (index, kind) in KINDS.into_iter().enumerate() {
                    let seed = 0x9e37_79b9_7f4a_7c15u64
                        .wrapping_mul((channels * 64 + index * 8) as u64 + mode + 1);
                    stream(seed, channels, kind, shape, cycles, &mut totals);
                }
            }
        });
        println!("{shape:?}: {totals:?}");
        totals
    }

    #[test]
    fn streams_at_the_vendors_ratios_match_the_original() {
        let totals = streams(Shape::Vendor, 40);
        assert_eq!(totals.declined, 0);
        assert!(totals.frames > 2_000_000);
        // The gain depends on nearly every frame taking the vector block.
        assert!(totals.vector * 10 > totals.frames * 9, "{totals:?}");
    }

    #[test]
    fn streams_at_any_ratio_match_the_original() {
        let totals = streams(Shape::Wide, 40);
        assert!(totals.declined * 10 < totals.calls, "{totals:?}");
        assert!(totals.frames > 2_000_000);
    }

    /// One call on a converter that keeps its state.
    fn call(
        plan: &mut Plan,
        state: &mut State,
        input: &[f32],
        wanted: usize,
        ratio: f64,
        outputs: &mut Outputs,
    ) -> Option<usize> {
        let mut output = [0f32; MAX_FRAMES];
        let mut data = Data {
            data_in: input.as_ptr(),
            data_out: output.as_mut_ptr(),
            input_frames: input.len() as c_long,
            output_frames: wanted as c_long,
            input_frames_used: 0,
            output_frames_gen: 0,
            end_of_input: 0,
            src_ratio: ratio,
        };
        let table = VTABLE.as_ptr() as usize;
        if !unsafe { prepare(plan, state, &data, table, LINEAR_VARI_OFFSET) } {
            return None;
        }
        assert_built(plan, input.len(), wanted);
        let context = || format!("ratio {ratio:e}, {} frames in, {wanted} out", input.len());
        outputs.compare(plan, state, input, &context);
        unsafe { commit(plan, state, &mut data) };
        Some(plan.count)
    }

    #[test]
    fn every_block_size_matches_the_original() {
        let ratios = [
            1.0,
            1.0 + 400.0 / 16384.0,
            1.0 - 400.0 / 16384.0,
            1.0 + 1.0 / 16384.0,
            2.0,
            0.5,
            160.0 / 147.0,
            147.0 / 160.0,
        ];
        let mut seen = [false; MAX_FRAMES + 1];
        each_float_mode(|_, mode| {
            let mut random = Random(0x51ed_270b_9f3c_0a11 + mode);
            let mut plan = Box::new(Plan::new());
            let mut outputs = Outputs::new();
            let mut input = vec![0f32; MAX_FRAMES];
            for ratio in ratios {
                let mut state = converter(random.unit(), ratio);
                for size in 1..=MAX_FRAMES {
                    let kind = KINDS[random.range(0, KINDS.len() - 1)];
                    fill(&mut random, kind, &mut input);
                    // The input ends the block, then the output room does.
                    let requests = [(size, MAX_FRAMES), (MAX_FRAMES, size)];
                    for (frames, wanted) in requests {
                        let count = call(
                            &mut plan,
                            &mut state,
                            &input[..frames],
                            wanted,
                            ratio,
                            &mut outputs,
                        );
                        if let Some(count) = count {
                            seen[count] = true;
                        }
                    }
                }
                // A position past the end of the input produces no frame.
                state.last_position += 4.0;
                let count = call(&mut plan, &mut state, &input[..2], 8, ratio, &mut outputs);
                assert_eq!(count, Some(0));
                seen[0] = true;
            }
        });
        let missing: Vec<usize> = (0..seen.len()).filter(|count| !seen[*count]).collect();
        assert!(
            missing.is_empty(),
            "frame counts never produced: {missing:?}"
        );
    }

    fn fraction(random: &mut Random) -> f64 {
        match random.range(0, 9) {
            0 => 0.0,
            1 => 1.0,
            2 => f64::from_bits(1),
            3 => 1.0 - f64::EPSILON / 2.0,
            _ => random.unit(),
        }
    }

    /// Steps of the input index over three frames that add up to 3 without
    /// being 1, 1, 1.
    const TRAPS: [[usize; 3]; 6] = [
        [0, 1, 2],
        [0, 2, 1],
        [1, 0, 2],
        [1, 2, 0],
        [2, 0, 1],
        [2, 1, 0],
    ];

    /// A schedule that `Plan::build` may or may not produce, over `frames`
    /// input frames: repeats, skips, backward jumps, runs of every length and
    /// any head.
    fn handcrafted(random: &mut Random, frames: usize) -> Box<Plan> {
        let mut plan = Box::new(Plan::new());
        let count = match random.range(0, 3) {
            0 => random.range(0, 12),
            1 => random.range(0, 130),
            _ => random.range(0, MAX_FRAMES),
        };
        let top = frames - 1;
        let style = random.range(0, 6);
        let trap = TRAPS[random.range(0, TRAPS.len() - 1)];
        let run = random.range(1, 9);
        let mut used = random.range(1, top);
        for frame in 0..count {
            plan.used[frame] = used as u16;
            plan.fraction[frame] = fraction(random);
            used += match style {
                0 => 1,
                1 if random.range(0, 15) == 0 => 2 * random.range(0, 1),
                1 => 1,
                2 => random.range(0, 2),
                3 => trap[frame % 3],
                // Runs of `run` frames with a repeat or a skip between them.
                4 if frame % run == run - 1 => 2 * random.range(0, 1),
                4 => 1,
                5 if random.range(0, 7) == 0 => frames,
                5 => 1,
                _ => random.range(0, frames),
            };
            if used > top {
                used = random.range(1, top);
            }
        }
        plan.count = count;
        plan.first = match random.range(0, 3) {
            0 => 0,
            1 => random.range(0, 5).min(count),
            _ => random.range(0, count),
        };
        plan
    }

    #[test]
    fn handcrafted_schedules_match_the_original() {
        let mut vector = 0;
        each_float_mode(|name, mode| {
            let mut random = Random(0x0c2b_18f3_55aa_9d47 + mode);
            let mut outputs = Outputs::new();
            let mut input = vec![0f32; MAX_FRAMES];
            let state = converter(0.0, 1.0);
            for case in 0..6_000 {
                let frames = match random.range(0, 2) {
                    0 => random.range(2, 9),
                    1 => random.range(2, 200),
                    _ => random.range(2, MAX_FRAMES),
                };
                let kind = KINDS[random.range(0, KINDS.len() - 1)];
                fill(&mut random, kind, &mut input[..frames]);
                unsafe { *last_value(&state) = sample(&mut random, kind) };
                let plan = handcrafted(&mut random, frames);
                vector += vector_frames(&plan);
                let context = || format!("case {case}, {kind:?}, float mode {name}");
                outputs.compare(&plan, &state, &input[..frames], &context);
            }
        });
        assert!(
            vector > 100_000,
            "the vector block was hardly used: {vector}"
        );
    }

    #[test]
    fn four_frames_are_a_run_only_when_each_step_is_one() {
        assert!(consecutive(&[5, 6, 7, 8], 0));
        assert!(consecutive(&[9, 5, 6, 7, 8], 1));
        assert!(consecutive(&[1020, 1021, 1022, 1023], 0));
        assert!(consecutive(
            &[u16::MAX - 3, u16::MAX - 2, u16::MAX - 1, u16::MAX],
            0
        ));
        // Too few frames left.
        assert!(!consecutive(&[5, 6, 7, 8], 1));
        assert!(!consecutive(&[5, 6, 7], 0));
        assert!(!consecutive(&[], 0));
        assert!(!consecutive(&[5, 6, 7, 8], 4));
        assert!(!consecutive(&[5, 6, 7, 8], 9));
        // A repeat, a skip, and steps that still add up to 3.
        assert!(!consecutive(&[5, 5, 6, 7], 0));
        assert!(!consecutive(&[5, 6, 7, 9], 0));
        assert!(!consecutive(&[5, 6, 6, 8], 0));
        for trap in TRAPS {
            let second = 5 + trap[0];
            let third = second + trap[1];
            let used = [5, second as u16, third as u16, (third + trap[2]) as u16];
            assert_eq!(used[3], 8);
            assert!(!consecutive(&used, 0), "{used:?}");
        }
        // The index does not wrap.
        assert!(!consecutive(&[u16::MAX, 0, 1, 2], 0));
        assert!(!consecutive(&[u16::MAX - 1, u16::MAX, 0, 1], 0));
    }

    #[test]
    fn broken_counts_are_clamped() {
        let mut input = [0f32; MAX_FRAMES];
        let mut random = Random(0x77);
        fill(&mut random, Samples::Audio, &mut input);
        let state = converter(0.0, 1.0);
        unsafe { *last_value(&state) = 0.25 };
        let mut plan = Box::new(Plan::new());
        for frame in 0..MAX_FRAMES {
            plan.used[frame] = frame.max(1) as u16;
            plan.fraction[frame] = random.unit();
        }
        let blend = |fraction: f64, before: f32, after: f32| {
            let before = f64::from(before);
            fraction.mul_add(f64::from(after) - before, before) as f32
        };
        // A build that fails after its first loop leaves `first` above `count`.
        (plan.first, plan.count) = (9, 4);
        for (name, path) in PATHS {
            let mut output = [f32::from_bits(CANARY); 32];
            unsafe { path(&plan, &state, input.as_ptr(), output.as_mut_ptr()) };
            for (frame, value) in output[..4].iter().enumerate() {
                let expected = blend(plan.fraction[frame], 0.25, input[0]);
                assert_eq!(value.to_bits(), expected.to_bits(), "{name}");
            }
            assert!(
                output[4..].iter().all(|value| value.to_bits() == CANARY),
                "{name}"
            );
        }
        // No build leaves `count` above the arrays.
        (plan.first, plan.count) = (0, MAX_FRAMES + 7);
        for (name, path) in PATHS {
            let mut output = [f32::from_bits(CANARY); MAX_FRAMES + SLACK];
            unsafe { path(&plan, &state, input.as_ptr(), output.as_mut_ptr()) };
            for (frame, value) in output[..MAX_FRAMES].iter().enumerate() {
                let used = usize::from(plan.used[frame]);
                let expected = blend(plan.fraction[frame], input[used - 1], input[used]);
                assert_eq!(value.to_bits(), expected.to_bits(), "{name}");
            }
            let canaries = &output[MAX_FRAMES..];
            assert!(
                canaries.iter().all(|value| value.to_bits() == CANARY),
                "{name}"
            );
        }
    }

    /// Flush-to-zero turns a subnormal sample into zero and default-NaN drops
    /// a NaN's payload. If a mode had no effect, the tests that run under each
    /// of them would cover one mode four times.
    #[cfg(target_arch = "aarch64")]
    #[test]
    fn each_float_mode_changes_the_arithmetic() {
        use std::hint::black_box;
        let mut seen = Vec::new();
        each_float_mode(|name, _| {
            let subnormal = black_box(f32::from_bits(1));
            let payload = black_box(f32::from_bits(0x7fc0_1234));
            let tiny = blend(black_box(0.0), subnormal, black_box(0.0));
            let nan = blend(black_box(0.5), payload, black_box(1.0));
            seen.push((name.to_owned(), tiny.to_bits(), nan.to_bits()));
        });
        let expected = [
            ("default", 1, 0x7fc0_1234),
            ("FZ", 0, 0x7fc0_1234),
            ("DN", 1, 0x7fc0_0000),
            ("FZ+DN", 0, 0x7fc0_0000),
        ];
        let expected = expected.map(|(name, tiny, nan)| (name.to_owned(), tiny, nan));
        assert_eq!(seen, expected);
    }

    /// Every way the input index can repeat, advance or skip over up to eight
    /// frames, with every head: each place a run of four can start, break and
    /// end.
    #[test]
    fn every_short_schedule_matches_the_original() {
        const LONGEST: usize = 8;
        let (mut cases, mut vector) = (0u64, 0);
        each_float_mode(|name, mode| {
            let mut random = Random(0x6d5a_11c3_90e4_b7f1 + mode);
            let mut outputs = Outputs::new();
            let mut plan = Box::new(Plan::new());
            let state = converter(0.0, 1.0);
            // The index starts at 1 and gains at most 2 a frame.
            let mut input = [0f32; 2 * LONGEST];
            for count in 0..=LONGEST {
                let patterns = 3usize.pow(count.saturating_sub(1) as u32);
                for pattern in 0..patterns {
                    let (mut used, mut steps) = (1, pattern);
                    for frame in 0..count {
                        plan.used[frame] = used as u16;
                        plan.fraction[frame] = fraction(&mut random);
                        used += steps % 3;
                        steps /= 3;
                    }
                    plan.count = count;
                    let kind = KINDS[random.range(0, KINDS.len() - 1)];
                    fill(&mut random, kind, &mut input);
                    unsafe { *last_value(&state) = sample(&mut random, kind) };
                    for head in 0..=count {
                        plan.first = head;
                        vector += vector_frames(&plan);
                        let context = || {
                            format!(
                                "{count} frames, steps {pattern} in base 3, head {head}, \
                                 {kind:?}, float mode {name}"
                            )
                        };
                        outputs.compare(&plan, &state, &input, &context);
                        cases += 1;
                    }
                }
            }
        });
        // 1 + 2 + 3 * 3 + 9 * 4 + ... + 2187 * 9 schedules in each float mode.
        assert_eq!(cases % 27_885, 0, "{cases}");
        assert!(cases > 0 && vector > 0);
    }

    /// No plan holds a fraction outside 0 to 1, but the paths agree for any
    /// fraction that is a number: infinite, negative, huge or subnormal.
    #[test]
    fn any_fraction_that_is_a_number_matches_the_original() {
        let odd = [
            f64::INFINITY,
            f64::NEG_INFINITY,
            -0.0,
            f64::MAX,
            f64::MIN,
            f64::MIN_POSITIVE,
            -f64::MIN_POSITIVE,
            f64::from_bits(1),
            -f64::from_bits(1),
            f64::from_bits(0x000f_ffff_ffff_ffff),
            2.0,
            -1.0,
            1e300,
            1e-300,
        ];
        each_float_mode(|name, mode| {
            let mut random = Random(0x1f83_d9ab_fb41_bd6b + mode);
            let mut outputs = Outputs::new();
            let mut input = vec![0f32; MAX_FRAMES];
            let state = converter(0.0, 1.0);
            for case in 0..3_000 {
                let frames = [random.range(2, 9), random.range(2, 200)][random.range(0, 1)];
                let kind = KINDS[random.range(0, KINDS.len() - 1)];
                fill(&mut random, kind, &mut input[..frames]);
                unsafe { *last_value(&state) = sample(&mut random, kind) };
                let mut plan = handcrafted(&mut random, frames);
                for frame in 0..plan.count {
                    plan.fraction[frame] = match random.range(0, 3) {
                        0 => odd[random.range(0, odd.len() - 1)],
                        1 => plan.fraction[frame],
                        _ => loop {
                            let value = f64::from_bits(random.next());
                            if !value.is_nan() {
                                break value;
                            }
                        },
                    };
                }
                let context = || format!("case {case}, {kind:?}, float mode {name}");
                outputs.compare(&plan, &state, &input[..frames], &context);
            }
        });
    }

    /// `prepare` accepts an output buffer that starts where the input ends, or
    /// ends where the input starts, and declines one that shares a frame with
    /// it. The vector block loads eight input frames before it stores four
    /// output frames, so the paths agree only while that holds. Each path must
    /// write its frames and nothing else, with the two buffers touching.
    #[test]
    fn buffers_that_touch_match_the_original() {
        let mut random = Random(0x2b99_0f5e_71c4_d3a5);
        let mut outputs = Outputs::new();
        let mut plan = Box::new(Plan::new());
        let mut state = converter(0.0, 1.0);
        let mut samples = vec![0f32; MAX_FRAMES];
        let mut arena = vec![0f32; 2 * MAX_FRAMES];
        let mut image = vec![0u32; 2 * MAX_FRAMES];
        let table = VTABLE.as_ptr() as usize;
        let (mut accepted, mut filled, mut vector) = (0, 0, 0);
        for case in 0..3_000 {
            let frames = block(&mut random).max(2);
            let wanted = block(&mut random);
            let kind = KINDS[random.range(0, KINDS.len() - 1)];
            fill(&mut random, kind, &mut samples[..frames]);
            let ratio = match random.range(0, 2) {
                0 => 1.0 + (random.range(0, 800) as f64 - 400.0) / 16384.0,
                1 => wanted as f64 / frames as f64,
                _ => (random.unit() * 2.0 - 1.0).exp2(),
            }
            .clamp(MIN_RATIO, MAX_RATIO);
            state.last_position = random.unit() * 2.0;
            state.last_ratio = [ratio, 1.0][random.range(0, 1)];
            unsafe { *last_value(&state) = sample(&mut random, kind) };
            for input_first in [true, false] {
                // Frame numbers within the arena.
                let (input_at, output_at) = if input_first {
                    (0, frames)
                } else {
                    (wanted, 0)
                };
                let base = arena.as_mut_ptr();
                let request = |output: *mut f32| Data {
                    data_in: unsafe { base.add(input_at) },
                    data_out: output,
                    input_frames: frames as c_long,
                    output_frames: wanted as c_long,
                    input_frames_used: 0,
                    output_frames_gen: 0,
                    end_of_input: 0,
                    src_ratio: ratio,
                };
                // One frame nearer, the two buffers share a frame.
                let shared = if input_first {
                    output_at - 1
                } else {
                    output_at + 1
                };
                let shared = request(unsafe { base.add(shared) });
                let touching = request(unsafe { base.add(output_at) });
                let current = unsafe {
                    assert!(!prepare(
                        &mut plan,
                        &state,
                        &shared,
                        table,
                        LINEAR_VARI_OFFSET
                    ));
                    prepare(&mut plan, &state, &touching, table, LINEAR_VARI_OFFSET)
                };
                if !current {
                    continue;
                }
                accepted += 1;
                filled += usize::from(plan.count == wanted);
                vector += vector_frames(&plan);
                assert_built(&plan, frames, wanted);
                let expected = outputs.expect(&plan, &state, &samples[..frames]);
                // The arena as each path must leave it.
                image.fill(CANARY);
                for (frame, value) in samples[..frames].iter().enumerate() {
                    image[input_at + frame] = value.to_bits();
                }
                for (frame, value) in expected[..plan.count].iter().enumerate() {
                    image[output_at + frame] = value.to_bits();
                }
                for (name, path) in PATHS {
                    arena.fill(f32::from_bits(CANARY));
                    arena[input_at..input_at + frames].copy_from_slice(&samples[..frames]);
                    let base = arena.as_mut_ptr();
                    unsafe { path(&plan, &state, base.add(input_at), base.add(output_at)) };
                    let differs = |frame: &usize| arena[*frame].to_bits() != image[*frame];
                    if let Some(frame) = (0..arena.len()).find(differs) {
                        panic!(
                            "{name} left {:#010x} for {:#010x} at arena frame {frame}: case \
                             {case}, {kind:?}, input at {input_at} for {frames}, output at \
                             {output_at} for {wanted}, {} written, ratio {ratio:e}",
                            arena[frame].to_bits(),
                            image[frame],
                            plan.count
                        );
                    }
                }
            }
        }
        println!("touching: {accepted} accepted, {filled} filled the output, {vector} vector");
        assert!(accepted > 4_000, "{accepted}");
        assert!(filled > 500, "{filled}");
        assert!(vector > 100_000, "{vector}");
    }

    /// Buffers that end, or start, against memory that faults when touched.
    #[cfg(all(
        target_os = "linux",
        any(target_arch = "aarch64", target_arch = "x86_64")
    ))]
    mod guarded {
        use super::*;
        use std::ffi::{c_int, c_void};

        unsafe extern "C" {
            fn mmap(
                address: *mut c_void,
                length: usize,
                protection: c_int,
                flags: c_int,
                file: c_int,
                offset: i64,
            ) -> *mut c_void;
            fn mprotect(address: *mut c_void, length: usize, protection: c_int) -> c_int;
            fn munmap(address: *mut c_void, length: usize) -> c_int;
        }
        const PROT_NONE: c_int = 0;
        const PROT_READ_WRITE: c_int = 3;
        const MAP_PRIVATE_ANONYMOUS: c_int = 0x22;
        /// A whole number of pages at every page size these targets use.
        const SPAN: usize = 1 << 16;

        /// `SPAN` usable bytes with `SPAN` inaccessible bytes on each side.
        struct Guarded(*mut u8);

        impl Guarded {
            fn new() -> Self {
                unsafe {
                    let flags = MAP_PRIVATE_ANONYMOUS;
                    let base = mmap(std::ptr::null_mut(), 3 * SPAN, PROT_NONE, flags, -1, 0);
                    assert_ne!(base as isize, -1, "mmap failed");
                    let usable = base.cast::<u8>().add(SPAN);
                    assert_eq!(mprotect(usable.cast(), SPAN, PROT_READ_WRITE), 0);
                    Self(usable)
                }
            }
            /// `frames` samples that start at the first usable byte.
            fn head(&mut self, frames: usize) -> &mut [f32] {
                unsafe { std::slice::from_raw_parts_mut(self.0.cast(), frames) }
            }
            /// `frames` samples that end at the last usable byte.
            fn tail(&mut self, frames: usize) -> &mut [f32] {
                unsafe {
                    let start = self.0.add(SPAN - frames * 4);
                    std::slice::from_raw_parts_mut(start.cast(), frames)
                }
            }
        }

        impl Drop for Guarded {
            fn drop(&mut self) {
                unsafe { munmap(self.0.sub(SPAN).cast(), 3 * SPAN) };
            }
        }

        /// A fault here means a path read outside its input frames or wrote
        /// outside the plan's output frames.
        #[test]
        fn no_path_reads_or_writes_outside_its_buffers() {
            let (mut source, mut sink) = (Guarded::new(), Guarded::new());
            let mut random = Random(0x3a91_44d0_c0de_f00d);
            let mut outputs = Outputs::new();
            let mut samples = vec![0f32; MAX_FRAMES];
            let mut plan = Box::new(Plan::new());
            let mut state = converter(0.0, 1.0);
            let table = VTABLE.as_ptr() as usize;
            for case in 0..6_000 {
                let frames = block(&mut random).max(2);
                let kind = KINDS[random.range(0, KINDS.len() - 1)];
                fill(&mut random, kind, &mut samples[..frames]);
                let built;
                let plan: &Plan = if case % 2 == 0 {
                    built = handcrafted(&mut random, frames);
                    &built
                } else {
                    let ratio = (random.unit() * 2.0 - 1.0).exp2();
                    state.last_position = random.unit() * 2.0;
                    state.last_ratio = [ratio, 1.0][random.range(0, 1)];
                    let data = Data {
                        data_in: samples.as_ptr(),
                        data_out: outputs.actual.as_mut_ptr(),
                        input_frames: frames as c_long,
                        output_frames: block(&mut random) as c_long,
                        input_frames_used: 0,
                        output_frames_gen: 0,
                        end_of_input: 0,
                        src_ratio: ratio,
                    };
                    if !unsafe { prepare(&mut plan, &state, &data, table, LINEAR_VARI_OFFSET) } {
                        continue;
                    }
                    &plan
                };
                let count = plan.count;
                let context = || format!("case {case}, {kind:?}");
                outputs.expect(plan, &state, &samples[..frames]);
                for at_end in [true, false] {
                    let (input, output) = if at_end {
                        (source.tail(frames), sink.tail(count))
                    } else {
                        (source.head(frames), sink.head(count))
                    };
                    input.copy_from_slice(&samples[..frames]);
                    for (name, path) in PATHS {
                        output.fill(f32::from_bits(CANARY));
                        unsafe { path(plan, &state, input.as_ptr(), output.as_mut_ptr()) };
                        outputs.same(plan, output, name, &context);
                    }
                }
            }
        }
    }
}
