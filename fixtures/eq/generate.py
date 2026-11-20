#!/usr/bin/env python3
"""Writes fixtures/eq/biquads.json: EQ design_q and biquad coefficients across
every BandType and BandMode, with each float as its IEEE 754 bits in hex, so
Rust (crates/rc2fx-core/tests/eq_fixtures.rs) and Go
(desktop/core/eq_fixtures_test.go) stay in lockstep.

Python's math.pow/sin/cos are glibc's, as Rust's f64 methods are on Linux.
Run from anywhere; it rewrites biquads.json beside it.
"""
import json
import math
import os
import struct

SAMPLE_RATE = 48_000.0


def bits(x):
    return "%016x" % struct.unpack("<Q", struct.pack("<d", x))[0]


def design_q(mode, band_type, gain, q):
    if mode == "rlc-bt" and band_type == "bell":
        g = math.pow(10.0, gain / 20.0)
        return (1.0 + 2.0 * q) * math.sqrt(g) / (2.0 * math.sqrt(1.0 + g * g))
    if mode == "rlc-bt":
        return (1.0 + q) / 2.0
    return max(q, 0.1)


def coefficients(mode, band_type, frequency, gain, q):
    if band_type == "off":
        return [1.0, 0.0, 0.0, 0.0, 0.0]
    w = 2.0 * math.pi * min(frequency, 0.49 * SAMPLE_RATE) / SAMPLE_RATE
    c = math.cos(w)
    alpha = math.sin(w) / (2.0 * design_q(mode, band_type, gain, q))
    a = math.pow(10.0, gain / 40.0)
    beta = 2.0 * math.sqrt(a) * alpha
    if band_type == "bell":
        n = [1.0 + alpha * a, -2.0 * c, 1.0 - alpha * a]
        d = [1.0 + alpha / a, -2.0 * c, 1.0 - alpha / a]
    elif band_type == "low-pass":
        n = [(1.0 - c) / 2.0, 1.0 - c, (1.0 - c) / 2.0]
        d = [1.0 + alpha, -2.0 * c, 1.0 - alpha]
    elif band_type == "high-pass":
        n = [(1.0 + c) / 2.0, -(1.0 + c), (1.0 + c) / 2.0]
        d = [1.0 + alpha, -2.0 * c, 1.0 - alpha]
    elif band_type == "notch":
        n = [1.0, -2.0 * c, 1.0]
        d = [1.0 + alpha, -2.0 * c, 1.0 - alpha]
    elif band_type == "low-shelf":
        n = [
            a * ((a + 1.0) - (a - 1.0) * c + beta),
            2.0 * a * ((a - 1.0) - (a + 1.0) * c),
            a * ((a + 1.0) - (a - 1.0) * c - beta),
        ]
        d = [
            (a + 1.0) + (a - 1.0) * c + beta,
            -2.0 * ((a - 1.0) + (a + 1.0) * c),
            (a + 1.0) + (a - 1.0) * c - beta,
        ]
    elif band_type == "high-shelf":
        n = [
            a * ((a + 1.0) + (a - 1.0) * c + beta),
            -2.0 * a * ((a - 1.0) + (a + 1.0) * c),
            a * ((a + 1.0) + (a - 1.0) * c - beta),
        ]
        d = [
            (a + 1.0) - (a - 1.0) * c + beta,
            2.0 * ((a - 1.0) - (a + 1.0) * c),
            (a + 1.0) - (a - 1.0) * c - beta,
        ]
    else:
        raise ValueError(band_type)
    return [n[0] / d[0], n[1] / d[0], n[2] / d[0], d[1] / d[0], d[2] / d[0]]


specs = [
    ("off", "rlc-bt", 1000.0, 0.0, 1.0),
    ("bell", "rlc-bt", 1000.0, 6.0, 1.41),
    ("bell", "rlc-bt", 250.0, -9.5, 4.0),
    ("bell", "apo-dr", 3200.0, 12.0, 0.1),
    ("bell", "apo-dr", 120.0, -18.0, 10.0),
    ("bell", "apo-dr", 1000.0, 0.0, 1.0),
    ("low-pass", "rlc-bt", 8000.0, 0.0, 0.707),
    ("low-pass", "apo-dr", 18000.0, 0.0, 0.5),
    ("high-pass", "rlc-bt", 80.0, 0.0, 0.707),
    ("high-pass", "apo-dr", 120.0, 0.0, 2.0),
    ("notch", "rlc-bt", 60.0, 0.0, 8.0),
    ("notch", "apo-dr", 4000.0, 0.0, 12.0),
    ("low-shelf", "rlc-bt", 105.0, 6.5, 0.7),
    ("low-shelf", "apo-dr", 46.0, 15.33, 0.705),
    ("low-shelf", "apo-dr", 200.0, -12.0, 1.2),
    ("high-shelf", "rlc-bt", 10000.0, -3.5, 0.7),
    ("high-shelf", "apo-dr", 7588.0, 2.49, 1.437),
    ("high-shelf", "apo-dr", 20000.0, 6.0, 0.707),
]

cases = []
for band_type, mode, freq, gain, q in specs:
    dq = design_q(mode, band_type, gain, q)
    coeffs = coefficients(mode, band_type, freq, gain, q)
    cases.append({
        "bandType": band_type,
        "mode": mode,
        "frequency": bits(freq),
        "gain": bits(gain),
        "q": bits(q),
        "designQ": bits(dq),
        "b0": bits(coeffs[0]),
        "b1": bits(coeffs[1]),
        "b2": bits(coeffs[2]),
        "a1": bits(coeffs[3]),
        "a2": bits(coeffs[4]),
    })

path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "biquads.json")
with open(path, "w") as f:
    json.dump(cases, f, indent=1)
    f.write("\n")
