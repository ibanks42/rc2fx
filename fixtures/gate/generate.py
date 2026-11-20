#!/usr/bin/env python3
"""Writes fixtures/gate/shapes.json: GateShape::for_strength at a spread of
strengths, each field as its IEEE 754 bits in hex, so the Rust crate
(crates/rc2fx-core/tests/gate_fixtures.rs) and the desktop's Go
(desktop/core/gate_fixtures_test.go) are held to the same values bit for bit.

Python's math.pow and math.log2 are glibc's, as Rust's powf and log2 are on
Linux. Run it from anywhere; it rewrites the file next to it.
"""
import json
import math
import os
import struct

HYSTERESIS = 0.15


def bits(x):
    return "%016x" % struct.unpack("<Q", struct.pack("<d", x))[0]


def shape(strength):
    s = min(max(strength, 0.0), 1.0)
    linear = lambda gentle, strict: gentle + (strict - gentle) * s
    geometric = lambda gentle, strict: gentle * math.pow(strict / gentle, s)
    threshold = linear(0.3, 0.7)
    return {
        "threshold": threshold,
        "closeThreshold": max(threshold - HYSTERESIS, 0.0),
        "reductionDb": 30.0 * math.pow(s, math.log2(1.5)),
        "attackMs": 5.0,
        "holdMs": linear(350.0, 150.0),
        "releaseMs": geometric(300.0, 90.0),
    }


strengths = [i / 20 for i in range(21)] + [1 / 3, 2 / 3, 0.123456789, -0.5, 1.5]
cases = [
    {"strength": bits(s), **{k: bits(v) for k, v in shape(s).items()}}
    for s in strengths
]
path = os.path.join(os.path.dirname(os.path.abspath(__file__)), "shapes.json")
with open(path, "w") as f:
    json.dump(cases, f, indent=1)
    f.write("\n")
