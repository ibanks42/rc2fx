//! Hash-qualified microphone-stem capture inside RØDE's own Chat routing.
//!
//! Read the already processed per-fader buffers; mirror only the linear sums,
//! mix-minus subtraction and gain ramps. Never change a vendor object, replay a
//! vendor processor, infer a pad waveform, or feed the completed mix to a model.
#![cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]
use crate::pipeline::FRAMES;
use std::{
    cell::Cell,
    sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};
type Stereo = [[f32; FRAMES]; 2];
const ZERO: Stereo = [[0.; FRAMES]; 2];
const MIXES: usize = 0xc0000;
const STREAM: usize = MIXES + 0x1fb0;
const CUSTOM_CHAT: usize = MIXES + 0x1ff8;
const MAIN_OUTPUT: usize = MIXES + 0x2060;
const CHAT_OUTPUT: usize = MIXES + 0x2088;
const MAIN_BUFFER: usize = 0xc6f28;
const EMPTY_BUFFER: usize = 0xc6cd8;
const MEDIA: [usize; 2] = [0xc72a0, 0xc74f0];

static INSTALLED: AtomicBool = AtomicBool::new(false);
static CAPTURED: AtomicU64 = AtomicU64::new(0);
static REFUSED: AtomicU64 = AtomicU64::new(0);
static ORIGINAL_MIXER: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_STREAM: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_SUM: AtomicUsize = AtomicUsize::new(0);
static ORIGINAL_CHAT: AtomicUsize = AtomicUsize::new(0);
thread_local! {
    static ACTIVE: Cell<*mut Capture> = const { Cell::new(std::ptr::null_mut()) };
    static MIXER: Cell<usize> = const { Cell::new(0) };
}

/// Owned by the callback's Chat Port. All arrays are allocated with the pipeline,
/// not while capturing. A capture is usable only in the frame that armed it.
pub struct Capture {
    wanted: [usize; 2],
    stream: Option<Stereo>,
    solo: Option<Stereo>,
    main: Option<Stereo>,
    chat: Option<Stereo>,
}
impl Capture {
    pub fn new() -> Self {
        Self {
            wanted: [0; 2],
            stream: None,
            solo: None,
            main: None,
            chat: None,
        }
    }
    /// # Safety
    /// Keep this capture at a fixed address until disarm, on this callback
    /// thread. Pipeline owns it in a Box and brackets the vendor call.
    pub unsafe fn arm(&mut self, wanted: [usize; 2]) {
        self.wanted = wanted;
        self.stream = None;
        self.solo = None;
        self.main = None;
        self.chat = None;
        ACTIVE.with(|active| active.set(self));
    }
    pub fn voice(&self) -> Option<[&[f32]; 2]> {
        self.chat
            .as_ref()
            .map(|voice| [voice[0].as_slice(), voice[1].as_slice()])
    }
}
pub fn disarm() {
    ACTIVE.with(|active| active.set(std::ptr::null_mut()));
}
pub fn installed() -> bool {
    INSTALLED.load(Ordering::Acquire)
}
pub fn status() -> serde_json::Value {
    serde_json::json!({"installed": installed(), "captured": CAPTURED.load(Ordering::Relaxed), "refused": REFUSED.load(Ordering::Relaxed)})
}
fn active() -> Option<*mut Capture> {
    ACTIVE.try_with(Cell::get).ok().filter(|p| !p.is_null())
}
fn root() -> Option<usize> {
    MIXER.try_with(Cell::get).ok().filter(|p| *p != 0)
}

// All memory access below is confined to a synchronous, pinned vendor call.
// Its objects/buffers have the same lifetime as the original function's own
// reads. Shape checks are defense in depth, not permission to read arbitrary
// addresses. Do not call these with user-provided pointers.
unsafe fn u32_at(address: usize) -> u32 {
    unsafe { (address as *const u32).read_unaligned() }
}
unsafe fn u8_at(address: usize) -> u8 {
    unsafe { (address as *const u8).read() }
}
unsafe fn f32_at(address: usize) -> f32 {
    f32::from_bits(unsafe { u32_at(address) })
}
unsafe fn pointer(address: usize) -> Option<usize> {
    let value = unsafe { (address as *const usize).read_unaligned() };
    (value >= 4096 && value.is_multiple_of(8)).then_some(value)
}
unsafe fn vector(object: usize, maximum: usize) -> Option<(usize, usize)> {
    let first = unsafe { ((object + 8) as *const usize).read_unaligned() };
    let end = unsafe { ((object + 16) as *const usize).read_unaligned() };
    if first == end {
        return Some((first, 0));
    }
    if first < 4096 || !first.is_multiple_of(8) || !end.is_multiple_of(8) {
        return None;
    }
    let count = end.checked_sub(first)?.checked_div(8)?;
    (count <= maximum).then_some((first, count))
}
unsafe fn channels(buffer: usize) -> Option<[usize; 2]> {
    if unsafe { u32_at(buffer) } != 2 || unsafe { u32_at(buffer + 4) } != FRAMES as u32 {
        return None;
    }
    let table = unsafe { pointer(buffer + 16) }?;
    let left = unsafe { pointer(table) }?;
    let right = unsafe { pointer(table + 8) }?;
    // Read-only mono sources may alias L/R. The actual output pointer pair is
    // separately matched to the non-overlapping JACK buffers resolved by Port.
    Some([left, right])
}
unsafe fn audio(buffer: usize) -> Option<Stereo> {
    // JUCE may leave a cleared/unused buffer without allocated channel data.
    // The original sum tests this flag before reading its table, too.
    if unsafe { u8_at(buffer + 0x120) } != 0 {
        return Some(ZERO);
    }
    let pointers = unsafe { channels(buffer) }?;
    let mut value = ZERO;
    for ch in 0..2 {
        let src = unsafe { std::slice::from_raw_parts(pointers[ch] as *const f32, FRAMES) };
        if src.iter().any(|v| !v.is_finite()) {
            return None;
        }
        value[ch].copy_from_slice(src);
    }
    Some(value)
}
fn microphone(source: u32) -> bool {
    matches!(source, 0..=6 | 19..=20)
}
unsafe fn strip_source(strip: usize) -> Option<bool> {
    if let Some(input) = unsafe { pointer(strip + 0x460) } {
        Some(microphone(unsafe { u32_at(input + 0xe4) }))
    } else if unsafe { u32_at(strip + 0x658) } == 0 || unsafe { u32_at(strip + 0x848) } == 0 {
        // An unassigned, muted strip contributes zero without a source object.
        Some(false)
    } else {
        None
    }
}
/// Match identities, not signal contents. Pads/USB/Bluetooth cannot be mistaken
/// for microphones even when playing speech or the same samples as a mic.
unsafe fn classify(mix: usize, buffer: usize) -> Option<bool> {
    for i in 0..9 {
        let strip = unsafe { pointer(mix + 0x148 + i * 8) }?;
        if [strip + 0x210, strip + 0x338].contains(&buffer) {
            if unsafe { u8_at(buffer + 0x120) } != 0 {
                return Some(false);
            }
            return unsafe { strip_source(strip) };
        }
        if let Some(input) = unsafe { pointer(strip + 0x460) }
            && buffer == input + 0x350
        {
            return Some(microphone(unsafe { u32_at(input + 0xe4) }));
        }
    }
    None
}
unsafe fn mix_voice(mix: usize) -> Option<Stereo> {
    let (first, count) = unsafe { vector(mix, 9) }?;
    let mut result = ZERO;
    for i in 0..count {
        let buffer = unsafe { pointer(first + i * 8) }?;
        if unsafe { classify(mix, buffer) }? {
            add(&mut result, &unsafe { audio(buffer) }?);
        }
    }
    Some(result)
}
fn add(to: &mut Stereo, value: &Stereo) {
    for ch in 0..2 {
        for i in 0..FRAMES {
            to[ch][i] += value[ch][i];
        }
    }
}
fn subtract(mut left: Stereo, right: Stereo) -> Stereo {
    for ch in 0..2 {
        for i in 0..FRAMES {
            left[ch][i] -= right[ch][i];
        }
    }
    left
}

#[derive(Clone, Copy, Default)]
struct Gain {
    desired: f32,
    current: f32,
    a: f32,
    b: f32,
    stride: usize,
}
impl Gain {
    unsafe fn read(
        object: usize,
        desired: usize,
        current: usize,
        coefficients: usize,
        stride: usize,
    ) -> Option<Self> {
        let g = Self {
            desired: unsafe { f32_at(object + desired) },
            current: unsafe { f32_at(object + current) },
            a: unsafe { f32_at(object + coefficients) },
            b: unsafe { f32_at(object + coefficients + 4) },
            stride,
        };
        [g.desired, g.current, g.a, g.b]
            .iter()
            .all(|v| v.is_finite() && v.abs() <= 64.)
            .then_some(g)
    }
    fn next(&mut self, sample: usize) -> f32 {
        if sample.is_multiple_of(self.stride) && self.desired != self.current {
            self.current = self.desired.mul_add(self.a, self.current * self.b);
        }
        self.current
    }
}
#[derive(Clone, Copy, Default)]
struct Solo {
    buffer: usize,
    microphone: bool,
    state: u32,
    gain: Gain,
}
unsafe fn solo_snapshot(stream: usize) -> Option<[Solo; 9]> {
    let mut result = [Solo::default(); 9];
    for (i, item) in result.iter_mut().enumerate() {
        let entry = unsafe { pointer(stream + 0x438 + i * 8) }?;
        let strip = unsafe { pointer(entry + 8) }?;
        item.microphone = unsafe { strip_source(strip) }?;
        item.state = unsafe { u32_at(entry + 0x338) };
        if item.state > 2 {
            return None;
        }
        if item.state == 0 || !item.microphone {
            continue;
        }
        item.buffer = unsafe { pointer(entry + 0x340) }?;
        if item.state == 2 {
            item.gain = unsafe { Gain::read(entry, 0x30c, 0x310, 0x230, 1) }?;
        }
    }
    Some(result)
}
unsafe fn solo_voice(snapshot: [Solo; 9]) -> Option<Stereo> {
    let mut result = ZERO;
    for mut item in snapshot {
        if !item.microphone || item.state == 0 {
            continue;
        }
        let input = unsafe { audio(item.buffer) }?;
        for i in 0..FRAMES {
            let gain = if item.state == 1 {
                1.
            } else {
                item.gain.next(i)
            };
            for ch in 0..2 {
                result[ch][i] = if item.state == 1 {
                    result[ch][i] + input[ch][i]
                } else {
                    input[ch][i].mul_add(gain, result[ch][i])
                };
            }
        }
    }
    Some(result)
}
/// Mirror SummingMix::Process's contribution gains BEFORE its original call
/// advances ramp state. The original alone emits property notifications.
unsafe fn sum_voice(
    object: usize,
    mut source: impl FnMut(usize) -> Option<Stereo>,
) -> Option<Stereo> {
    let (first, count) = unsafe { vector(object, 6) }?;
    let mut result = ZERO;
    for i in 0..count {
        let entry = unsafe { pointer(first + i * 8) }?;
        let state = unsafe { u32_at(entry + 0x1e8) };
        if state > 3 {
            return None;
        }
        if state == 1 {
            continue;
        }
        let buffer = unsafe { pointer(entry + 0xd0) }?;
        let input = source(buffer)?;
        if state == 3 {
            add(&mut result, &input);
            continue;
        }
        let mut gain = unsafe { Gain::read(entry, 0x1bc, 0x1c0, 0xe0, 4) }?;
        for i in 0..FRAMES {
            let g = gain.next(i);
            for ch in 0..2 {
                result[ch][i] += input[ch][i] * g;
            }
        }
    }
    Some(result)
}
unsafe fn return_voice(mixer: usize, buffer: usize) -> Option<Stereo> {
    if buffer == mixer + EMPTY_BUFFER {
        return Some(ZERO);
    }
    let stream = unsafe { pointer(mixer + STREAM) }?;
    match unsafe { classify(stream, buffer) } {
        Some(true) => unsafe { audio(buffer) },
        Some(false) => Some(ZERO),
        None => None,
    }
}
unsafe fn chat_voice(capture: &Capture, mixer: usize, object: usize) -> Option<Stereo> {
    let custom = unsafe { pointer(mixer + CUSTOM_CHAT) }?;
    let stream = unsafe { pointer(mixer + STREAM) }?;
    let mut result = unsafe {
        sum_voice(object, |buffer| {
            if buffer == mixer + MAIN_BUFFER {
                return capture.main;
            }
            if buffer == custom + 0x20 {
                return mix_voice(custom);
            }
            if buffer == object + 0x2a0 {
                let a = pointer(object + 0x278)?;
                if a != mixer + MAIN_BUFFER {
                    return None;
                }
                return Some(subtract(
                    capture.main?,
                    return_voice(mixer, pointer(object + 0x298)?)?,
                ));
            }
            if buffer == object + 0x3c8 {
                let a = pointer(object + 0x280)?;
                if a != stream + 0x190 {
                    return None;
                }
                return Some(subtract(
                    capture.solo?,
                    return_voice(mixer, pointer(object + 0x290)?)?,
                ));
            }
            if MEDIA.iter().any(|offset| buffer == mixer + offset) {
                return Some(ZERO);
            }
            None
        })
    }?;
    // MixMinusStereo's complete-output mute and gain follow its sum.
    if (unsafe { u8_at(object + 0x502) } != 0 && unsafe { u8_at(object + 0x501) } == 0)
        || unsafe { u32_at(object + 0x268) } == 0
    {
        return Some(ZERO);
    }
    let state = unsafe { u32_at(object + 0x268) };
    if state > 2 {
        return None;
    }
    let mut gain = if state == 1 {
        let desired = unsafe { f32_at(object + 0x23c) };
        if !desired.is_finite() || desired.abs() > 64. {
            return None;
        }
        Gain {
            desired,
            ..Gain::default()
        }
    } else {
        unsafe { Gain::read(object, 0x23c, 0x240, 0x160, 4) }?
    };
    for i in 0..FRAMES {
        let g = if state == 1 {
            gain.desired
        } else {
            gain.next(i)
        };
        for channel in &mut result {
            channel[i] *= g;
        }
    }
    Some(result)
}

// Unlike the JACK-facing callback, Mixer::Process has its frame count in w4.
// The pinned parent stores w4 at sp+0x1b4, later captured by its queued job.
// Its separate w5 parameter must be forwarded unchanged, not used as a size.
type MixerFn = unsafe extern "C-unwind" fn(usize, usize, i32, usize, i32, u32, i64, i64);
type StreamFn = unsafe extern "C-unwind" fn(usize, i32);
type SumFn = unsafe extern "C-unwind" fn(usize, usize);
type ChatFn = unsafe extern "C-unwind" fn(usize, usize, usize);
unsafe extern "C-unwind" fn mixer_hook(
    mixer: usize,
    inputs: usize,
    ni: i32,
    outputs: usize,
    frames: i32,
    vendor_parameter: u32,
    a: i64,
    b: i64,
) {
    let original: MixerFn = unsafe { std::mem::transmute(ORIGINAL_MIXER.load(Ordering::Acquire)) };
    struct Restore(usize);
    impl Drop for Restore {
        fn drop(&mut self) {
            MIXER.with(|v| v.set(self.0));
        }
    }
    let _restore = Restore(MIXER.with(|v| {
        v.replace(if frames as usize == FRAMES && active().is_some() {
            mixer
        } else {
            0
        })
    }));
    unsafe {
        original(mixer, inputs, ni, outputs, frames, vendor_parameter, a, b);
    }
}
unsafe extern "C-unwind" fn stream_hook(object: usize, frames: i32) {
    let original: StreamFn =
        unsafe { std::mem::transmute(ORIGINAL_STREAM.load(Ordering::Acquire)) };
    let wanted = root().zip(active()).filter(|(mixer, _)| {
        frames as usize == FRAMES && unsafe { pointer(mixer + STREAM) } == Some(object)
    });
    let snapshot = wanted.and_then(|_| unsafe { solo_snapshot(object) });
    unsafe {
        original(object, frames);
    }
    if let Some((_, capture)) = wanted {
        let stream = unsafe { mix_voice(object) };
        let solo = snapshot.and_then(|v| unsafe { solo_voice(v) });
        // Never hold a Rust reference across a vendor call (which may invoke another hook).
        let capture = unsafe { &mut *capture };
        capture.stream = stream;
        capture.solo = solo;
    }
}
unsafe extern "C-unwind" fn sum_hook(object: usize, buffer: usize) {
    let original: SumFn = unsafe { std::mem::transmute(ORIGINAL_SUM.load(Ordering::Acquire)) };
    let wanted = root().zip(active()).filter(|(mixer,_)| unsafe { pointer(mixer + MAIN_OUTPUT) } == Some(object) && buffer == mixer + MAIN_BUFFER);
    let voice = wanted.and_then(|(mixer, capture)| {
        let stream = unsafe { pointer(mixer + STREAM) }?;
        unsafe {
            sum_voice(object, |source| {
                if source == stream + 0x20 {
                    return (*capture).stream;
                }
                if MEDIA.iter().any(|offset| source == mixer + offset) {
                    return Some(ZERO);
                }
                None
            })
        }
    });
    unsafe {
        original(object, buffer);
    }
    if let Some((_, capture)) = wanted {
        unsafe {
            (*capture).main = voice;
        }
    }
}
unsafe extern "C-unwind" fn chat_hook(object: usize, buffer: usize, extra: usize) {
    let original: ChatFn = unsafe { std::mem::transmute(ORIGINAL_CHAT.load(Ordering::Acquire)) };
    let wanted = root().zip(active()).filter(|(mixer,capture)|
        unsafe { pointer(mixer + CHAT_OUTPUT) } == Some(object)
            && unsafe { channels(buffer) } == Some(unsafe { (**capture).wanted }));
    let voice = wanted.and_then(|(mixer, capture)| unsafe { chat_voice(&*capture, mixer, object) });
    unsafe {
        original(object, buffer, extra);
    }
    if let Some((_, capture)) = wanted {
        if voice.is_some() {
            CAPTURED.fetch_add(1, Ordering::Relaxed);
        } else {
            REFUSED.fetch_add(1, Ordering::Relaxed);
        }
        unsafe {
            (*capture).chat = voice;
        }
    }
}

const PROLOGUES: [[u32; 4]; 4] = [
    [0xd503233f, 0xd12983ff, 0xa9007bfd, 0x910003fd],
    [0xd503233f, 0xa9b87bfd, 0x910003fd, 0xa9025bf5],
    [0xd503233f, 0xa9b77bfd, 0x910003fd, 0xa90153f3],
    [0xd503233f, 0xa9b97bfd, 0x910003fd, 0x39540803],
];
fn sites(hash: &str) -> Option<[usize; 4]> {
    match hash {
        "897b9e25919d826ade563cad89a5acd140541b274b74524f655273488089f48c" => {
            Some([0x657250, 0x424fe0, 0x42de60, 0x42e3f0])
        }
        "5151d0eb843dab46f441da4c2cca66f1636bfe522b2302345c17f28ecbffd0dd" => {
            Some([0x657470, 0x428020, 0x430ea0, 0x431430])
        }
        _ => None,
    }
}
#[cfg(target_arch = "aarch64")]
pub fn install(base: usize, profile: &rc2fx_firmware::Profile) -> Result<(), String> {
    let sites = sites(&profile.mixer_sha256)
        .ok_or("Chat voice layout is not qualified for this mixer image")?;
    if installed() {
        return Err("Chat voice hooks already installed".into());
    }
    if std::fs::read_dir("/proc/self/task")
        .map_err(|e| e.to_string())?
        .count()
        != 1
    {
        return Err("Chat voice hooks require pre-thread initialization".into());
    }
    // select_firmware verified the complete image. Also refuse any changed live
    // prologue before patching the first site. All displaced instructions are
    // PC-independent, established by the offline two-image audit.
    for (offset, words) in sites.iter().zip(PROLOGUES) {
        let expected: Vec<u8> = words.into_iter().flat_map(u32::to_le_bytes).collect();
        let address = base
            .checked_add(*offset)
            .ok_or("Chat hook address overflow")?;
        if unsafe { std::slice::from_raw_parts(address as *const u8, 16) } != expected {
            return Err(format!("Chat hook prologue changed at {offset:x}"));
        }
    }
    for (offset, (destination, original)) in sites.into_iter().zip([
        (mixer_hook as *const () as usize, &ORIGINAL_MIXER),
        (stream_hook as *const () as usize, &ORIGINAL_STREAM),
        (sum_hook as *const () as usize, &ORIGINAL_SUM),
        (chat_hook as *const () as usize, &ORIGINAL_CHAT),
    ]) {
        unsafe {
            crate::vendor_job::install_detour((base + offset) as *mut u8, destination, original)?;
        }
    }
    INSTALLED.store(true, Ordering::Release);
    Ok(())
}
#[cfg(not(target_arch = "aarch64"))]
pub fn install(_: usize, _: &rc2fx_firmware::Profile) -> Result<(), String> {
    Err("Chat voice hooks require the qualified ARM64 image".into())
}

#[cfg(test)]
mod fixtures;
#[cfg(all(test, target_arch = "aarch64"))]
mod trampoline_tests;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_identity_excludes_pads_and_all_playback() {
        for source in 0..=20 {
            assert_eq!(microphone(source), matches!(source,0..=6|19..=20));
        }
        assert!(!microphone(11));
        assert!(!microphone(u32::MAX));
    }
    #[test]
    fn capture_is_frame_scoped_and_unknown_firmware_is_refused() {
        let mut capture = Capture::new();
        capture.chat = Some([[0.25; FRAMES]; 2]);
        unsafe {
            capture.arm([4096, 8192]);
        }
        assert!(capture.voice().is_none());
        disarm();
        assert!(active().is_none());
        assert!(sites("unknown").is_none());
    }
    #[test]
    fn gain_ramp_uses_the_vendors_four_sample_steps() {
        let mut gain = Gain {
            desired: 1.,
            current: 0.,
            a: 0.25,
            b: 0.75,
            stride: 4,
        };
        let values: Vec<_> = (0..12).map(|i| gain.next(i)).collect();
        assert_eq!(&values[..4], &[0.25; 4]);
        assert_eq!(&values[4..8], &[0.4375; 4]);
        assert_eq!(&values[8..], &[0.578125; 4]);
    }
}
