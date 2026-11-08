use super::*;

/// One second of speech-like harmonic bursts, clicks and a low noise floor,
/// quantised to 16 bits (the reference was rendered from this as a WAV).
fn signal() -> Vec<f32> {
    let mut seed = 0x2545_f491u32;
    let mut noise = move || {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (seed >> 8) as f32 / (1 << 23) as f32 - 1.
    };
    (0..dsp::SAMPLE_RATE)
        .map(|i| {
            let t = i as f32 / dsp::SAMPLE_RATE as f32;
            let envelope = (t * 3. * std::f32::consts::TAU).sin().max(0.).powi(2);
            let pitch = 140. + 30. * (t * 2.).sin();
            let voice: f32 = (1..12)
                .map(|h| (h as f32 * pitch * t * std::f32::consts::TAU).sin() / h as f32)
                .sum::<f32>()
                * envelope
                * 0.15;
            let click = if i % 7_919 < 24 {
                0.3 * (1. - (i % 7_919) as f32 / 24.)
            } else {
                0.
            };
            let x = voice + click + 0.01 * noise();
            (x * 32768.).round().clamp(-32768., 32767.) / 32768.
        })
        .collect()
}

fn run(filter: &mut DeepFilter, input: &[f32]) -> Vec<f32> {
    let mut out = input.to_vec();
    for frame in out.chunks_exact_mut(filter.frame()) {
        filter.process(frame);
    }
    out
}

/// The embedded weights with their framing tensors replaced.
fn reframed(fft: u32, synthesis: u32, hop: u32, lookahead: u32) -> Vec<u8> {
    let tensors = Tensors::parse(WEIGHTS).unwrap();
    let mut out = Vec::new();
    let framing = [
        ("rc2fx.fft_size", fft),
        ("rc2fx.synthesis_size", synthesis),
        ("rc2fx.hop_size", hop),
        ("rc2fx.lookahead", lookahead),
    ];
    let mut names: Vec<&String> = tensors.0.keys().collect();
    names.sort();
    out.extend_from_slice(&(names.len() as u32).to_le_bytes());
    for name in names {
        let (dims, data) = &tensors.0[name];
        let value = framing
            .iter()
            .find(|(n, _)| n == name)
            .map(|&(_, v)| v as f32);
        out.extend_from_slice(&(name.len() as u32).to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&(dims.len() as u32).to_le_bytes());
        for &d in dims {
            out.extend_from_slice(&(d as u32).to_le_bytes());
        }
        for &x in data {
            out.extend_from_slice(&value.unwrap_or(x).to_le_bytes());
        }
    }
    out
}

/// 16-bit mono PCM samples of a canonical 44-byte-header WAV.
fn wav_samples(bytes: &[u8]) -> Vec<f32> {
    assert_eq!(&bytes[..4], b"RIFF");
    assert_eq!(&bytes[36..40], b"data");
    bytes[44..]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as f32 / 32768.)
        .collect()
}

#[test]
fn matches_deepfilternet_reference() {
    // DeepFilterNet's own runtime (libDF with tract, every stage forced on)
    // on the same input and weights; see model/README.md.
    let reference = wav_samples(include_bytes!("../model/reference.wav"));
    assert_matches_reference(&run(&mut DeepFilter::new().unwrap(), &signal()), &reference);
}

/// Prints a digest of the embedded model's output bits on `signal()`, so two
/// builds can be compared exactly:
/// `cargo test output_digest -- --ignored --nocapture`.
#[test]
#[ignore]
fn output_digest() {
    let out = run(&mut DeepFilter::new().unwrap(), &signal());
    let digest = out.iter().fold(0xcbf2_9ce4_8422_2325u64, |h, v| {
        v.to_bits()
            .to_le_bytes()
            .iter()
            .fold(h, |h, &b| (h ^ b as u64).wrapping_mul(0x0100_0000_01b3))
    });
    println!("output digest {digest:016x} samples {}", out.len());
}

#[test]
fn preserved_la1_matches_its_own_reference() {
    let mut filter = DeepFilter::from_weights(include_bytes!("../model/la1/weights.bin")).unwrap();
    assert_eq!(filter.frame(), 240);
    assert_eq!(filter.lookahead(), 1);
    assert_eq!(filter.delay(), 360);
    let reference = wav_samples(include_bytes!("../model/la1/reference.wav"));
    assert_matches_reference(&run(&mut filter, &signal()), &reference);
}

fn assert_matches_reference(ours: &[f32], reference: &[f32]) {
    assert_eq!(ours.len(), reference.len());
    let (mut err, mut energy, mut worst) = (0f64, 0f64, 0f32);
    for (a, b) in ours.iter().zip(reference) {
        let d = a - b;
        err += (d * d) as f64;
        energy += (b * b) as f64;
        worst = worst.max(d.abs());
    }
    let db = 10. * (err / energy).log10();
    eprintln!("engine vs DeepFilterNet reference: {db:.1} dB, worst sample {worst:.2e}");
    // References are rounded to 16 bits. The int8 weights measure about
    // -45 to -48 dB against them, for LA0 and LA1 alike. This checks
    // inference agreement, not perceived denoising quality.
    let (db_limit, worst_limit) = (-43., 128.);
    assert!(db < db_limit, "{db:.1} dB");
    assert!(
        worst <= worst_limit / 32768.,
        "worst sample error {worst} ({db:.1} dB)"
    );
}

#[test]
fn reports_a_bounded_local_snr_for_every_frame() {
    let mut filter = DeepFilter::new().unwrap();
    for frame in signal().chunks_exact(filter.frame()) {
        let lsnr = filter.process(&mut frame.to_vec());
        assert!((-15.0..=35.0).contains(&lsnr), "{lsnr}");
    }
}

#[test]
fn reset_forgets_history() {
    let input = signal();
    let mut filter = DeepFilter::new().unwrap();
    let first = run(&mut filter, &input);
    filter.reset();
    assert_eq!(run(&mut filter, &input), first);
}

/// `for_lookahead` selects between the two embedded exports, and refuses a
/// lookahead that is not one of them.
#[test]
fn the_lookahead_selector_picks_the_embedded_export() {
    let la0 = DeepFilter::for_lookahead(0).unwrap();
    assert_eq!(la0.lookahead(), 0);
    assert_eq!(la0.delay(), 120);
    let mut la1 = DeepFilter::for_lookahead(1).unwrap();
    assert_eq!(la1.lookahead(), 1);
    assert_eq!(la1.delay(), 360);
    assert_eq!(la1.frame(), 240);
    // The same model as the preserved file embedded by path.
    let mut preserved =
        DeepFilter::from_weights(include_bytes!("../model/la1/weights.bin")).unwrap();
    let signal = signal();
    assert_eq!(run(&mut la1, &signal), run(&mut preserved, &signal));
    for lookahead in [2, 3, 240] {
        assert!(DeepFilter::for_lookahead(lookahead).is_err(), "{lookahead}");
    }
}

#[test]
fn embedded_model_framing() {
    let filter = DeepFilter::new().unwrap();
    assert_eq!(filter.frame(), 240);
    assert_eq!(filter.lookahead(), 0);
    assert_eq!(filter.delay(), 360 - 240);
}

/// Two hops per call give bit for bit what the same hops give one per call,
/// in any mix of call sizes, and the same local SNR for the newer hop.
#[test]
fn two_hops_per_call_match_one_at_a_time() {
    for weights in [
        WEIGHTS,
        include_bytes!("../model/la1/weights.bin").as_slice(),
    ] {
        assert_batching_matches(weights);
    }
}

fn assert_batching_matches(weights: &[u8]) {
    let input = signal();
    let mut batched = DeepFilter::from_weights(weights).unwrap();
    let mut single = DeepFilter::from_weights(weights).unwrap();
    let mut mixed = DeepFilter::from_weights(weights).unwrap();
    // Exercise both internal paths regardless of the public build selection.
    let hop = batched.framing.hop;
    let frame = 2 * hop;
    let (mut a, mut b, mut c) = (input.clone(), input.clone(), input);
    for (i, ((a, b), c)) in a
        .chunks_exact_mut(frame)
        .zip(b.chunks_exact_mut(frame))
        .zip(c.chunks_exact_mut(frame))
        .enumerate()
    {
        let lsnr = batched.process_hops(a);
        let (b0, b1) = b.split_at_mut(hop);
        single.process_hops(b0);
        assert_eq!(single.process_hops(b1).to_bits(), lsnr.to_bits());
        // Calls of one hop shift the pairing by a hop.
        if i % 3 == 1 {
            let (c0, c1) = c.split_at_mut(hop);
            mixed.process_hops(c0);
            mixed.process_hops(c1);
        } else {
            mixed.process_hops(c);
        }
    }
    let bits = |x: &[f32]| x.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
    assert!(a.iter().any(|&x| x != 0.));
    assert_eq!(bits(&a), bits(&b));
    assert_eq!(bits(&a), bits(&c));
}

/// The selected public API must preserve every enhanced sample. One-hop
/// calls expose their own hop's SNR; paired calls expose the newer hop's.
#[test]
fn selected_public_call_matches_paired_stream() {
    for weights in [
        WEIGHTS,
        include_bytes!("../model/la1/weights.bin").as_slice(),
    ] {
        let mut selected = DeepFilter::from_weights(weights).unwrap();
        let mut paired = DeepFilter::from_weights(weights).unwrap();
        let mut expected = signal();
        let mut actual = expected.clone();
        let pair = 2 * paired.framing.hop;
        for (actual, expected) in actual
            .chunks_exact_mut(pair)
            .zip(expected.chunks_exact_mut(pair))
        {
            let expected_snr = paired.process_hops(expected);
            let mut actual_snr = 0.;
            for frame in actual.chunks_exact_mut(selected.frame()) {
                actual_snr = selected.process(frame);
            }
            assert_eq!(actual_snr.to_bits(), expected_snr.to_bits());
        }
        assert_eq!(
            actual.iter().map(|x| x.to_bits()).collect::<Vec<_>>(),
            expected.iter().map(|x| x.to_bits()).collect::<Vec<_>>()
        );
    }
}

fn bits(x: &[f32]) -> Vec<u32> {
    x.iter().map(|v| v.to_bits()).collect()
}

/// Hops per call for a stream: one or two, in a fixed irregular pattern, so
/// both internal paths and both pairings are covered whatever the build.
fn call_sizes(i: usize) -> usize {
    if i % 5 == 2 || i % 7 == 4 { 2 } else { 1 }
}

/// Runs `input` through `filter` in calls of `call_sizes`, calling `between`
/// after each call (with the call's index); returns the output and every
/// call's local SNR bits.
fn stream(
    filter: &mut DeepFilter,
    input: &[f32],
    mut between: impl FnMut(&mut DeepFilter, usize),
) -> (Vec<f32>, Vec<u32>) {
    let hop = filter.framing.hop;
    let mut out = input.to_vec();
    let mut snr = Vec::new();
    let (mut at, mut i) = (0, 0);
    while at + 2 * hop <= out.len() {
        let len = call_sizes(i) * hop;
        snr.push(filter.process_hops(&mut out[at..at + len]).to_bits());
        between(filter, i);
        at += len;
        i += 1;
    }
    out.truncate(at);
    (out, snr)
}

const MODELS: [&[u8]; 2] = [WEIGHTS, include_bytes!("../model/la1/weights.bin")];

#[test]
fn gru_prep_is_off_by_default_and_inert_when_off() {
    let mut filter = DeepFilter::new().unwrap();
    assert!(!filter.gru_prep());
    assert_eq!(filter.prep_pending(), 0);
    assert!(!filter.prepare_step());
    assert_eq!(filter.prepare_while(|| true), 0);
    let mut frame = vec![0.1f32; filter.frame()];
    filter.process(&mut frame);
    assert!(!filter.prepare_step());
    assert_eq!(filter.prep_pending(), 0);
}

/// Every GRU layer has a recurrent product to prepare once enabled; one
/// step prepares one layer, preparing again is a no-op until the next call
/// moves the hidden states, and `go` is asked before every layer.
#[test]
fn gru_prep_works_one_layer_at_a_time() {
    let mut filter = DeepFilter::new().unwrap();
    filter.set_gru_prep(true);
    assert!(filter.gru_prep());
    let layers = filter.prep_pending();
    // LA0: one encoder layer, two ERB decoder layers, two DF decoder layers.
    assert_eq!(layers, 5);
    let mut asked = 0;
    assert_eq!(
        filter.prepare_while(|| {
            asked += 1;
            false
        }),
        0
    );
    assert_eq!((asked, filter.prep_pending()), (1, layers));
    assert!(filter.prepare_step());
    assert_eq!(filter.prep_pending(), layers - 1);
    let mut budget = 2;
    assert_eq!(
        filter.prepare_while(|| {
            budget -= 1;
            budget >= 0
        }),
        2
    );
    assert_eq!(filter.prep_pending(), layers - 3);
    assert_eq!(filter.prepare_while(|| true), layers - 3);
    assert_eq!(filter.prep_pending(), 0);
    assert!(!filter.prepare_step());
    assert_eq!(filter.prepare_while(|| true), 0);
    let mut frame = vec![0.1f32; filter.frame()];
    filter.process(&mut frame);
    assert_eq!(filter.prep_pending(), layers);
    assert_eq!(filter.prepare_while(|| true), layers);
    filter.set_gru_prep(false);
    assert_eq!(filter.prep_pending(), 0);
    filter.set_gru_prep(true);
    assert_eq!(
        filter.prep_pending(),
        layers,
        "disabling discards prepared layers"
    );
    filter.prepare_while(|| true);
    filter.reset();
    assert_eq!(
        filter.prep_pending(),
        layers,
        "reset discards prepared layers"
    );
}

/// Preparing every layer between calls gives bit for bit the baseline
/// stream: one- and two-hop calls, both embedded models.
#[test]
fn full_gru_prep_matches_the_baseline_stream() {
    let input = signal();
    for weights in MODELS {
        let (expected, expected_snr) = stream(
            &mut DeepFilter::from_weights(weights).unwrap(),
            &input,
            |_, _| {},
        );
        let mut staged = DeepFilter::from_weights(weights).unwrap();
        staged.set_gru_prep(true);
        // Including the zero states before the first call.
        staged.prepare_while(|| true);
        let (actual, actual_snr) = stream(&mut staged, &input, |f, _| {
            f.prepare_while(|| true);
            assert_eq!(f.prep_pending(), 0);
        });
        assert!(expected.iter().any(|&x| x != 0.));
        assert_eq!(bits(&actual), bits(&expected));
        assert_eq!(actual_snr, expected_snr);
    }
}

/// Abandoning preparation after any number of layers, or never starting it,
/// leaves the stream unchanged: unprepared layers are computed inline.
#[test]
fn partial_gru_prep_matches_the_baseline_stream() {
    let input = signal();
    for weights in MODELS {
        let (expected, expected_snr) = stream(
            &mut DeepFilter::from_weights(weights).unwrap(),
            &input,
            |_, _| {},
        );
        let mut staged = DeepFilter::from_weights(weights).unwrap();
        staged.set_gru_prep(true);
        let layers = staged.prep_pending();
        let mut seed = 0x9e37_79b9u32;
        let mut counts = vec![0usize; layers + 1];
        let (actual, actual_snr) = stream(&mut staged, &input, |f, _| {
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let mut budget = (seed >> 16) as usize % (layers + 1);
            let want = budget;
            let done = f.prepare_while(|| {
                let go = budget > 0;
                budget = budget.saturating_sub(1);
                go
            });
            assert_eq!(done, want);
            counts[want] += 1;
        });
        assert!(
            counts.iter().all(|&c| c > 0),
            "every prefix length occurs: {counts:?}"
        );
        assert_eq!(bits(&actual), bits(&expected));
        assert_eq!(actual_snr, expected_snr);
    }
}

/// A reset after preparation must not reuse products of the forgotten
/// hidden states, and turning preparation off discards them too.
#[test]
fn reset_and_disabling_discard_prepared_products() {
    let input = signal();
    for weights in MODELS {
        let reset_at = |i: usize| i % 37 == 11;
        let (expected, expected_snr) = stream(
            &mut DeepFilter::from_weights(weights).unwrap(),
            &input,
            |f, i| {
                if reset_at(i) {
                    f.reset();
                }
            },
        );
        let mut staged = DeepFilter::from_weights(weights).unwrap();
        staged.set_gru_prep(true);
        let (actual, actual_snr) = stream(&mut staged, &input, |f, i| {
            f.prepare_while(|| true);
            if reset_at(i) {
                f.reset();
                // Prepared again from the zero states, or not at all.
                if i % 2 == 0 {
                    f.prepare_while(|| true);
                }
            }
            if i % 23 == 5 {
                f.set_gru_prep(false);
                f.set_gru_prep(true);
            }
        });
        assert_eq!(bits(&actual), bits(&expected));
        assert_eq!(actual_snr, expected_snr);
    }
}

/// A call skipped by the caller (a dropped input) leaves the hidden states
/// and so the prepared products valid; a call that runs consumes them.
#[test]
fn prepared_products_follow_the_hidden_state_not_the_clock() {
    let input = signal();
    let hop = DeepFilter::new().unwrap().framing.hop;
    // Hops 3, 9, ... never reach either filter.
    let kept: Vec<f32> = input
        .chunks_exact(hop)
        .enumerate()
        .filter(|(i, _)| i % 6 != 3)
        .flat_map(|(_, c)| c.iter().copied())
        .collect();
    let mut baseline = DeepFilter::new().unwrap();
    let mut staged = DeepFilter::new().unwrap();
    staged.set_gru_prep(true);
    let mut expected = kept.clone();
    let mut actual = kept;
    for (i, (a, e)) in actual
        .chunks_exact_mut(hop)
        .zip(expected.chunks_exact_mut(hop))
        .enumerate()
    {
        baseline.process_hops(e);
        staged.process_hops(a);
        staged.prepare_while(|| true);
        if i % 5 == 2 {
            // Asked again where a dropped hop would have been: nothing to do.
            assert_eq!(staged.prepare_while(|| true), 0);
        }
    }
    assert_eq!(bits(&actual), bits(&expected));
}

/// A click can show up from its own frame on (the deep filter's taps reach
/// `lookahead` frames ahead of the frame they rebuild, so the newest
/// spectrum feeds every output frame), but never before.
fn click_never_shows_early(filter: &mut DeepFilter) {
    let frame = filter.frame();
    let at = filter.lookahead() + 1;
    let mut input = vec![0f32; frame * (at + 6)];
    input[at * frame] = 0.5;
    let out = run(filter, &input);
    assert!(out.iter().all(|x| x.is_finite()));
    assert!(out[..at * frame].iter().all(|&x| x == 0.));
    assert!(out[at * frame..].iter().any(|&x| x != 0.));
    // The click's own spectrum is enhanced `delay()` samples later.
    let delayed = at * frame + filter.delay();
    assert!(out[delayed..delayed + frame].iter().any(|&x| x != 0.));
}

#[test]
fn silence_stays_silent_until_a_click() {
    click_never_shows_early(&mut DeepFilter::new().unwrap());
}

#[test]
fn loads_a_model_without_lookahead() {
    let mut filter = DeepFilter::from_weights(&reframed(960, 960, 480, 0)).unwrap();
    assert_eq!(filter.frame(), 480);
    assert_eq!(filter.lookahead(), 0);
    assert_eq!(filter.delay(), 480);
    click_never_shows_early(&mut filter);
    let out = run(&mut filter, &signal());
    assert!(out.iter().all(|x| x.is_finite()));
}

/// The 960-point analysis at a 240 hop: the call size varies, but the delay is
/// the synthesis window's overlap, and the model's bins are unchanged.
#[test]
fn loads_an_asymmetric_framing() {
    for (synthesis, delay) in [(480, 240), (360, 120), (300, 60), (240, 0)] {
        let mut filter = DeepFilter::from_weights(&reframed(960, synthesis, 240, 0)).unwrap();
        assert_eq!(filter.frame(), 240);
        assert_eq!(filter.lookahead(), 0);
        assert_eq!(filter.delay(), delay, "synthesis {synthesis}");
        click_never_shows_early(&mut filter);
        let out = run(&mut filter, &signal());
        assert!(out.iter().all(|x| x.is_finite()));
    }
    let filter = DeepFilter::from_weights(&reframed(960, 480, 240, 1)).unwrap();
    assert_eq!(filter.delay(), 240 + 240);
}

#[test]
fn rejects_an_invalid_framing() {
    // The synthesis window spans one to two hops within the FFT (even), the
    // lookahead is under the filter order (5 for this model), and the deep
    // filter must fit in the spectrum.
    for (fft, synthesis, hop, lookahead) in [
        (960, 960, 0, 0),
        (960, 960, 240, 0),
        (960, 239, 240, 0),
        (480, 960, 480, 0),
        (961, 480, 240, 0),
        (960, 960, 480, 5),
        (96, 96, 48, 0),
    ] {
        assert_eq!(
            DeepFilter::from_weights(&reframed(fft, synthesis, hop, lookahead)).err(),
            Some(Error::Weights),
            "{fft} {synthesis} {hop} {lookahead}"
        );
    }
    // Framing tensors must be present and integral.
    let mut without = Vec::new();
    let mut count = 0u32;
    let mut body = Vec::new();
    let mut r = &WEIGHTS[4..];
    let total = u32::from_le_bytes(WEIGHTS[..4].try_into().unwrap());
    for _ in 0..total {
        let l = u32::from_le_bytes(r[..4].try_into().unwrap()) as usize;
        let name = &r[4..4 + l];
        let rank = u32::from_le_bytes(r[4 + l..8 + l].try_into().unwrap()) as usize;
        let n: usize = (0..rank)
            .map(|i| u32::from_le_bytes(r[8 + l + 4 * i..12 + l + 4 * i].try_into().unwrap()))
            .product::<u32>() as usize;
        let len = 8 + l + 4 * rank + 4 * n;
        if name != b"rc2fx.lookahead" {
            body.extend_from_slice(&r[..len]);
            count += 1;
        }
        r = &r[len..];
    }
    without.extend_from_slice(&count.to_le_bytes());
    without.extend_from_slice(&body);
    assert_eq!(
        DeepFilter::from_weights(&without).err(),
        Some(Error::Weights)
    );
    let mut fractional = reframed(960, 960, 480, 0);
    let at = fractional
        .windows(15)
        .position(|w| w == b"rc2fx.lookahead")
        .unwrap();
    // name, rank (1), dim (1), then the value.
    let value = at + 15 + 8;
    fractional[value..value + 4].copy_from_slice(&1.5f32.to_le_bytes());
    assert_eq!(
        DeepFilter::from_weights(&fractional).err(),
        Some(Error::Weights)
    );
}

#[test]
fn rejects_malformed_weights() {
    assert_eq!(Tensors::parse(&WEIGHTS[..100]).err(), Some(Error::Weights));
    // A tensor count with nothing after it.
    assert_eq!(
        Tensors::parse(&1u32.to_le_bytes()).err(),
        Some(Error::Weights)
    );
    // Well formed, but not a DeepFilterNet3.
    let mut empty = Vec::new();
    empty.extend_from_slice(&0u32.to_le_bytes());
    assert_eq!(
        Dfn::new(&Tensors::parse(&empty).unwrap()).err(),
        Some(Error::Weights)
    );
}

/// Writes the reference input; render model/reference.wav from it as
/// described in model/README.md.
#[test]
#[ignore]
fn write_reference_input() {
    let path = std::env::var("RC2FX_DFN_INPUT").expect("RC2FX_DFN_INPUT=path.wav");
    let samples = signal();
    let mut wav = Vec::new();
    let data = (samples.len() * 2) as u32;
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&(dsp::SAMPLE_RATE as u32).to_le_bytes());
    wav.extend_from_slice(&(dsp::SAMPLE_RATE as u32 * 2).to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        wav.extend_from_slice(&((s * 32768.) as i16).to_le_bytes());
    }
    std::fs::write(path, wav).unwrap();
}

/// Loads a candidate weights file, reports its framing and runs the test
/// signal through it: a host-side check of an export before it goes to the
/// mixer. `RC2FX_DFN_WEIGHTS=path cargo test -p rc2fx-dfn --release -- --ignored candidate_weights`;
/// with `RC2FX_DFN_REFERENCE=reference.wav` (handoff.sh's dfref output for the
/// same weights) it must also match DeepFilterNet as closely as the embedded model.
#[test]
#[ignore]
fn candidate_weights() {
    let path = std::env::var("RC2FX_DFN_WEIGHTS").expect("RC2FX_DFN_WEIGHTS=weights.bin");
    let bytes = std::fs::read(&path).unwrap();
    let mut filter = DeepFilter::from_weights(&bytes).unwrap();
    eprintln!(
        "{path}: frame {} samples, delay {} samples (synthesis overlap plus lookahead {} frames)",
        filter.frame(),
        filter.delay(),
        filter.lookahead()
    );
    let input = signal();
    let started = std::time::Instant::now();
    let out = run(&mut filter, &input);
    let frames = input.len() / filter.frame();
    eprintln!(
        "{frames} frames in {:.1} ms on this host",
        started.elapsed().as_secs_f64() * 1e3
    );
    assert!(out.iter().all(|x| x.is_finite()));
    if let Ok(reference) = std::env::var("RC2FX_DFN_REFERENCE") {
        assert_matches_reference(&out, &wav_samples(&std::fs::read(reference).unwrap()));
    }
}
