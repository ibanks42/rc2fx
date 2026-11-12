//! The feasibility probe that preceded the library's tiled int8 kernel
//! (`kernels::matvec_q8`, 4-row x 16-column tiles). Standalone: it carries
//! its own copies of the row-major kernel, of a four-rows-at-a-time kernel
//! over row-major weights (the removed `dfn_q8_rows4` kernel, whose reason
//! for removal is not recorded) and of a first packed-tile kernel. The
//! library's kernel is a later form of `packed`, and `rc2fx-bench --kernel
//! compare` times it against the row-major one through the whole model;
//! this probe remains for timing the three layouts on one resident matrix.
//!
//!     cargo run --release -p rc2fx-dfn --example packing_bench -- [ITERATIONS]
//!
//! On AArch64 (or under QEMU) it checks the three NEON kernels bit for bit
//! against a scalar reference with wrapping i32 sums, over several shapes
//! (including row tails and 1024 columns with i16 extremes, where the sums
//! wrap), then times them. Elsewhere it checks packing and the scalar
//! packed reference only. Timings from x86 or QEMU say nothing about the
//! mixer's Cortex-A53.
#![cfg_attr(not(target_arch = "aarch64"), allow(dead_code))]

/// Row-major int8 rows to 4-row x 16-column tiles: for each group of four
/// rows, for each 16 columns, the four rows' 16 bytes in row order (64
/// bytes). Rows past the last complete group follow row-major.
fn pack(q: &[i8], rows: usize, cols: usize) -> Vec<i8> {
    assert!(cols.is_multiple_of(16) && q.len() == rows * cols);
    let mut out = Vec::with_capacity(q.len());
    let groups = rows / 4;
    for g in 0..groups {
        for i in (0..cols).step_by(16) {
            for r in 0..4 {
                let at = (4 * g + r) * cols + i;
                out.extend_from_slice(&q[at..at + 16]);
            }
        }
    }
    out.extend_from_slice(&q[groups * 4 * cols..]);
    assert_eq!(out.len(), q.len());
    out
}

/// The library's scalar arithmetic, with explicitly wrapping i32 sums.
fn scalar(q: &[i8], scale: &[f32], rows: usize, cols: usize, xq: &[i16], xs: f32, y: &mut [f32]) {
    for r in 0..rows {
        let mut acc = 0i32;
        for i in 0..cols {
            acc = acc.wrapping_add((q[r * cols + i] as i32).wrapping_mul(xq[i] as i32));
        }
        y[r] = acc as f32 * (scale[r] * xs);
    }
}

/// The scalar packed reference: the same sums read from tiles.
fn scalar_packed(
    p: &[i8],
    scale: &[f32],
    rows: usize,
    cols: usize,
    xq: &[i16],
    xs: f32,
    y: &mut [f32],
) {
    let groups = rows / 4;
    for g in 0..groups {
        let mut acc = [0i32; 4];
        for (t, i) in (0..cols).step_by(16).enumerate() {
            let tile = &p[(g * cols / 16 + t) * 64..][..64];
            for (r, acc) in acc.iter_mut().enumerate() {
                for j in 0..16 {
                    *acc =
                        acc.wrapping_add((tile[r * 16 + j] as i32).wrapping_mul(xq[i + j] as i32));
                }
            }
        }
        for (r, acc) in acc.into_iter().enumerate() {
            y[4 * g + r] = acc as f32 * (scale[4 * g + r] * xs);
        }
    }
    let tail = groups * 4;
    scalar(
        &p[tail * cols..],
        &scale[tail..],
        rows - tail,
        cols,
        xq,
        xs,
        &mut y[tail..],
    );
}

#[cfg(target_arch = "aarch64")]
mod neon {
    use std::arch::aarch64::*;

    /// `matvec_q8_steps::<1>`'s original row loop.
    #[inline(never)]
    pub fn baseline(
        q: &[i8],
        scale: &[f32],
        rows: usize,
        cols: usize,
        xq: &[i16],
        xs: f32,
        y: &mut [f32],
    ) {
        assert!(
            q.len() >= rows * cols && xq.len() >= cols && y.len() >= rows && scale.len() >= rows
        );
        unsafe {
            for r in 0..rows {
                let row = q.as_ptr().add(r * cols);
                let mut acc = [vdupq_n_s32(0); 2];
                let mut i = 0;
                while i < cols {
                    let wv = vld1q_s8(row.add(i));
                    let w01 = vmovl_s8(vget_low_s8(wv));
                    let w23 = vmovl_high_s8(wv);
                    let xp = xq.as_ptr().add(i);
                    let x01 = vld1q_s16(xp);
                    let x23 = vld1q_s16(xp.add(8));
                    acc[0] = vmlal_s16(acc[0], vget_low_s16(w01), vget_low_s16(x01));
                    acc[1] = vmlal_high_s16(acc[1], w01, x01);
                    acc[0] = vmlal_s16(acc[0], vget_low_s16(w23), vget_low_s16(x23));
                    acc[1] = vmlal_high_s16(acc[1], w23, x23);
                    i += 16;
                }
                let sum = vaddvq_s32(vaddq_s32(acc[0], acc[1]));
                *y.get_unchecked_mut(r) = sum as f32 * (*scale.get_unchecked(r) * xs);
            }
        }
    }

    /// `matvec_q8_rows4` over complete groups, the row loop for the rest.
    #[inline(never)]
    pub fn rows4(
        q: &[i8],
        scale: &[f32],
        rows: usize,
        cols: usize,
        xq: &[i16],
        xs: f32,
        y: &mut [f32],
    ) {
        assert!(
            q.len() >= rows * cols && xq.len() >= cols && y.len() >= rows && scale.len() >= rows
        );
        let end = rows / 4 * 4;
        unsafe {
            for row in (0..end).step_by(4) {
                let base = q.as_ptr().add(row * cols);
                let mut acc = [[vdupq_n_s32(0); 2]; 4];
                for i in (0..cols).step_by(16) {
                    let x01 = vld1q_s16(xq.as_ptr().add(i));
                    let x23 = vld1q_s16(xq.as_ptr().add(i + 8));
                    for (r, acc) in acc.iter_mut().enumerate() {
                        let wv = vld1q_s8(base.add(r * cols + i));
                        let w01 = vmovl_s8(vget_low_s8(wv));
                        let w23 = vmovl_high_s8(wv);
                        acc[0] = vmlal_s16(acc[0], vget_low_s16(w01), vget_low_s16(x01));
                        acc[1] = vmlal_high_s16(acc[1], w01, x01);
                        acc[0] = vmlal_s16(acc[0], vget_low_s16(w23), vget_low_s16(x23));
                        acc[1] = vmlal_high_s16(acc[1], w23, x23);
                    }
                }
                for (r, [a, b]) in acc.into_iter().enumerate() {
                    let sum = vaddvq_s32(vaddq_s32(a, b));
                    *y.get_unchecked_mut(row + r) =
                        sum as f32 * (*scale.get_unchecked(row + r) * xs);
                }
            }
        }
        baseline(
            &q[end * cols..],
            &scale[end..],
            rows - end,
            cols,
            xq,
            xs,
            &mut y[end..],
        );
    }

    /// The same products as `rows4`, reading each 4 x 16 tile as one
    /// contiguous 64-byte block through a single advancing pointer.
    #[inline(never)]
    pub fn packed(
        p: &[i8],
        scale: &[f32],
        rows: usize,
        cols: usize,
        xq: &[i16],
        xs: f32,
        y: &mut [f32],
    ) {
        assert!(
            p.len() >= rows * cols && xq.len() >= cols && y.len() >= rows && scale.len() >= rows
        );
        let end = rows / 4 * 4;
        unsafe {
            let mut tile = p.as_ptr();
            for row in (0..end).step_by(4) {
                let mut acc = [[vdupq_n_s32(0); 2]; 4];
                for i in (0..cols).step_by(16) {
                    let x01 = vld1q_s16(xq.as_ptr().add(i));
                    let x23 = vld1q_s16(xq.as_ptr().add(i + 8));
                    let w = vld1q_s8_x4(tile);
                    tile = tile.add(64);
                    for (acc, wv) in acc.iter_mut().zip([w.0, w.1, w.2, w.3]) {
                        let w01 = vmovl_s8(vget_low_s8(wv));
                        let w23 = vmovl_high_s8(wv);
                        acc[0] = vmlal_s16(acc[0], vget_low_s16(w01), vget_low_s16(x01));
                        acc[1] = vmlal_high_s16(acc[1], w01, x01);
                        acc[0] = vmlal_s16(acc[0], vget_low_s16(w23), vget_low_s16(x23));
                        acc[1] = vmlal_high_s16(acc[1], w23, x23);
                    }
                }
                for (r, [a, b]) in acc.into_iter().enumerate() {
                    let sum = vaddvq_s32(vaddq_s32(a, b));
                    *y.get_unchecked_mut(row + r) =
                        sum as f32 * (*scale.get_unchecked(row + r) * xs);
                }
            }
        }
        baseline(
            &p[end * cols..],
            &scale[end..],
            rows - end,
            cols,
            xq,
            xs,
            &mut y[end..],
        );
    }
}

struct Lcg(u32);
impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        self.0 >> 8
    }
}

fn main() {
    #[cfg_attr(not(target_arch = "aarch64"), allow(unused_variables))]
    let iterations: usize = std::env::args()
        .nth(1)
        .map_or(2000, |a| a.parse().expect("ITERATIONS"));
    let mut rng = Lcg(0x2545_f491);
    let bits = |y: &[f32]| y.iter().map(|v| v.to_bits()).collect::<Vec<_>>();
    // (rows, cols, extremes): the LA0 GRU shape, row tails, and 1024 columns
    // with i16 extremes so the i32 sums wrap.
    let shapes = [
        (384, 128, false),
        (384, 128, true),
        (387, 128, false),
        (6, 16, true),
        (3, 64, false),
        (64, 1024, true),
        (65, 1024, true),
    ];
    let mut checked = 0;
    for &(rows, cols, extremes) in &shapes {
        for case in 0..8 {
            let q: Vec<i8> = (0..rows * cols)
                .map(|_| {
                    if extremes && case % 2 == 0 {
                        [-127i8, 127][(rng.next() & 1) as usize]
                    } else {
                        (rng.next() % 255) as i8
                    }
                })
                .map(|v| if v == -128 { -127 } else { v })
                .collect();
            let xq: Vec<i16> = (0..cols)
                .map(|_| {
                    if extremes {
                        [i16::MIN, i16::MAX, -2047, 2047][(rng.next() & 3) as usize]
                    } else {
                        (rng.next() % 4095) as i16 - 2047
                    }
                })
                .collect();
            let scale: Vec<f32> = (0..rows)
                .map(|_| (rng.next() % 1000 + 1) as f32 * 1e-5)
                .collect();
            let xs = (rng.next() % 1000 + 1) as f32 * 1e-4;
            let packed = pack(&q, rows, cols);
            let mut want = vec![0f32; rows];
            scalar(&q, &scale, rows, cols, &xq, xs, &mut want);
            let mut got = vec![0f32; rows];
            scalar_packed(&packed, &scale, rows, cols, &xq, xs, &mut got);
            assert_eq!(bits(&got), bits(&want), "scalar packed {rows}x{cols}");
            #[cfg(target_arch = "aarch64")]
            {
                for (name, f) in [
                    (
                        "baseline",
                        neon::baseline as fn(&[i8], &[f32], usize, usize, &[i16], f32, &mut [f32]),
                    ),
                    ("rows4", neon::rows4),
                ] {
                    got.fill(f32::NAN);
                    f(&q, &scale, rows, cols, &xq, xs, &mut got);
                    assert_eq!(bits(&got), bits(&want), "{name} {rows}x{cols}");
                }
                got.fill(f32::NAN);
                neon::packed(&packed, &scale, rows, cols, &xq, xs, &mut got);
                assert_eq!(bits(&got), bits(&want), "packed {rows}x{cols}");
            }
            checked += 1;
        }
    }
    println!(
        "{{\"arch\":\"{}\",\"cases\":{checked},\"neonChecked\":{},\"weightBytesRowMajor\":{},\"weightBytesPacked\":{}}}",
        std::env::consts::ARCH,
        cfg!(target_arch = "aarch64"),
        384 * 128,
        pack(&vec![0i8; 384 * 128], 384, 128).len()
    );
    #[cfg(target_arch = "aarch64")]
    {
        let (rows, cols) = (384, 128);
        let q: Vec<i8> = (0..rows * cols)
            .map(|_| ((rng.next() % 255) as i8).max(-127))
            .collect();
        let xq: Vec<i16> = (0..cols)
            .map(|_| (rng.next() % 4095) as i16 - 2047)
            .collect();
        let scale = vec![1e-3f32; rows];
        let packed = pack(&q, rows, cols);
        let mut y = vec![0f32; rows];
        let time = |f: &dyn Fn(&mut [f32])| {
            let mut y = vec![0f32; rows];
            let t = std::time::Instant::now();
            for _ in 0..iterations {
                f(std::hint::black_box(&mut y));
            }
            t.elapsed().as_nanos() as f64 / iterations as f64
        };
        let b = time(&|y| neon::baseline(&q, &scale, rows, cols, &xq, 1e-3, y));
        let r = time(&|y| neon::rows4(&q, &scale, rows, cols, &xq, 1e-3, y));
        let p = time(&|y| neon::packed(&packed, &scale, rows, cols, &xq, 1e-3, y));
        neon::packed(&packed, &scale, rows, cols, &xq, 1e-3, &mut y);
        println!(
            "{{\"nsPerMatvec384x128\":{{\"baseline\":{b:.0},\"rows4\":{r:.0},\"packed\":{p:.0}}},\"note\":\"L1/L2-resident repeat of one matrix; not a mixer measurement\"}}"
        );
    }
}
