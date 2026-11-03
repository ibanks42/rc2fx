use rc2fx_core::{Band, BandType, Effect, Target, TargetKind};
use rc2fx_dsp::{Dsp, DspError, Prepared, RetirementError, Suppression};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Arc;
use std::sync::atomic::AtomicU32;

thread_local! {static COUNT:Cell<Option<usize>>=const{Cell::new(None)};}
struct Counting;
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        COUNT.with(|c| {
            if let Some(n) = c.get() {
                c.set(Some(n + 1))
            }
        });
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new: usize) -> *mut u8 {
        COUNT.with(|c| {
            if let Some(n) = c.get() {
                c.set(Some(n + 1))
            }
        });
        unsafe { System.realloc(ptr, layout, new) }
    }
}
#[global_allocator]
static ALLOCATOR: Counting = Counting;
fn target(channels: u8) -> Target {
    let mut t = Target::new("test", "Test", TargetKind::Output, channels);
    t.limiter.enabled = false;
    t
}
fn prepared(t: &Target) -> Prepared {
    Prepared::new(t, 48000).unwrap()
}
fn wave(n: usize, offset: usize) -> Vec<f32> {
    (0..n)
        .map(|i| (((i + offset) * 31 % 1024) as f32 - 512.0) / 2048.0)
        .collect()
}

#[test]
fn eq_only_microphone_has_no_worker_delay_and_suppression_toggle_requires_restart() {
    let mut t = target(1);
    t.kind = TargetKind::Mic;
    t.eq.enabled = true;
    t.eq.input_gain_db = -6.0;
    let disabled = prepared(&t);
    assert_eq!(disabled.suppression_geometry(), None);
    let mut dsp = Dsp::without_suppression(&disabled).unwrap();
    assert_eq!(dsp.latency_samples(), 0);
    let input = [0.125; 112];
    let mut output = [0.; 112];
    COUNT.with(|count| count.set(Some(0)));
    dsp.process(&disabled, [&input, &[]], [&mut output, &mut []])
        .unwrap();
    assert_eq!(COUNT.with(|count| count.replace(None).unwrap()), 0);
    let expected = 0.125 * 10_f32.powf(-6.0 / 20.0);
    assert!(
        output
            .iter()
            .all(|sample| (*sample - expected).abs() < 1e-6)
    );
    t.suppression.enabled = true;
    let enabled = prepared(&t);
    assert!(disabled.requires_reset(&enabled));
    assert!(enabled.requires_reset(&disabled));
    let previous = output;
    assert_eq!(
        dsp.process(&enabled, [&input, &[]], [&mut output, &mut []]),
        Err(DspError::IncompatibleSnapshot)
    );
    assert_eq!(output, previous);
    assert!(matches!(
        Dsp::without_suppression(&enabled),
        Err(DspError::MissingSuppression)
    ));
}

#[test]
fn constructor_rejects_missing_or_mismatched_microphone_worker() {
    let mut t = target(1);
    t.kind = TargetKind::Mic;
    t.suppression.enabled = true;
    assert!(matches!(
        Dsp::without_suppression(&prepared(&t)),
        Err(DspError::MissingSuppression)
    ));
    let bad = Delay::new(7);
    assert!(matches!(
        Dsp::new(&prepared(&t), [Some(Box::new(bad)), None]),
        Err(DspError::SuppressionGeometry)
    ));
    t.kind = TargetKind::Output;
    t.suppression.enabled = false;
    assert!(matches!(
        Dsp::new(&prepared(&t), [Some(Box::new(Delay::new(7))), None]),
        Err(DspError::UnexpectedSuppression)
    ));
}

#[test]
fn arbitrary_chunks_in_place_and_zero_blocks_preserve_history() {
    let mut t = target(2);
    t.highpass.enabled = true;
    t.eq.enabled = true;
    t.compressor.enabled = true;
    let mut b = Band::new("bell");
    b.frequency = 1870.0;
    b.gain = 6.0;
    t.eq.bands = vec![b];
    let p = prepared(&t);
    let mut whole = Dsp::without_suppression(&p).unwrap();
    let mut chunks = Dsp::without_suppression(&p).unwrap();
    let input = [wave(16384, 0), wave(16384, 9)];
    let mut expected = [vec![0.; 16384], vec![0.; 16384]];
    let [a, b] = &mut expected;
    whole.process(&p, [&input[0], &input[1]], [a, b]).unwrap();
    let mut actual = input.clone();
    let sizes = [0, 1, 112, 7, 480, 23, 211, 3];
    let mut pos = 0;
    let mut step = 0;
    while pos < actual[0].len() {
        let n = sizes[step % sizes.len()].min(actual[0].len() - pos);
        step += 1;
        let [a, b] = &mut actual;
        chunks
            .process_in_place(&p, [&mut a[pos..pos + n], &mut b[pos..pos + n]])
            .unwrap();
        pos += n;
    }
    assert_eq!(actual, expected);
}

#[test]
fn stereo_compressor_and_limiter_link_the_detector() {
    for compressor in [false, true] {
        let mut t = target(2);
        t.compressor.enabled = compressor;
        t.compressor.threshold = -30.;
        t.compressor.ratio = 10.;
        t.compressor.attack = 0.1;
        t.limiter.enabled = !compressor;
        t.limiter.ceiling = -12.;
        let p = prepared(&t);
        let mut dsp = Dsp::without_suppression(&p).unwrap();
        let left = vec![0.8; 4096];
        let right = vec![0.05; 4096];
        let mut a = vec![0.; 4096];
        let mut b = vec![0.; 4096];
        let m = dsp.process(&p, [&left, &right], [&mut a, &mut b]).unwrap();
        assert!(m.gain_reduction > 5.);
        for i in 0..4096 {
            assert!((a[i] - 16. * b[i]).abs() < 1e-7);
        }
    }
}

#[test]
fn rejects_incompatible_updates_and_bad_buffers_before_mutating() {
    let t = target(1);
    let p = prepared(&t);
    let mut dsp = Dsp::without_suppression(&p).unwrap();
    let mut modified = t.clone();
    modified.channels = 2;
    let q = prepared(&modified);
    assert!(p.requires_reset(&q));
    let input = [0.2; 8];
    let mut output = [9.; 8];
    assert_eq!(
        dsp.process(&q, [&input, &[]], [&mut output, &mut []]),
        Err(DspError::IncompatibleSnapshot)
    );
    assert_eq!(output, [9.; 8]);
    assert_eq!(
        dsp.process(&p, [&input, &[]], [&mut output[..4], &mut []]),
        Err(DspError::BufferShape)
    );
    assert_eq!(output, [9.; 8]);
    dsp.process(&p, [&input, &[]], [&mut output, &mut []])
        .unwrap();
    assert_eq!(output, input);
}

#[test]
fn callbacks_and_structural_eq_handoff_do_not_allocate() {
    let mut t = target(2);
    t.eq.enabled = true;
    t.eq.bands = (0..32)
        .map(|i| {
            let mut b = Band::new(format!("b{i}"));
            b.frequency = 100. + i as f64 * 300.;
            b.gain = if i % 2 == 0 { 2. } else { -2. };
            b
        })
        .collect();
    let p = prepared(&t);
    t.eq.bands.reverse();
    t.eq.bands[0].band_type = BandType::HighShelf;
    let q = prepared(&t);
    let mut dsp = Dsp::without_suppression(&p).unwrap();
    let input = wave(1024, 0);
    let mut a = vec![0.; 1024];
    let mut b = vec![0.; 1024];
    COUNT.with(|c| c.set(Some(0)));
    let result = dsp.process(&q, [&input, &input], [&mut a, &mut b]);
    let allocations = COUNT.with(|c| c.replace(None).unwrap());
    assert!(result.is_ok());
    assert_eq!(allocations, 0);
}

#[test]
fn disabled_steady_identity_callbacks_do_not_allocate() {
    let mut target = target(2);
    target.eq.enabled = false;
    let prepared = prepared(&target);
    let mut dsp = Dsp::without_suppression(&prepared).unwrap();
    let input = [0.125; 112];
    let mut left = [0.0; 112];
    let mut right = left;
    COUNT.with(|count| count.set(Some(0)));
    let result = dsp.process(&prepared, [&input, &input], [&mut left, &mut right]);
    let allocations = COUNT.with(|count| count.replace(None).unwrap());
    assert_eq!(allocations, 0);
    let meter = result.unwrap();
    assert_eq!(left, input);
    assert_eq!(right, input);
    assert_eq!(meter.samples, 224);
}

fn suppression_delay() -> usize {
    rc2fx_core::Suppression::admitted_geometry(0)
        .unwrap()
        .latency_samples() as usize
}

// Test-only injected worker contract, never a runtime model selector/backend.
struct Delay {
    ring: Vec<f32>,
    position: usize,
    geometry: rc2fx_core::SuppressionGeometry,
    drain_error: Option<RetirementError>,
    /// The voice score it reports, as f32 bits; NO_VOICE for none. Shared so
    /// a test can change it while the provider is owned by the Dsp.
    voice: Arc<AtomicU32>,
}
const NO_VOICE: u32 = u32::MAX;
fn set_voice(cell: &AtomicU32, voice: Option<f32>) {
    cell.store(
        voice.map_or(NO_VOICE, f32::to_bits),
        std::sync::atomic::Ordering::Relaxed,
    );
}
impl Delay {
    fn new(n: usize) -> Self {
        Self {
            ring: vec![0.; n],
            position: 0,
            geometry: rc2fx_core::Suppression::admitted_geometry(0).unwrap(),
            drain_error: None,
            voice: Arc::new(AtomicU32::new(NO_VOICE)),
        }
    }
}
impl Suppression for Delay {
    fn latency_samples(&self) -> usize {
        self.ring.len()
    }
    fn geometry(&self) -> rc2fx_core::SuppressionGeometry {
        self.geometry
    }
    fn drain(&mut self, _: std::time::Duration) -> Result<(), RetirementError> {
        self.drain_error.map_or(Ok(()), Err)
    }
    fn process_sample(&mut self, input: f32, _wet: f32) -> f32 {
        let y = self.ring[self.position];
        self.ring[self.position] = input;
        self.position = (self.position + 1) % self.ring.len();
        y
    }
    fn voice(&self) -> Option<f32> {
        let bits = self.voice.load(std::sync::atomic::Ordering::Relaxed);
        (bits != NO_VOICE).then(|| f32::from_bits(bits))
    }
}

#[test]
fn exact_shape_not_just_total_delay_is_required() {
    let mut t = target(1);
    t.kind = TargetKind::Mic;
    t.suppression.enabled = true;
    let p = prepared(&t);
    let mut wrong = Delay::new(suppression_delay());
    wrong.geometry.frame_samples -= 1;
    wrong.geometry.intrinsic_delay_samples += 1;
    assert_eq!(
        wrong.geometry.latency_samples() as usize,
        suppression_delay()
    );
    assert!(matches!(
        Dsp::new(&p, [Some(Box::new(wrong)), None]),
        Err(DspError::SuppressionGeometry)
    ));
    struct Invalid;
    impl Suppression for Invalid {
        fn geometry(&self) -> rc2fx_core::SuppressionGeometry {
            rc2fx_core::SuppressionGeometry {
                frame_samples: u32::MAX,
                ..rc2fx_core::Suppression::admitted_geometry(0).unwrap()
            }
        }
        fn process_sample(&mut self, _: f32, _: f32) -> f32 {
            unreachable!("invalid provider must not publish")
        }
        fn voice(&self) -> Option<f32> {
            None
        }
        fn drain(&mut self, _: std::time::Duration) -> Result<(), RetirementError> {
            Err(RetirementError::Unsupported)
        }
    }
    // Reject shape before invoking the default latency sum, which would
    // overflow on this deliberately malformed provider's public geometry.
    assert!(matches!(
        Dsp::new(&p, [Some(Box::new(Invalid)), None]),
        Err(DspError::SuppressionGeometry)
    ));
}

#[test]
fn control_drain_irreversibly_retires_even_on_timeout() {
    for error in [None, Some(RetirementError::TimedOut)] {
        let mut t = target(1);
        t.kind = TargetKind::Mic;
        t.suppression.enabled = true;
        let p = prepared(&t);
        let mut provider = Delay::new(suppression_delay());
        provider.drain_error = error;
        let mut dsp = Dsp::new(&p, [Some(Box::new(provider)), None]).unwrap();
        assert_eq!(
            dsp.drain_suppression(std::time::Duration::ZERO),
            error.map_or(Ok(()), Err)
        );
        let mut output = [7.0; 112];
        assert_eq!(
            dsp.process(&p, [&[0.1; 112], &[]], [&mut output, &mut []]),
            Err(DspError::Retired)
        );
        assert_eq!(output, [7.0; 112]);
        assert_eq!(
            dsp.process_in_place(&p, [&mut output, &mut []]),
            Err(DspError::Retired)
        );
        assert_eq!(output, [7.0; 112]);
    }
}

#[test]
fn provider_must_match_every_field_of_the_prepared_model() {
    let mut t = target(1);
    t.kind = TargetKind::Mic;
    t.suppression.enabled = true;
    let p = prepared(&t);
    let geometry = p.suppression_geometry().unwrap();
    assert_eq!(geometry, t.suppression.geometry().unwrap());
    let provider = || Delay {
        ring: vec![0.; geometry.latency_samples() as usize],
        position: 0,
        geometry,
        drain_error: None,
        voice: Arc::new(AtomicU32::new(NO_VOICE)),
    };
    let dsp = Dsp::new(&p, [Some(Box::new(provider())), None]).unwrap();
    assert_eq!(dsp.latency_samples(), suppression_delay());
    for field in 0..3 {
        let mut wrong = provider();
        match field {
            0 => wrong.geometry.sample_rate += 1,
            1 => {
                wrong.geometry.frame_samples += 1;
                wrong.geometry.intrinsic_delay_samples -= 1;
            }
            _ => {
                wrong.geometry.worker_lead_samples += 1;
                wrong.geometry.intrinsic_delay_samples -= 1;
            }
        }
        assert!(matches!(
            Dsp::new(&p, [Some(Box::new(wrong)), None]),
            Err(DspError::SuppressionGeometry)
        ));
    }
    // Settings outside the model do not need a new worker.
    let mut scalar = t.clone();
    scalar.gain.db = -6.0;
    assert!(!p.requires_reset(&prepared(&scalar)));
}
#[test]
fn bypass_and_zero_blend_keep_exact_injected_delay() {
    let mut t = target(1);
    t.kind = TargetKind::Mic;
    t.suppression.enabled = true;
    t.suppression.wet = 0.0;
    t.bypass = true;
    let p = prepared(&t);
    let mut dsp = Dsp::new(&p, [Some(Box::new(Delay::new(suppression_delay()))), None]).unwrap();
    assert_eq!(dsp.latency_samples(), suppression_delay());
    let input = wave(5000, 0);
    let mut out = vec![0.; 5000];
    dsp.process(&p, [&input, &[]], [&mut out, &mut []]).unwrap();
    assert!(out[..suppression_delay()].iter().all(|v| *v == 0.));
    assert_eq!(
        &out[suppression_delay()..],
        &input[..input.len() - suppression_delay()]
    );
    t.bypass = false;
    t.suppression.wet = 1.0;
    let q = prepared(&t);
    let next = wave(1500, 5000);
    let mut output = vec![0.; 1500];
    dsp.process(&q, [&next, &[]], [&mut output, &mut []])
        .unwrap();
    let first_len = input.len();
    let combined = [input, next].concat();
    assert_eq!(
        &output[..],
        &combined[first_len - suppression_delay()..combined.len() - suppression_delay()]
    );
}

#[test]
fn finite_guard_and_meter_are_not_skipped_by_bypass() {
    let mut t = target(1);
    t.bypass = true;
    let p = prepared(&t);
    let mut dsp = Dsp::without_suppression(&p).unwrap();
    let input = [f32::NAN, f32::INFINITY, -20., 0.25];
    let mut out = [0.; 4];
    let m = dsp.process(&p, [&input, &[]], [&mut out, &mut []]).unwrap();
    assert_eq!(out, [0., 0., -1., 0.25]);
    assert_eq!(m.clipped, 4);
    assert_eq!(m.input_peak, 20.);
    assert_eq!(m.samples, 4);
    assert_eq!(m.sum_squares, 1.0625);
}

#[test]
fn effect_order_without_a_worker_is_live_and_right_eq_remains_independent() {
    let mut t = target(2);
    t.eq.enabled = true;
    let mut b = Band::new("left");
    b.gain = 12.;
    b.frequency = 1000.;
    t.eq.bands = vec![b];
    t.eq.right_bands = Some(Vec::new());
    let p = prepared(&t);
    let mut dsp = Dsp::without_suppression(&p).unwrap();
    let input = wave(1024, 0);
    let mut a = vec![0.; 1024];
    let mut b = vec![0.; 1024];
    dsp.process(&p, [&input, &input], [&mut a, &mut b]).unwrap();
    assert_eq!(b, input);
    assert_ne!(a, b);
    t.order = [
        Effect::Gain,
        Effect::Highpass,
        Effect::Eq,
        Effect::Suppression,
        Effect::Limiter,
        Effect::Compressor,
    ]
    .to_vec();
    let reordered = prepared(&t);
    assert!(!p.requires_reset(&reordered));
    dsp.process(&reordered, [&input, &input], [&mut a, &mut b])
        .unwrap();
    assert_eq!(b, input);
    assert_ne!(a, b);
}

#[test]
fn suppression_order_is_live_only_without_an_allocated_provider() {
    for kind in [TargetKind::Mic, TargetKind::Playback, TargetKind::Output] {
        let mut t = target(1);
        t.kind = kind;
        let classic = prepared(&t);
        t.order.swap(0, 1);
        let current = prepared(&t);
        assert!(!classic.requires_reset(&current));
        let mut dsp = Dsp::without_suppression(&classic).unwrap();
        let mut output = [0.0; 8];
        dsp.process(&current, [&[0.125; 8], &[]], [&mut output, &mut []])
            .unwrap();
        assert_eq!(output, [0.125; 8]);
    }
    let mut t = target(1);
    t.kind = TargetKind::Mic;
    t.suppression.enabled = true;
    let classic = prepared(&t);
    t.order.swap(0, 1);
    assert!(classic.requires_reset(&prepared(&t)));
    t.order.swap(0, 1);
    t.suppression.enabled = false;
    assert!(classic.requires_reset(&prepared(&t)));
}

#[test]
fn ordered_gain_and_limiter_change_audio_as_configured() {
    let mut t = target(1);
    t.gain.enabled = true;
    t.gain.db = 12.0;
    t.limiter.enabled = true;
    t.limiter.ceiling = -12.0;
    let gain_first = prepared(&t);
    t.order.swap(4, 5);
    let limiter_first = prepared(&t);
    let mut a = Dsp::without_suppression(&gain_first).unwrap();
    let mut b = Dsp::without_suppression(&limiter_first).unwrap();
    let input = [0.1; 32];
    let mut x = [0.0; 32];
    let mut y = [0.0; 32];
    a.process(&gain_first, [&input, &[]], [&mut x, &mut []])
        .unwrap();
    b.process(&limiter_first, [&input, &[]], [&mut y, &mut []])
        .unwrap();
    let ceiling = 10_f32.powf(-12.0 / 20.0);
    let raised = 0.1 * 10_f32.powf(12.0 / 20.0);
    assert!(x.iter().all(|v| (*v - ceiling).abs() < 1e-7));
    assert!(y.iter().all(|v| (*v - raised).abs() < 1e-7));

    assert!(!limiter_first.requires_reset(&gain_first));
    // Reuse the running limiter-first processor. A live reordering must change
    // the actual signal, not merely accept and acknowledge new settings.
    b.process(&gain_first, [&input, &[]], [&mut y, &mut []])
        .unwrap();
    assert!(y.iter().all(|v| (*v - ceiling).abs() < 1e-7));
}

#[test]
fn disabled_stage_updates_freeze_history_and_reenable_without_reset() {
    for stage in [Effect::Highpass, Effect::Compressor, Effect::Limiter] {
        let mut t = target(1);
        t.eq.enabled = false;
        t.set_effect_enabled(stage, true);
        t.compressor.threshold = -30.0;
        t.compressor.ratio = 10.0;
        t.compressor.attack = 0.1;
        t.limiter.ceiling = -12.0;
        let enabled = prepared(&t);
        let mut with_disabled_interval = Dsp::without_suppression(&enabled).unwrap();
        let mut without_interval = Dsp::without_suppression(&enabled).unwrap();
        let warm = wave(256, 0);
        let mut scratch = [0.0; 256];
        for dsp in [&mut with_disabled_interval, &mut without_interval] {
            dsp.process(&enabled, [&warm, &[]], [&mut scratch, &mut []])
                .unwrap();
        }
        t.set_effect_enabled(stage, false);
        t.highpass.frequency = 230.0;
        t.compressor.release = 230.0;
        t.limiter.release = 230.0;
        let disabled = prepared(&t);
        assert!(!enabled.requires_reset(&disabled));
        let middle = wave(397, 256);
        let mut dry = vec![0.0; middle.len()];
        with_disabled_interval
            .process(&disabled, [&middle, &[]], [&mut dry, &mut []])
            .unwrap();
        assert_eq!(
            dry.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
            middle.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
        );
        t.set_effect_enabled(stage, true);
        let resumed = prepared(&t);
        assert!(!disabled.requires_reset(&resumed));
        let probe = wave(512, 653);
        let mut a = [0.0; 512];
        let mut b = [0.0; 512];
        let ma = with_disabled_interval
            .process(&resumed, [&probe, &[]], [&mut a, &mut []])
            .unwrap();
        let mb = without_interval
            .process(&resumed, [&probe, &[]], [&mut b, &mut []])
            .unwrap();
        assert_eq!(a.map(f32::to_bits), b.map(f32::to_bits));
        assert_eq!(
            [
                ma.input_peak,
                ma.output_peak,
                ma.sum_squares,
                ma.gain_reduction
            ]
            .map(f64::to_bits),
            [
                mb.input_peak,
                mb.output_peak,
                mb.sum_squares,
                mb.gain_reduction
            ]
            .map(f64::to_bits)
        );
        assert_eq!((ma.samples, ma.clipped), (mb.samples, mb.clipped));
    }
}

#[test]
fn disabled_gain_keeps_smoothing_toward_unity() {
    let mut t = target(1);
    t.eq.enabled = false;
    t.gain.enabled = true;
    t.gain.db = -12.0;
    let enabled = prepared(&t);
    let mut dsp = Dsp::without_suppression(&enabled).unwrap();
    t.gain.enabled = false;
    let disabled = prepared(&t);
    assert!(!enabled.requires_reset(&disabled));
    let input = [0.25; 512];
    let mut output = [0.0; 512];
    dsp.process(&disabled, [&input, &[]], [&mut output, &mut []])
        .unwrap();
    assert!(output[0] > 0.06 && output[0] < 0.07);
    assert!(output.windows(2).all(|x| x[1] > x[0]));
    assert!(output[511] < input[511]);
}

#[test]
fn every_mic_order_keeps_enabled_provider_warm_across_wet_zero_and_bypass() {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct ObservedDelay {
        inner: Delay,
        calls: Arc<AtomicUsize>,
    }
    impl Suppression for ObservedDelay {
        fn geometry(&self) -> rc2fx_core::SuppressionGeometry {
            self.inner.geometry()
        }
        fn process_sample(&mut self, input: f32, wet: f32) -> f32 {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.inner.process_sample(input, wet)
        }
        fn voice(&self) -> Option<f32> {
            self.inner.voice()
        }
        fn drain(&mut self, timeout: std::time::Duration) -> Result<(), RetirementError> {
            self.inner.drain(timeout)
        }
    }
    for rotation in 0..6 {
        let mut t = target(1);
        t.kind = TargetKind::Mic;
        t.suppression.enabled = true;
        t.eq.enabled = false;
        t.order.rotate_left(rotation);
        let initial = prepared(&t);
        let calls = Arc::new(AtomicUsize::new(0));
        let mut dsp = Dsp::new(
            &initial,
            [
                Some(Box::new(ObservedDelay {
                    inner: Delay::new(suppression_delay()),
                    calls: calls.clone(),
                })),
                None,
            ],
        )
        .unwrap();
        let input: Vec<f32> = (0..1500).map(|i| 0.125 + (i % 17) as f32 / 128.0).collect();
        let mut output = vec![0.0; input.len()];
        for (step, (wet, bypass)) in [
            (0.0, true),
            (0.0, false),
            (1.0, false),
            (0.7, true),
            (1.0, false),
        ]
        .into_iter()
        .enumerate()
        {
            t.suppression.wet = wet;
            t.bypass = bypass;
            let p = prepared(&t);
            assert!(!initial.requires_reset(&p));
            let start = step * 300;
            dsp.process(
                &p,
                [&input[start..start + 300], &[]],
                [&mut output[start..start + 300], &mut []],
            )
            .unwrap();
            dsp.process(&p, [&[], &[]], [&mut [], &mut []]).unwrap();
            assert_eq!(calls.load(Ordering::Relaxed), (step + 1) * 300);
        }
        assert!(
            output[..suppression_delay()]
                .iter()
                .all(|x| x.to_bits() == 0)
        );
        assert_eq!(
            output[suppression_delay()..]
                .iter()
                .map(|x| x.to_bits())
                .collect::<Vec<_>>(),
            input[..input.len() - suppression_delay()]
                .iter()
                .map(|x| x.to_bits())
                .collect::<Vec<_>>()
        );
    }
}

/// A mono mic with only suppression (a fixed-geometry Delay) and the voice gate.
fn gated_mic() -> Target {
    let mut t = target(1);
    t.kind = TargetKind::Mic;
    t.eq.enabled = false;
    t.suppression.enabled = true;
    t.suppression.gate.strength = 0.5;
    t
}
/// Runs `n` samples of constant input in 112-sample periods; returns the last
/// output sample and the last period's meter.
fn run_constant(dsp: &mut Dsp, p: &Prepared, n: usize, x: f32) -> (f32, rc2fx_dsp::Meter) {
    let input = [x; 112];
    let mut out = [0.; 112];
    let mut meter = rc2fx_dsp::Meter::default();
    for _ in 0..n / 112 {
        meter = dsp.process(p, [&input, &[]], [&mut out, &mut []]).unwrap();
    }
    (out[111], meter)
}

#[test]
fn voice_gate_closes_after_its_hold_and_reopens_on_voice() {
    let t = gated_mic();
    let p = prepared(&t);
    let provider = Delay::new(suppression_delay());
    let voice = provider.voice.clone();
    let mut dsp = Dsp::new(&p, [Some(Box::new(provider)), None]).unwrap();
    set_voice(&voice, Some(0.9));
    let (y, m) = run_constant(&mut dsp, &p, 4816, 0.25);
    assert_eq!(y, 0.25, "an open gate is exact");
    assert_eq!((m.voice as f32, m.gate_open), (0.9, 1.));

    // At strength 0.5: opens at 0.5, stays open down to 0.35, holds 250 ms,
    // then turns down by 20 dB with a 164 ms time constant (in dB); opens
    // with a 5 ms one.
    let shape = t.suppression.gate.shape();
    assert_eq!((shape.threshold, shape.hold_ms), (0.5, 250.));
    assert!((shape.reduction_db - 20.).abs() < 1e-9);
    // Above the close threshold an open gate stays open.
    set_voice(&voice, Some(0.4));
    let (y, _) = run_constant(&mut dsp, &p, 72016, 0.25);
    assert_eq!(y, 0.25, "hysteresis keeps it open");
    set_voice(&voice, Some(0.2));
    let (y, _) = run_constant(&mut dsp, &p, 10416, 0.25);
    assert_eq!(y, 0.25, "still holding");
    let (y, m) = run_constant(&mut dsp, &p, 72016, 0.25);
    let floor = 0.25 * 10_f32.powf(-shape.reduction_db as f32 / 20.);
    assert!((y - floor).abs() < 1e-4, "{y} vs {floor}");
    assert!(m.gate_open < 0.01, "{}", m.gate_open);
    assert_eq!(m.voice as f32, 0.2);

    // A score between the thresholds does not reopen a closed gate.
    set_voice(&voice, Some(0.4));
    let (y, _) = run_constant(&mut dsp, &p, 4816, 0.25);
    assert!((y - floor).abs() < 1e-4, "{y}");
    // Voice again: within 0.1 dB of open in 35 ms (seven 5 ms time constants).
    set_voice(&voice, Some(0.6));
    let (y, m) = run_constant(&mut dsp, &p, 1680, 0.25);
    assert!(y > 0.248, "{y}");
    assert!(m.gate_open > 0.99);
    // Fully settled, an open gate is exact again.
    let (y, _) = run_constant(&mut dsp, &p, 4816, 0.25);
    assert_eq!(y, 0.25);
}

#[test]
fn voice_gate_opens_when_the_model_has_no_score() {
    let t = gated_mic();
    let p = prepared(&t);
    let provider = Delay::new(suppression_delay());
    let voice = provider.voice.clone();
    let mut dsp = Dsp::new(&p, [Some(Box::new(provider)), None]).unwrap();
    set_voice(&voice, Some(0.));
    run_constant(&mut dsp, &p, 72016, 0.25);
    // The delayed original (late or failed worker) is never cut.
    set_voice(&voice, None);
    let (y, m) = run_constant(&mut dsp, &p, 1680, 0.25);
    assert!(y > 0.248, "{y}");
    assert_eq!(m.voice, 0.);
}

#[test]
fn voice_gate_off_is_bit_exact_and_its_settings_are_live() {
    let mut t = gated_mic();
    t.suppression.gate.strength = 0.;
    let off = prepared(&t);
    let provider = Delay::new(suppression_delay());
    let voice = provider.voice.clone();
    let mut dsp = Dsp::new(&off, [Some(Box::new(provider)), None]).unwrap();
    set_voice(&voice, Some(0.));
    let input = wave(4816, 0);
    let mut out = vec![0.; 4816];
    for (x, y) in input.chunks(112).zip(out.chunks_mut(112)) {
        let m = dsp.process(&off, [x, &[]], [y, &mut []]).unwrap();
        assert_eq!(m.gate_open, 1., "an off gate reports open");
    }
    for (n, y) in out.iter().enumerate() {
        let expected = if n >= suppression_delay() {
            input[n - suppression_delay()]
        } else {
            0.
        };
        assert_eq!(y.to_bits(), expected.to_bits());
    }
    // Turning the gate on or moving its sliders keeps the model running.
    t.suppression.gate.strength = 0.5;
    let on = prepared(&t);
    assert!(!off.requires_reset(&on));
    t.suppression.gate.strength = 0.9;
    let strict = prepared(&t);
    assert!(!on.requires_reset(&strict));
    let (y, _) = run_constant(&mut dsp, &strict, 72016, 0.25);
    assert!(y < 0.25 * 0.05, "{y}");
    // Advanced parameters apply live too: no reduction leaves it open.
    t.suppression.gate.set_advanced(true);
    t.suppression.gate.reduction = 0.;
    let custom = prepared(&t);
    assert!(!strict.requires_reset(&custom));
    let (y, _) = run_constant(&mut dsp, &custom, 4816, 0.25);
    assert!(y > 0.248, "{y}");
}

#[test]
fn edits_to_disabled_eq_use_the_latest_coefficients_when_enabled() {
    let mut t = target(2);
    t.eq.enabled = false;
    t.eq.bands = (0..20)
        .map(|index| {
            let mut band = Band::new(format!("band-{index}"));
            band.frequency = 100.0 + index as f64 * 600.0;
            band.gain = 1.0;
            band
        })
        .collect();
    let initial = prepared(&t);
    let mut edited = Dsp::without_suppression(&initial).unwrap();
    let mut deferred = Dsp::without_suppression(&initial).unwrap();
    let input = wave(1024, 0);
    let mut actual = [vec![0.0; input.len()], vec![0.0; input.len()]];
    let mut expected = actual.clone();
    for round in 0..5 {
        t.eq.bands[0].frequency += 120.0;
        t.eq.bands[0].gain -= 2.0;
        t.eq.bands[round + 1].enabled = false;
        let next = prepared(&t);
        let [left, right] = &mut actual;
        edited
            .process(&next, [&input, &input], [left, right])
            .unwrap();
        let [left, right] = &mut expected;
        deferred
            .process(&initial, [&input, &input], [left, right])
            .unwrap();
        assert_eq!(actual, expected);
        assert_eq!(actual[0], input);
    }
    t.eq.enabled = true;
    let enabled = prepared(&t);
    let [left, right] = &mut actual;
    COUNT.with(|count| count.set(Some(0)));
    let result = edited.process(&enabled, [&input, &input], [left, right]);
    let allocations = COUNT.with(|count| count.replace(None).unwrap());
    result.unwrap();
    assert_eq!(allocations, 0);
    let [left, right] = &mut expected;
    deferred
        .process(&enabled, [&input, &input], [left, right])
        .unwrap();
    assert_eq!(
        actual, expected,
        "disabled edits cannot leave stale coefficients"
    );
    assert_ne!(actual[0], input);
}
