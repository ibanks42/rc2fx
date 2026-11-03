use crate::{Error, Geometry};

pub(crate) trait Model: Send {
    /// Processes one frame in place and returns its voice score (see
    /// `VendorModel::process`).
    fn process(&mut self, samples: &mut [f32]) -> Result<f32, Error>;
    fn reset(&mut self) -> Result<(), Error>;
    /// Called once on the worker thread before it reports ready, so any
    /// per-thread or one-time setup never lands on a live frame.
    fn warm_thread(&mut self) -> Result<(), Error> {
        Ok(())
    }
    /// One bounded step of work for the next frame, run after a frame's
    /// result is published; never changes results.
    /// False when there was nothing to do.
    fn prepare_step(&mut self) -> bool {
        false
    }
    /// Steps `prepare_step` could still take before the next frame.
    fn prep_pending(&self) -> usize {
        0
    }
}

/// Direct model-frame interface for offline validation/control-side use. Never
/// call this from an audio callback; Worker is the callback-facing interface.
pub struct VendorModel {
    filter: Box<rc2fx_dfn::DeepFilter>,
    geometry: Geometry,
    failed: bool,
}
impl VendorModel {
    pub fn new(lookahead: u8) -> Result<Self, Error> {
        let geometry = Geometry::for_lookahead(lookahead)?;
        let filter =
            rc2fx_dfn::DeepFilter::for_lookahead(lookahead).map_err(|_| Error::ModelCreation)?;
        Self::with_filter(geometry, filter)
    }
    /// DeepFilterNet on the given weights file instead of the embedded model,
    /// with the geometry those weights define and no worker lead: for timing
    /// a candidate model on the mixer before it is trained or admitted.
    pub fn deep_filter_from_weights(weights: &[u8]) -> Result<Self, Error> {
        let filter =
            rc2fx_dfn::DeepFilter::from_weights(weights).map_err(|_| Error::ModelCreation)?;
        let geometry = Geometry {
            sample_rate: 48_000,
            frame_samples: filter.frame(),
            intrinsic_delay_samples: filter.delay(),
            worker_lead_samples: 0,
        };
        Self::with_filter(geometry, filter)
    }
    fn with_filter(geometry: Geometry, mut filter: rc2fx_dfn::DeepFilter) -> Result<Self, Error> {
        filter.set_layer_pause(None);
        // Staged GRU preparation: the worker prepares the next frame's
        // recurrent products after publishing each result.
        filter.set_gru_prep(true);
        if geometry.frame_samples != filter.frame()
            || geometry.intrinsic_delay_samples != filter.delay()
        {
            return Err(Error::ModelGeometry);
        }
        Ok(Self {
            filter: Box::new(filter),
            geometry,
            failed: false,
        })
    }
    pub fn geometry(&self) -> Geometry {
        self.geometry
    }
    /// Overrides DeepFilterNet3's pause between layers (see `layer_pause`);
    /// for measuring pause lengths with `rc2fx-bench`.
    pub fn set_layer_pause(&mut self, pause: Option<rc2fx_dfn::LayerPause>) {
        self.filter.set_layer_pause(pause);
    }
    /// Entire frame is validated before model history or caller output changes.
    /// Finite headroom is clamped exactly as the old normalized factory did.
    ///
    /// Returns the frame's voice score from 0 (no speech) to 1: DeepFilterNet's
    /// local SNR estimate mapped linearly from -15 dB (the bottom of its range)
    /// to +15 dB, so 0 dB local SNR scores 0.5.
    pub fn process(&mut self, samples: &mut [f32]) -> Result<f32, Error> {
        if samples.len() != self.geometry.frame_samples || samples.iter().any(|x| !x.is_finite()) {
            return Err(Error::InvalidFrame);
        }
        if self.failed {
            return Err(Error::ModelFailed);
        }
        for x in samples.iter_mut() {
            *x = x.clamp(-1., 1.);
        }
        let lsnr = self.filter.process(samples);
        if !lsnr.is_finite() || samples.iter().any(|x| !x.is_finite()) {
            self.failed = true;
            return Err(Error::ModelFailed);
        }
        Ok(((lsnr + 15.) / 30.).clamp(0., 1.))
    }
    /// One GRU layer of DeepFilterNet3's next frame prepared ahead
    /// (`rc2fx_dfn::DeepFilter::prepare_step`). False when there was nothing to do
    /// or after a failure.
    pub fn prepare_step(&mut self) -> bool {
        !self.failed && self.filter.prepare_step()
    }
    pub fn prep_pending(&self) -> usize {
        if self.failed {
            0
        } else {
            self.filter.prep_pending()
        }
    }
    pub fn reset(&mut self) -> Result<(), Error> {
        self.filter.reset();
        self.failed = false;
        Ok(())
    }
}
impl Model for VendorModel {
    fn process(&mut self, samples: &mut [f32]) -> Result<f32, Error> {
        self.process(samples)
    }
    fn reset(&mut self) -> Result<(), Error> {
        self.reset()
    }
    fn prepare_step(&mut self) -> bool {
        self.prepare_step()
    }
    fn prep_pending(&self) -> usize {
        self.prep_pending()
    }
}
