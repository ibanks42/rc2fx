//! Compares every byte of `src_fast_core` output and converter state with the
//! real libsamplerate over randomised and vendor-shaped calls. Exits non-zero
//! at the first difference.
//!
//! Both interpolations are compared with the library on every call: the one
//! the mixer runs, which is NEON on AArch64, and the scalar reference.
#[allow(dead_code)]
#[path = "../../../crates/rc2fx-native/src/src_fast_core.rs"]
mod core;

use core::{Data, Linear, Plan, State};
use std::ffi::{CStr, c_char, c_int, c_long, c_void};

#[repr(C)]
struct DlInfo {
    name: *const c_char,
    base: *mut c_void,
    symbol: *const c_char,
    address: *mut c_void,
}
unsafe extern "C" {
    fn dlopen(path: *const c_char, flags: c_int) -> *mut c_void;
    fn dlsym(handle: *mut c_void, name: *const c_char) -> *mut c_void;
    fn dladdr(address: *const c_void, info: *mut DlInfo) -> c_int;
}
type New = unsafe extern "C" fn(c_int, c_int, *mut c_int) -> *mut State;
type Reset = unsafe extern "C" fn(*mut State) -> c_int;
type Process = unsafe extern "C" fn(*mut State, *mut Data) -> c_int;

struct Library {
    new: New,
    reset: Reset,
    process: Process,
    vt: usize,
    vari: usize,
}

fn open() -> Library {
    unsafe {
        let handle = dlopen(c"libsamplerate.so.0".as_ptr(), 2);
        assert!(!handle.is_null(), "libsamplerate not found");
        let symbol = |name: &CStr| {
            let found = dlsym(handle, name.as_ptr());
            assert!(!found.is_null());
            found
        };
        let process = symbol(c"src_process");
        let mut info = std::mem::zeroed::<DlInfo>();
        assert!(dladdr(process, &mut info) != 0);
        let base = info.base as usize;
        let text =
            std::slice::from_raw_parts((base + core::TEXT_OFFSET) as *const u8, core::TEXT_SIZE);
        let fnv = text.iter().fold(0xcbf2_9ce4_8422_2325u64, |hash, byte| {
            (hash ^ u64::from(*byte)).wrapping_mul(0x100_0000_01b3)
        });
        assert_eq!(fnv, core::TEXT_FNV, "not the verified library");
        Library {
            new: std::mem::transmute::<*mut c_void, New>(symbol(c"src_new")),
            reset: std::mem::transmute::<*mut c_void, Reset>(symbol(c"src_reset")),
            process: std::mem::transmute::<*mut c_void, Process>(process),
            vt: base + core::LINEAR_VT_OFFSET,
            vari: base + core::LINEAR_VARI_OFFSET,
        }
    }
}

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
    fn range(&mut self, low: i64, high: i64) -> i64 {
        low + (self.next() % (high - low + 1) as u64) as i64
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Samples {
    Audio,
    AnyBits,
    Tiny,
    Special,
}

/// Signed zeros, infinities, quiet and signalling NaNs with several payloads,
/// and the largest and smallest normal and subnormal values. Random bits
/// almost never give a zero or an infinity, and seldom two NaNs side by side.
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

fn fill(random: &mut Random, kind: Samples, input: &mut [f32]) {
    for sample in input {
        *sample = match kind {
            Samples::Audio => (random.unit() * 2.0 - 1.0) as f32,
            Samples::AnyBits => f32::from_bits(random.next() as u32),
            Samples::Tiny => f32::from_bits((random.next() as u32) & 0x807f_ffff),
            // Half audio, half from the table, so that a special value meets
            // both ordinary samples and other special values.
            Samples::Special => match random.next() {
                bits if bits & 1 == 0 => (random.unit() * 2.0 - 1.0) as f32,
                bits => f32::from_bits(SPECIAL[(bits >> 1) as usize % SPECIAL.len()]),
            },
        };
    }
}

/// A converter pair: the library's own and the one `core` drives.
struct Pair {
    real: *mut State,
    fast: *mut State,
}

#[derive(Default)]
struct Counts {
    calls: u64,
    reused: u64,
    declined: u64,
    frames: u64,
}

fn compare(pair: &Pair, want: &Data, got: &Data, expected: &[f32], actual: &[f32], what: &str) {
    unsafe {
        let (a, b) = (&*pair.real, &*pair.fast);
        let (x, y): (&Linear, &Linear) = (&*a.private_data, &*b.private_data);
        let same = want.input_frames_used == got.input_frames_used
            && want.output_frames_gen == got.output_frames_gen
            && a.last_ratio.to_bits() == b.last_ratio.to_bits()
            && a.last_position.to_bits() == b.last_position.to_bits()
            && (*x.last_value).to_bits() == (*y.last_value).to_bits()
            && x.dirty == y.dirty
            && x.in_count == y.in_count
            && x.in_used == y.in_used
            && x.out_count == y.out_count
            && x.out_gen == y.out_gen;
        let frames = want.output_frames_gen as usize;
        let difference = (0..frames).find(|i| expected[*i].to_bits() != actual[*i].to_bits());
        if !same || difference.is_some() {
            println!("MISMATCH in {what}: frame {difference:?}");
            println!(
                "used {} {} gen {} {} ratio {:e} {:e} position {:e} {:e} request in {} out {} target {:e}",
                want.input_frames_used,
                got.input_frames_used,
                want.output_frames_gen,
                got.output_frames_gen,
                a.last_ratio,
                b.last_ratio,
                a.last_position,
                b.last_position,
                want.input_frames,
                want.output_frames,
                want.src_ratio
            );
            std::process::exit(1);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn call(
    library: &Library,
    plan: &mut Plan,
    pair: &Pair,
    input: &[f32],
    output_frames: c_long,
    ratio: f64,
    buffers: &mut Buffers,
    counts: &mut Counts,
    what: &str,
) {
    let template = |out: *mut f32| Data {
        data_in: input.as_ptr(),
        data_out: out,
        input_frames: input.len() as c_long,
        output_frames,
        input_frames_used: 0,
        output_frames_gen: 0,
        end_of_input: 0,
        src_ratio: ratio,
    };
    let mut want = template(buffers.real.as_mut_ptr());
    let mut got = template(buffers.fast.as_mut_ptr());
    let status = unsafe { (library.process)(pair.real, &mut want) };
    assert_eq!(status, 0, "library error in {what}");
    let before = unsafe { (*pair.fast).last_position.to_bits() };
    unsafe {
        if core::prepare(plan, pair.fast, &got, library.vt, library.vari) {
            let frames = plan.count();
            for output in [&mut buffers.fast, &mut buffers.scalar] {
                output[frames..frames + GUARD].fill(f32::from_bits(CANARY));
            }
            core::interpolate(plan, pair.fast, got.data_in, got.data_out);
            core::interpolate_scalar(plan, pair.fast, got.data_in, buffers.scalar.as_mut_ptr());
            core::commit(plan, pair.fast, &mut got);
            reference(buffers, frames, what);
        } else {
            counts.declined += 1;
            assert_eq!((library.process)(pair.fast, &mut got), 0);
        }
    }
    let _ = before;
    counts.calls += 1;
    counts.frames += want.output_frames_gen as u64;
    compare(pair, &want, &got, &buffers.real, &buffers.fast, what);
}

/// The scalar reference must write what the library wrote, and neither
/// interpolation may write past the plan's frames.
fn reference(buffers: &Buffers, frames: usize, what: &str) {
    let bits = |samples: &[f32], frame: usize| samples[frame].to_bits();
    let difference = (0..frames).find(|i| bits(&buffers.real, *i) != bits(&buffers.scalar, *i));
    if difference.is_some() {
        println!("MISMATCH in {what}: frame {difference:?} of the scalar reference");
        std::process::exit(1);
    }
    for (name, output) in [("fast", &buffers.fast), ("scalar", &buffers.scalar)] {
        if (frames..frames + GUARD).any(|i| bits(output, i) != CANARY) {
            println!("OVERRUN in {what}: {name} wrote past frame {frames}");
            std::process::exit(1);
        }
    }
}

fn pairs(library: &Library, count: usize) -> Vec<Pair> {
    (0..count)
        .map(|_| unsafe {
            let mut error = 0;
            let real = (library.new)(4, 1, &mut error);
            let fast = (library.new)(4, 1, &mut error);
            assert!(!real.is_null() && !fast.is_null());
            Pair { real, fast }
        })
        .collect()
}

fn reset(library: &Library, pair: &Pair) {
    unsafe {
        (library.reset)(pair.real);
        (library.reset)(pair.fast);
    }
}

/// Output of the library, of `core::interpolate` and of
/// `core::interpolate_scalar` for one call.
struct Buffers {
    real: Vec<f32>,
    fast: Vec<f32>,
    scalar: Vec<f32>,
}

/// Samples after the plan's frames that must stay as they were.
const GUARD: usize = 8;
const CANARY: u32 = 0x5a5a_a5a5;

fn buffers() -> Buffers {
    let buffer = || vec![0.0; core::MAX_FRAMES * 2];
    Buffers {
        real: buffer(),
        fast: buffer(),
        scalar: buffer(),
    }
}

/// The vendor's shape: channels in step, one ratio per cycle on a fine grid.
fn vendor(
    library: &Library,
    seed: u64,
    channels: usize,
    cycles: usize,
    kind: Samples,
    counts: &mut Counts,
) {
    let mut random = Random(seed);
    let converters = pairs(library, channels);
    let mut plan = Box::new(Plan::new());
    let mut io = buffers();
    let mut input = vec![0f32; 256];
    let mut step = 0i64;
    for cycle in 0..cycles {
        if random.range(0, 3) == 0 {
            step = (step + random.range(-2, 2)).clamp(-400, 400);
        }
        let ratio = (step as f64 / 16384.0 + 1.0) as f32 as f64;
        let frames = [112usize, 96, 49, 196][(seed % 4) as usize] + random.range(0, 1) as usize;
        let output = (frames as f64 * ratio).ceil() as c_long + random.range(0, 2);
        if cycle % 977 == 0 {
            for pair in &converters {
                reset(library, pair);
            }
        }
        for pair in &converters {
            fill(&mut random, kind, &mut input[..frames]);
            let before = counts.declined;
            call(
                library,
                &mut plan,
                pair,
                &input[..frames],
                output,
                ratio,
                &mut io,
                counts,
                "vendor",
            );
            let _ = before;
        }
    }
}

/// Anything the public interface allows, one converter at a time.
fn wide(library: &Library, seed: u64, cycles: usize, kind: Samples, counts: &mut Counts) {
    let mut random = Random(seed);
    let converters = pairs(library, 3);
    let mut plan = Box::new(Plan::new());
    let mut io = buffers();
    let mut input = vec![0f32; core::MAX_FRAMES + 8];
    let mut ratio = 1.0f64;
    for cycle in 0..cycles {
        let pair = &converters[cycle % 3];
        match random.range(0, 7) {
            0 => ratio = (random.unit() * 16.0 - 8.0).exp2(),
            1 => ratio *= 1.0 + (random.unit() - 0.5) * 1e-15,
            2 => ratio *= 1.0 + (random.unit() - 0.5) * 1e-3,
            3 => ratio = 1.0,
            _ => {}
        }
        ratio = ratio.clamp(1.0 / 256.0, 256.0);
        if random.range(0, 63) == 0 {
            reset(library, pair);
        }
        let frames = match random.range(0, 3) {
            0 => random.range(1, 4),
            1 => random.range(1, 128),
            _ => random.range(1, core::MAX_FRAMES as i64 + 4),
        } as usize;
        let output = match random.range(0, 3) {
            0 => random.range(1, 4),
            1 => random.range(1, 300),
            _ => random.range(1, core::MAX_FRAMES as i64 + 4),
        };
        fill(&mut random, kind, &mut input[..frames]);
        call(
            library,
            &mut plan,
            pair,
            &input[..frames],
            output,
            ratio,
            &mut io,
            counts,
            "wide",
        );
    }
}

#[cfg(target_arch = "aarch64")]
fn flush_to_zero(on: bool) {
    unsafe {
        let mut control: u64;
        std::arch::asm!("mrs {}, fpcr", out(reg) control);
        control = if on {
            control | 1 << 24
        } else {
            control & !(1 << 24)
        };
        std::arch::asm!("msr fpcr, {}", in(reg) control);
    }
}

fn main() {
    let library = open();
    let cycles: usize = std::env::args()
        .nth(1)
        .and_then(|v| v.parse().ok())
        .unwrap_or(20_000);
    let mut counts = Counts::default();
    for flush in [false, true] {
        #[cfg(target_arch = "aarch64")]
        flush_to_zero(flush);
        let _ = flush;
        for (seed, kind) in [
            (0x1234_5671u64, Samples::Audio),
            (0x1234_5672, Samples::Audio),
            (0x1234_5673, Samples::AnyBits),
            (0x1234_5674, Samples::Tiny),
            (0x1234_5675, Samples::Special),
        ] {
            vendor(&library, seed, 20, cycles / 20, kind, &mut counts);
            vendor(&library, seed ^ 0xff, 2, cycles, kind, &mut counts);
            wide(&library, seed ^ 0xabcd, cycles * 4, kind, &mut counts);
        }
    }
    println!(
        "OK {} calls, {} frames bit-identical, {} declined to the library",
        counts.calls, counts.frames, counts.declined
    );
    let vector = cfg!(target_arch = "aarch64");
    println!(
        "compared with the library: interpolate ({}) and interpolate_scalar",
        if vector { "NEON" } else { "scalar" }
    );
    let _ = counts.reused;
}
