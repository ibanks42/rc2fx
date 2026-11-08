//! Runs the embedded model over a 48 kHz 16-bit mono WAV, writing the enhanced
//! audio aligned to the input, for listening tests of engine changes:
//!
//!     cargo run --release -p rc2fx-dfn --example enhance -- in.wav out.wav
use rc2fx_dfn::DeepFilter;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, input, output] = args.as_slice() else {
        eprintln!("usage: enhance IN.wav OUT.wav (48 kHz, 16-bit, mono)");
        std::process::exit(2);
    };
    let bytes = std::fs::read(input).expect("read input");
    let (rate, channels, bits, data) = parse_wav(&bytes);
    assert!(
        rate == 48_000 && channels == 1 && bits == 16,
        "need 48 kHz 16-bit mono, got {rate} Hz {channels} ch {bits} bit"
    );
    let samples: Vec<f32> = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.)
        .collect();
    let mut filter = DeepFilter::new().expect("embedded model");
    let frame = filter.frame();
    let delay = filter.delay();
    let mut out = Vec::with_capacity(samples.len() + delay + frame);
    let mut buf = vec![0f32; frame];
    for chunk in samples.chunks(frame) {
        buf.fill(0.);
        buf[..chunk.len()].copy_from_slice(chunk);
        filter.process(&mut buf);
        out.extend_from_slice(&buf);
    }
    for _ in 0..delay.div_ceil(frame) {
        buf.fill(0.);
        filter.process(&mut buf);
        out.extend_from_slice(&buf);
    }
    let aligned = &out[delay..delay + samples.len()];
    let mut wav = Vec::with_capacity(44 + aligned.len() * 2);
    let size = (aligned.len() * 2) as u32;
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + size).to_le_bytes());
    wav.extend_from_slice(b"WAVEfmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes());
    wav.extend_from_slice(&48_000u32.to_le_bytes());
    wav.extend_from_slice(&96_000u32.to_le_bytes());
    wav.extend_from_slice(&2u16.to_le_bytes());
    wav.extend_from_slice(&16u16.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&size.to_le_bytes());
    for &s in aligned {
        wav.extend_from_slice(&((s.clamp(-1., 1.) * 32767.).round() as i16).to_le_bytes());
    }
    std::fs::write(output, wav).expect("write output");
    eprintln!("{output}: {} samples, delay {delay}", aligned.len());
}

/// (sample rate, channels, bits per sample, PCM data) from a canonical WAV.
fn parse_wav(b: &[u8]) -> (u32, u16, u16, &[u8]) {
    assert!(&b[..4] == b"RIFF" && &b[8..12] == b"WAVE", "not a WAV file");
    let u16_at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
    let u32_at = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    let (mut o, mut fmt, mut data) = (12, None, None);
    while o + 8 <= b.len() {
        let id = &b[o..o + 4];
        let len = u32_at(o + 4) as usize;
        let body = o + 8;
        match id {
            b"fmt " => fmt = Some((u16_at(body + 2), u32_at(body + 4), u16_at(body + 14))),
            b"data" => data = Some(&b[body..(body + len).min(b.len())]),
            _ => {}
        }
        o = body + len + (len & 1);
    }
    let (channels, rate, bits) = fmt.expect("fmt chunk");
    (rate, channels, bits, data.expect("data chunk"))
}
