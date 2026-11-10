//! df_convp's temporal convolution: `kt` frames of c0 to the deep filter's
//! coefficient pathway, in two groups.
//!
//! Per group it is a pointwise convolution over `kt` frames of the group's
//! c0 channels. Output n sums its taps in this order, which the results
//! depend on and which this module keeps exactly: taps 0 to kt-2 read frames
//! n-(kt-2) to n, oldest first, and the last tap, kt-1, reads frame n-(kt-1).
//! The engine has summed them this way since it was written. A causal
//! convolution in the training framework pairs tap j with frame n-(kt-1)+j
//! instead; whether the difference is intended is an open question, and
//! changing the pairing would change output samples, so it is left alone
//! here.
//!
//! Instead of keeping `kt` frames of c0 and reading them all for every
//! output, each frame is added, as it arrives, to the running sum of every
//! output it belongs to: tap j of frame n goes to output n+(kt-2)-j. Frame n
//! is the last frame output n waits for except for its final tap, which
//! needs the frame kt-1 back; that one frame per output is all the history
//! kept. Each output still receives the same multiply-adds in the same
//! order, so the sums are bit for bit those of the full pointwise
//! convolution (`reference::History`, kept for the tests).
use crate::kernels::{Weights, pack_pointwise, pointwise_onto};

pub(crate) struct Convp {
    /// Per group, [op][kt * gin] packed for `pointwise` (four outputs per
    /// input), the `half` outputs padded to `op` with zero rows.
    w: Vec<Weights>,
    /// Input channels per group.
    gin: usize,
    /// Outputs per group.
    half: usize,
    kt: usize,
    /// Bins per channel.
    f: usize,
    /// Per group, kt-1 frames of the group's c0 ([gin][f] each): frame m is
    /// in slot m % (kt-1) until frame m+kt-1 has read it and takes its place.
    ring: Vec<Vec<f32>>,
    /// Per group, `slots()` running sums ([half][f] each): output m's is in
    /// slot m % slots(). The slot of the output finished last stays valid
    /// until the next frame arrives.
    sums: Vec<Vec<f32>>,
    /// The next frame's index modulo `slots()`.
    pos: usize,
    /// The same convolution by the original method; it runs instead while
    /// `crate::reference` is on.
    #[cfg(test)]
    reference: reference::History,
}

impl Convp {
    /// `w` is the conv's [2 * half][gin][kt][1] weight.
    pub(crate) fn new(w: &[f32], half: usize, gin: usize, kt: usize, f: usize) -> Self {
        assert!(kt > 0 && w.len() == 2 * half * gin * kt);
        let op = half.next_multiple_of(4);
        let slots = (kt - 1).max(1);
        Self {
            w: (0..2)
                .map(|g| {
                    let mut p = vec![0f32; op * kt * gin];
                    for o in 0..half {
                        for i in 0..gin {
                            for t in 0..kt {
                                p[o * kt * gin + t * gin + i] =
                                    w[((g * half + o) * gin + i) * kt + t];
                            }
                        }
                    }
                    Weights::rows(&pack_pointwise(&p, op, kt * gin), 4)
                })
                .collect(),
            gin,
            half,
            kt,
            f,
            ring: vec![vec![0.; (kt - 1) * gin * f]; 2],
            sums: vec![vec![0.; slots * half * f]; 2],
            pos: 0,
            #[cfg(test)]
            reference: reference::History::new(half, gin, kt, f),
        }
    }

    fn slots(&self) -> usize {
        (self.kt - 1).max(1)
    }

    /// Forgets every frame: zero sums are what zero frames add up to.
    pub(crate) fn reset(&mut self) {
        self.ring.iter_mut().for_each(|r| r.fill(0.));
        self.sums.iter_mut().for_each(|s| s.fill(0.));
        self.pos = 0;
        #[cfg(test)]
        self.reference.reset();
    }

    /// Adds one frame of c0 ([2 * gin][f]) and finishes its output, which
    /// `mix` then reads.
    pub(crate) fn step(&mut self, c0: &[f32]) {
        #[cfg(test)]
        if crate::reference::on() {
            return self.reference.step(&self.w, c0);
        }
        let (gin, half, kt, f) = (self.gin, self.half, self.kt, self.f);
        let (slot, sum, total) = (gin * f, half * f, kt * gin);
        let (slots, pos) = (self.slots(), self.pos);
        for (g, ((w, ring), sums)) in self
            .w
            .iter()
            .zip(&mut self.ring)
            .zip(&mut self.sums)
            .enumerate()
        {
            let new = &c0[g * slot..(g + 1) * slot];
            // Every tap but the last reads the new frame: tap j belongs to
            // the output kt-2-j frames ahead. Tap 0 starts a new output's
            // sum, in the slot of the output finished one frame ago.
            for j in 0..kt - 1 {
                let at = (pos + kt - 2 - j) % slots;
                let y = &mut sums[at * sum..(at + 1) * sum];
                pointwise_onto(w, half, total, j * gin, gin, new, f, y, j == 0);
            }
            // The last tap finishes this frame's output from the frame kt-1
            // back, which the new frame then replaces.
            let y = &mut sums[pos * sum..(pos + 1) * sum];
            if kt == 1 {
                pointwise_onto(w, half, total, 0, gin, new, f, y, true);
            } else {
                let old = &mut ring[pos * slot..(pos + 1) * slot];
                #[cfg(test)]
                {
                    use crate::traffic::*;
                    touch(Class::State, old.as_ptr(), slot * 4);
                    store(Class::State, old.as_ptr(), slot * 4);
                }
                pointwise_onto(w, half, total, (kt - 1) * gin, gin, old, f, y, false);
                old.copy_from_slice(new);
            }
        }
        self.pos = (pos + 1) % slots;
    }

    /// Group `g`'s output of the last `step`, [half][f].
    fn out(&self, g: usize) -> &[f32] {
        let sum = self.half * self.f;
        let at = (self.pos + self.slots() - 1) % self.slots();
        &self.sums[g][at * sum..(at + 1) * sum]
    }

    /// The 1x1 convolution after the temporal one, over the last `step`'s
    /// outputs of both groups: z[o][f] = sum_i pw[o][i] y[i][f], each sum
    /// starting from zero and taking its inputs in order. z is
    /// [2 * half][f].
    pub(crate) fn mix(&self, pw: &[f32], z: &mut [f32]) {
        #[cfg(test)]
        if crate::reference::on() {
            return self.reference.mix(pw, z);
        }
        let (half, f) = (self.half, self.f);
        let out = 2 * half;
        let y = [self.out(0), self.out(1)];
        // Every output reads every conv output over every bin.
        #[cfg(test)]
        for y in y {
            use crate::traffic::*;
            touch_n(Class::State, y.as_ptr(), half * f * 4, out);
        }
        z[..out * f].fill(0.);
        for (o, z) in z[..out * f].chunks_exact_mut(f).enumerate() {
            for i in 0..out {
                let w = pw[o * out + i];
                let row = &y[i / half][(i % half) * f..][..f];
                for (z, &y) in z.iter_mut().zip(row) {
                    *z += w * y;
                }
            }
        }
    }
}

/// The convolution and the mix after it as they were first written, kept as
/// the tests' reference: `kt` frames of c0 per group, read in full by
/// `pointwise` for every output.
#[cfg(test)]
pub(crate) mod reference {
    use crate::kernels::{Weights, pointwise};

    pub(crate) struct History {
        gin: usize,
        half: usize,
        kt: usize,
        nb_df: usize,
        // Per group, 2 kt slots of [gin][nb_df]; frame n goes to slots
        // n % kt and n % kt + kt, so the last kt frames are always
        // contiguous.
        hist: Vec<Vec<f32>>,
        pos: usize,
        /// [2][op][nb_df]: the last step's outputs, zero-padded rows
        /// included.
        y: Vec<f32>,
    }

    impl History {
        pub(crate) fn new(half: usize, gin: usize, kt: usize, nb_df: usize) -> Self {
            Self {
                gin,
                half,
                kt,
                nb_df,
                hist: vec![vec![0.; 2 * kt * gin * nb_df]; 2],
                pos: 0,
                y: vec![0.; 2 * half.next_multiple_of(4) * nb_df],
            }
        }

        pub(crate) fn reset(&mut self) {
            self.hist.iter_mut().for_each(|h| h.fill(0.));
            self.pos = 0;
        }

        /// `Convp::step` by the original method, with `Convp`'s weights.
        pub(crate) fn step(&mut self, w: &[Weights], c0: &[f32]) {
            let (gin, kt, nb_df) = (self.gin, self.kt, self.nb_df);
            let op = self.half.next_multiple_of(4);
            let slot = gin * nb_df;
            let pos = self.pos;
            for (g, hist) in self.hist.iter_mut().enumerate() {
                let src = &c0[g * slot..(g + 1) * slot];
                hist[pos * slot..(pos + 1) * slot].copy_from_slice(src);
                hist[(pos + kt) * slot..(pos + kt + 1) * slot].copy_from_slice(src);
            }
            self.pos = (pos + 1) % kt;
            let y = &mut self.y[..2 * op * nb_df];
            let start = (self.pos + 1) * slot;
            for (g, w) in w.iter().enumerate() {
                let x = &self.hist[g][start..start + kt * slot];
                pointwise(
                    w,
                    op,
                    kt * gin,
                    x,
                    nb_df,
                    &mut y[g * op * nb_df..(g + 1) * op * nb_df],
                );
            }
        }

        /// Group `g`'s output of the last `step`, [half][nb_df].
        pub(crate) fn out(&self, g: usize) -> &[f32] {
            let op = self.half.next_multiple_of(4);
            &self.y[g * op * self.nb_df..][..self.half * self.nb_df]
        }

        /// `Convp::mix` as it was first written.
        pub(crate) fn mix(&self, pw: &[f32], z: &mut [f32]) {
            let (half, nb_df) = (self.half, self.nb_df);
            let (out, op) = (2 * half, half.next_multiple_of(4));
            let y = &self.y;
            let z = &mut z[..out * nb_df];
            z.fill(0.);
            for o in 0..out {
                for i in 0..out {
                    let w = pw[o * out + i];
                    let row = (i / half) * op + i % half;
                    for f in 0..nb_df {
                        z[o * nb_df + f] += w * y[row * nb_df + f];
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::glue::tests::{CLASSES, Rng, assert_same, each_float_mode};

    fn bits(x: &[f32]) -> Vec<u32> {
        x.iter().map(|v| v.to_bits()).collect()
    }

    /// Runs `frames` through both forms and compares every output; returns
    /// whether any output was nonzero. `exact` also compares NaN payloads.
    fn compare(
        c: &mut Convp,
        h: &mut reference::History,
        frames: &[Vec<f32>],
        exact: bool,
    ) -> bool {
        let mut nonzero = false;
        for (n, c0) in frames.iter().enumerate() {
            c.step(c0);
            h.step(&c.w, c0);
            for g in 0..2 {
                if exact {
                    assert_eq!(bits(c.out(g)), bits(h.out(g)), "frame {n} group {g}");
                } else {
                    assert_same(c.out(g), h.out(g), &format!("frame {n} group {g}"));
                }
                nonzero |= c.out(g).iter().any(|&v| v != 0.);
            }
        }
        nonzero
    }

    /// The model's shape and every awkward one: one to six taps, outputs
    /// that fill blocks of four or leave one to three over, 16-bin blocks
    /// with and without the 8-bin tail.
    #[test]
    fn running_sums_match_the_full_convolution_bit_for_bit() {
        let mut rng = Rng(0x0dd_ba11_5eed_f00d);
        let shapes = [
            (5, 32, 5, 64),
            (5, 32, 5, 24),
            (1, 1, 1, 8),
            (1, 3, 2, 8),
            (2, 4, 3, 16),
            (3, 5, 4, 40),
            (4, 2, 6, 16),
            (6, 7, 5, 8),
            (7, 3, 2, 24),
            (8, 4, 1, 16),
            (9, 2, 3, 32),
        ];
        // Under each float mode too: flush-to-zero changes what the sums
        // of subnormals are, in both forms alike.
        each_float_mode(|mode| {
            for (half, gin, kt, f) in shapes {
                let what = format!("{mode}: half {half} gin {gin} kt {kt} f {f}");
                let w = rng.values(0, 2 * half * gin * kt);
                let mut c = Convp::new(&w, half, gin, kt, f);
                let mut h = reference::History::new(half, gin, kt, f);
                for class in 0..CLASSES {
                    // Inputs without infinities or NaNs must agree in every
                    // bit; with them, NaN payloads are left to the compiler's
                    // operand order (see `glue`).
                    let exact = class < CLASSES - 1;
                    let frames: Vec<Vec<f32>> = (0..3 * kt + 7)
                        .map(|n| match n % 11 {
                            // Silence, and negative zeros.
                            4 => vec![0.; 2 * gin * f],
                            9 => vec![-0.; 2 * gin * f],
                            _ => rng.values(class, 2 * gin * f),
                        })
                        .collect();
                    let nonzero = compare(&mut c, &mut h, &frames, exact);
                    assert!(nonzero, "{what}: the outputs are not trivially zero");
                    // A reset part way, with outputs still pending.
                    c.reset();
                    h.reset();
                    compare(&mut c, &mut h, &frames[..kt + 2], exact);
                    c.reset();
                    h.reset();
                }
            }
        });
    }

    /// The mix after the convolution, against its original form on the
    /// original padded layout.
    #[test]
    fn mix_matches_the_original_bit_for_bit() {
        let mut rng = Rng(0xabad_1dea_0bad_c0de);
        for (half, gin, kt, f) in [(5, 32, 5, 64), (1, 2, 2, 8), (3, 4, 3, 24), (4, 4, 1, 16)] {
            let w = rng.values(0, 2 * half * gin * kt);
            let mut c = Convp::new(&w, half, gin, kt, f);
            let mut h = reference::History::new(half, gin, kt, f);
            let out = 2 * half;
            for class in 0..CLASSES {
                let pw = rng.values(class.min(3), out * out);
                for _ in 0..kt + 1 {
                    let c0 = rng.values(class, 2 * gin * f);
                    c.step(&c0);
                    h.step(&c.w, &c0);
                    let mut want = rng.values(0, out * f);
                    let mut got = want.clone();
                    h.mix(&pw, &mut want);
                    c.mix(&pw, &mut got);
                    assert_same(&got, &want, &format!("half {half} f {f} class {class}"));
                }
            }
        }
    }

    /// With the reference switch on, `step` runs the original form. Both
    /// forms give the same sums, so the sums cannot show which one ran; the
    /// traffic accounting can. The running sums are stored as state, and the
    /// original form stores only its activations. Without this the
    /// whole-model comparison in `tests.rs` could compare the running sums
    /// with themselves and never fail.
    #[test]
    fn the_reference_switch_selects_the_original_form() {
        use crate::traffic::{self, Class};
        struct Off;
        impl Drop for Off {
            fn drop(&mut self) {
                crate::reference::set(false);
                traffic::disable();
            }
        }
        let _off = Off;
        let mut rng = Rng(11);
        let (half, gin, kt, f) = (5, 32, 5, 64);
        let w = rng.values(0, 2 * half * gin * kt);
        let c0 = rng.values(0, 2 * gin * f);
        let mut c = Convp::new(&w, half, gin, kt, f);
        let mut state_stores = |on: bool| {
            traffic::enable();
            crate::reference::set(on);
            c.step(&c0);
            crate::reference::set(false);
            let stores = traffic::trace()
                .iter()
                .filter(|a| a.write && a.key.0 == Class::State)
                .count();
            traffic::disable();
            stores
        };
        assert_eq!(state_stores(true), 0, "the original form");
        // Per group: one store of sums per tap, and the ring slot.
        assert_eq!(state_stores(false), 2 * (kt + 1), "the running sums");
    }

    /// `step` on frames of c0 that each end at an inaccessible page, and
    /// again that each begin at one (`Fenced`): no load leaves the frame,
    /// and the sums are still the full convolution's.
    #[cfg(all(target_os = "linux", target_pointer_width = "64"))]
    #[test]
    fn step_reads_nothing_outside_its_frame() {
        use crate::glue::tests::Fenced;
        let mut rng = Rng(0xfe1c_ed00_c0de_0003);
        for at_end in [true, false] {
            for (half, gin, kt, f) in [(5, 32, 5, 64), (3, 5, 4, 40), (1, 1, 1, 8), (6, 7, 2, 24)] {
                let w = rng.values(0, 2 * half * gin * kt);
                let mut c = Convp::new(&w, half, gin, kt, f);
                let mut h = reference::History::new(half, gin, kt, f);
                for n in 0..2 * kt + 3 {
                    let c0 = rng.values(1, 2 * gin * f);
                    c.step(&Fenced::new(&c0, at_end));
                    h.step(&c.w, &c0);
                    for g in 0..2 {
                        assert_eq!(bits(c.out(g)), bits(h.out(g)), "frame {n} group {g}");
                    }
                }
            }
        }
    }

    /// Outputs after a reset are those of a new instance.
    #[test]
    fn reset_forgets_pending_sums_and_history() {
        let mut rng = Rng(42);
        let (half, gin, kt, f) = (5, 32, 5, 64);
        let w = rng.values(0, 2 * half * gin * kt);
        let frames: Vec<Vec<f32>> = (0..12).map(|_| rng.values(0, 2 * gin * f)).collect();
        let mut used = Convp::new(&w, half, gin, kt, f);
        for c0 in &frames[..7] {
            used.step(c0);
        }
        used.reset();
        let mut new = Convp::new(&w, half, gin, kt, f);
        for c0 in &frames {
            used.step(c0);
            new.step(c0);
            for g in 0..2 {
                assert_eq!(bits(used.out(g)), bits(new.out(g)));
            }
        }
    }
}
