//! One mono voice processor alongside a stereo bypass, followed by the ordinary
//! output effects. The separate microphone stem is mandatory: pads and playback
//! never become model input or voice-gate input.
use super::*;

#[derive(Clone)]
pub(crate) struct PreparedChat {
    voice: Prepared,
    output: Prepared,
}
impl PreparedChat {
    pub fn new(target: &Target) -> Result<Self, CoreError> {
        let mut voice = Target::new("chat-voices", "Chat voices", TargetKind::Mic, 1);
        voice.suppression = target.suppression.clone();
        voice.bypass = target.bypass;
        let mut output = target.clone();
        output.suppression.enabled = false;
        Ok(Self {
            voice: Prepared::new(&voice, 48_000)?,
            output: Prepared::new(&output, 48_000)?,
        })
    }
}

/// Construction/retirement on control; a single callback owns all processing.
/// The bypass has the same fixed delay as the voice model, including dry fallback.
pub struct Chat {
    voice: Dsp,
    output: Dsp,
    mixed_delay: [Box<[f32]>; 2],
    voice_delay: [Box<[f32]>; 2],
    position: usize,
}
impl Chat {
    pub fn new(p: &Prepared, provider: Box<dyn Suppression>) -> Result<Self, DspError> {
        let cp = p.chat.as_ref().ok_or(DspError::MissingChatStem)?;
        let voice = Dsp::new(&cp.voice, [Some(provider), None])?;
        let latency = voice.latency_samples();
        Ok(Self {
            voice,
            output: Dsp::without_suppression(&cp.output)?,
            mixed_delay: std::array::from_fn(|_| vec![0.; latency].into_boxed_slice()),
            voice_delay: std::array::from_fn(|_| vec![0.; latency].into_boxed_slice()),
            position: 0,
        })
    }
    pub fn latency_samples(&self) -> usize {
        self.voice.latency_samples()
    }
    pub fn drain_suppression(
        &mut self,
        timeout: std::time::Duration,
    ) -> Result<(), RetirementError> {
        self.voice.drain_suppression(timeout)
    }
    /// `microphones` is the routed, gained microphone contribution to `mixed`,
    /// captured before non-voice content is joined. Neither slice is retained.
    pub fn process(
        &mut self,
        p: &Prepared,
        microphones: [&[f32]; 2],
        mixed: [&mut [f32]; 2],
    ) -> Result<Meter, DspError> {
        let cp = p.chat.as_ref().ok_or(DspError::MissingChatStem)?;
        if self.voice.retired {
            return Err(DspError::Retired);
        }
        if self.voice.signature != cp.voice.signature
            || self.output.signature != cp.output.signature
        {
            return Err(DspError::IncompatibleSnapshot);
        }
        let n = mixed[0].len();
        if mixed[1].len() != n || microphones.iter().any(|v| v.len() != n) {
            return Err(DspError::BufferShape);
        }
        // Reject before advancing history or modifying any output.
        if microphones.into_iter().flatten().any(|v| !v.is_finite()) {
            return Err(DspError::MissingChatStem);
        }
        let mut voice_meter = Meter::default();
        let mut input_peak = 0.0f64;
        let vp = &cp.voice;
        for i in 0..n {
            let mono =
                (0.5 * f64::from(microphones[0][i]) + 0.5 * f64::from(microphones[1][i])) as f32;
            let mono = safe(f64::from(mono), 16., &mut voice_meter) as f32;
            self.voice.wet += GAIN_SMOOTH * (vp.wet - self.voice.wet);
            self.voice.processing_blend +=
                GAIN_SMOOTH * ((if vp.bypass { 0. } else { 1. }) - self.voice.processing_blend);
            let provider = self.voice.channels[0]
                .suppression
                .as_mut()
                .ok_or(DspError::MissingSuppression)?;
            let enhanced = provider.process_sample(mono, self.voice.wet as f32);
            let score = provider.voice().filter(|v| v.is_finite());
            let at = self.position;
            let dry_voice = [self.voice_delay[0][at], self.voice_delay[1][at]];
            let dry_mono = 0.5 * f64::from(dry_voice[0]) + 0.5 * f64::from(dry_voice[1]);
            // A bad provider must not poison the recombined pad/playback signal.
            let (enhanced, score) = if enhanced.is_finite() {
                (f64::from(enhanced), score)
            } else {
                (dry_mono, None)
            };
            let gate = self.voice.gate_step(vp, score, &mut voice_meter);
            let blend = self.voice.processing_blend;
            let processed_mono = enhanced * gate * blend + dry_mono * (1. - blend);
            // Preserve the original stereo voice at dry blend/bypass. At fully
            // wet, microphones are mono; pads/playback retain their stereo image.
            let side_gain = (1. - self.voice.wet) * gate * blend + (1. - blend);
            for ch in 0..2 {
                let delayed = self.mixed_delay[ch][at];
                let raw = f64::from(mixed[ch][i]);
                if raw.is_finite() {
                    input_peak = input_peak.max(raw.abs());
                }
                self.mixed_delay[ch][at] = mixed[ch][i];
                self.voice_delay[ch][at] = microphones[ch][i];
                let voice = processed_mono + (f64::from(dry_voice[ch]) - dry_mono) * side_gain;
                mixed[ch][i] = (f64::from(delayed) - f64::from(dry_voice[ch]) + voice) as f32;
            }
            self.position = (at + 1) % self.latency_samples();
        }
        let mut meter = self.output.process_in_place(&cp.output, mixed)?;
        meter.input_peak = input_peak;
        meter.voice = voice_meter.voice;
        meter.gate_open = voice_meter.gate_open;
        Ok(meter)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    struct SilentVoice {
        calls: Arc<AtomicUsize>,
        contaminated: Arc<AtomicUsize>,
    }
    impl Suppression for SilentVoice {
        fn geometry(&self) -> SuppressionGeometry {
            rc2fx_core::Suppression::admitted_geometry(0).unwrap()
        }
        fn process_sample(&mut self, input: f32, _: f32) -> f32 {
            self.calls.fetch_add(1, Ordering::Relaxed);
            // Both voice inputs are constant 0.125. Pad content must not reach us.
            if input != 0.125 {
                self.contaminated.fetch_add(1, Ordering::Relaxed);
            }
            0.
        }
        fn voice(&self) -> Option<f32> {
            Some(0.)
        }
        fn drain(&mut self, _: std::time::Duration) -> Result<(), RetirementError> {
            Ok(())
        }
    }
    fn target() -> Target {
        let mut t = Target::new("send-chat", "Chat", TargetKind::Output, 2);
        t.suppression.enabled = true;
        t
    }
    #[test]
    fn stereo_pads_bypass_the_only_model_and_a_closed_voice_gate() {
        let mut t = target();
        t.suppression.gate.strength = 1.;
        let p = Prepared::new(&t, 48_000).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let contaminated = Arc::new(AtomicUsize::new(0));
        let mut chat = Chat::new(
            &p,
            Box::new(SilentVoice {
                calls: calls.clone(),
                contaminated: contaminated.clone(),
            }),
        )
        .unwrap();
        let n = 8_000;
        let delay = chat.latency_samples();
        let voices = vec![0.125; n];
        let pads_l: Vec<f32> = (0..n).map(|i| ((i % 17) as f32 - 8.) / 64.).collect();
        let pads_r: Vec<f32> = (0..n).map(|i| ((i % 23) as f32 - 11.) / 64.).collect();
        let mut left: Vec<_> = pads_l.iter().map(|x| x + 0.125).collect();
        let mut right: Vec<_> = pads_r.iter().map(|x| x + 0.125).collect();
        chat.process(&p, [&voices, &voices], [&mut left, &mut right])
            .unwrap();
        assert_eq!(
            calls.load(Ordering::Relaxed),
            n,
            "one stream, not two stereo denoisers"
        );
        assert_eq!(
            contaminated.load(Ordering::Relaxed),
            0,
            "pads never enter the model"
        );
        assert_eq!(&left[delay..], &pads_l[..n - delay]);
        assert_eq!(&right[delay..], &pads_r[..n - delay]);
    }
    struct InvalidVoice;
    impl Suppression for InvalidVoice {
        fn geometry(&self) -> SuppressionGeometry {
            rc2fx_core::Suppression::admitted_geometry(0).unwrap()
        }
        fn process_sample(&mut self, _: f32, _: f32) -> f32 {
            f32::NAN
        }
        fn voice(&self) -> Option<f32> {
            Some(f32::NAN)
        }
        fn drain(&mut self, _: std::time::Duration) -> Result<(), RetirementError> {
            Ok(())
        }
    }
    #[test]
    fn invalid_model_output_cannot_silence_or_poison_pads() {
        let p = Prepared::new(&target(), 48_000).unwrap();
        let mut chat = Chat::new(&p, Box::new(InvalidVoice)).unwrap();
        let delay = chat.latency_samples();
        let voice = vec![0.125; 3000];
        let mut left = vec![0.375; 3000];
        let mut right = vec![-0.125; 3000];
        chat.process(&p, [&voice, &voice], [&mut left, &mut right])
            .unwrap();
        assert!(left[delay..].iter().all(|&v| v == 0.375));
        assert!(right[delay..].iter().all(|&v| v == -0.125));
    }
    struct DelayedSilentVoice {
        buffer: Vec<f32>,
        position: usize,
    }
    impl Suppression for DelayedSilentVoice {
        fn geometry(&self) -> SuppressionGeometry {
            rc2fx_core::Suppression::admitted_geometry(0).unwrap()
        }
        fn process_sample(&mut self, input: f32, wet: f32) -> f32 {
            let dry = self.buffer[self.position];
            self.buffer[self.position] = input;
            self.position = (self.position + 1) % self.buffer.len();
            dry * (1. - wet)
        }
        fn voice(&self) -> Option<f32> {
            None
        }
        fn drain(&mut self, _: std::time::Duration) -> Result<(), RetirementError> {
            Ok(())
        }
    }
    #[test]
    fn dry_blend_and_bypass_preserve_stereo_with_the_same_delay() {
        for (wet, bypass) in [(0., false), (1., true)] {
            let mut t = target();
            t.suppression.wet = wet;
            t.bypass = bypass;
            let p = Prepared::new(&t, 48_000).unwrap();
            let delay = t.suppression.geometry().unwrap().latency_samples() as usize;
            let mut chat = Chat::new(
                &p,
                Box::new(DelayedSilentVoice {
                    buffer: vec![0.; delay],
                    position: 0,
                }),
            )
            .unwrap();
            let voice_l = vec![0.25; 4000];
            let voice_r = vec![-0.125; 4000];
            let mut left = vec![0.375; 4000];
            let mut right = vec![0.125; 4000];
            chat.process(&p, [&voice_l, &voice_r], [&mut left, &mut right])
                .unwrap();
            assert!(left[..delay].iter().all(|&v| v == 0.));
            assert!(right[..delay].iter().all(|&v| v == 0.));
            assert!(left[delay..].iter().all(|&v| v == 0.375));
            assert!(right[delay..].iter().all(|&v| v == 0.125));
        }
    }
    #[test]
    fn output_effect_order_does_not_move_or_replace_the_voice_model() {
        let mut t = target();
        let first = Prepared::new(&t, 48_000).unwrap();
        t.order.swap(0, 5);
        let next = Prepared::new(&t, 48_000).unwrap();
        assert!(!first.requires_reset(&next));
    }
    #[test]
    fn ordinary_output_processing_refuses_to_denoise_the_completed_mix() {
        let p = Prepared::new(&target(), 48_000).unwrap();
        assert!(matches!(
            Dsp::without_suppression(&p),
            Err(DspError::MissingChatStem)
        ));
    }
    #[test]
    fn absent_or_invalid_stem_does_not_advance_the_worker_or_touch_audio() {
        let p = Prepared::new(&target(), 48_000).unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let mut chat = Chat::new(
            &p,
            Box::new(SilentVoice {
                calls: calls.clone(),
                contaminated: Arc::new(AtomicUsize::new(0)),
            }),
        )
        .unwrap();
        let mut left = [0.25; 8];
        let mut right = [-0.25; 8];
        assert_eq!(
            chat.process(&p, [&[], &[]], [&mut left, &mut right])
                .unwrap_err(),
            DspError::BufferShape
        );
        let invalid = [f32::NAN; 8];
        assert_eq!(
            chat.process(&p, [&invalid, &invalid], [&mut left, &mut right])
                .unwrap_err(),
            DspError::MissingChatStem
        );
        assert_eq!(left, [0.25; 8]);
        assert_eq!(right, [-0.25; 8]);
        assert_eq!(calls.load(Ordering::Relaxed), 0);
    }
}
