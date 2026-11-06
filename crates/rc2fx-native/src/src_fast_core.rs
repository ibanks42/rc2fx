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

/// Writes the plan's output frames to `output`.
///
/// # Safety
/// `plan` must be current for `state` and `input`, and `output` must hold
/// `plan.count()` frames.
#[inline]
pub unsafe fn interpolate(plan: &Plan, state: *const State, input: *const f32, output: *mut f32) {
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
