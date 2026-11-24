"""Offline Chat stem oracle using Unicorn to run the pinned vendor mix functions.

Runs two machines with identical routing (one with all sources active, one with
pads and playback zeroed) to generate a mic-only reference for testing the Rust
capture path.
"""

from __future__ import annotations
import argparse
import json
from pathlib import Path
import struct
from .elf import Image
from .duo_routes import Machine, counterpart

FRAMES = 112
PINS = {
    "5151d0eb843dab46f441da4c2cca66f1636bfe522b2302345c17f28ecbffd0dd": (
        0x657470,
        0x428020,
        0x430930,
        0x430EA0,
        0x431430,
        0x4318C0,
    ),
    "897b9e25919d826ade563cad89a5acd140541b274b74524f655273488089f48c": (
        0x657250,
        0x424FE0,
        0x42D8F0,
        0x42DE60,
        0x42E3F0,
        0x42E880,
    ),
}
SOURCES = [0, 11, 8, 19, 7, 1, 10, 20, 12]
INPUTS = [
    [0.125, 0.125],
    [0.25, -0.125],
    [0.0625, 0.03125],
    [0.0625, 0.125],
    [0.03125, -0.03125],
    [0.0625, 0.0625],
    [0.0, 0.0],
    [0.0, 0.0],
    [0.0, 0.0],
]
VOICE_IDS = {0, 1, 2, 3, 4, 5, 6, 19, 20}


class Fixture:
    def __init__(self, image, recipe, voice_only=False):
        self.m = Machine(image)
        self.sites = PINS[image.sha256]
        self.recipe = recipe
        self.voice_only = voice_only
        self.buffers = {}
        for address, name in image.plt.items():
            if name == "memset":
                self.m.stubs[address] = self.memset
            if name == "memcpy":
                self.m.stubs[address] = self.memcpy
        self.root = self.m.allocate(0xC8000)
        self.stream, self.strips, self.wrappers = self.mix(stream=True)
        self.custom, self.custom_strips, _ = self.mix(stream=False)
        self.main = self.m.allocate(0x98)
        self.chat = self.m.allocate(0x508)
        for offset, value in [
            (0xC1FB0, self.stream),
            (0xC1FF8, self.custom),
            (0xC2060, self.main),
            (0xC2088, self.chat),
        ]:
            self.m.words(self.root + offset, value)
        media = [[0.125, -0.0625], [-0.03125, 0.0625]]
        for offset, values in zip((0xC72A0, 0xC74F0), media):
            self.buffer(self.root + offset, [0.0, 0.0] if voice_only else values)
        self.buffer(self.root + 0xC6CD8, [0.0, 0.0])
        self.buffer(self.root + 0xC6F28, [0.0, 0.0])
        self.buffer(self.chat + 0x2A0, [0.0, 0.0])
        self.buffer(self.chat + 0x3C8, [0.0, 0.0])
        self.output = self.m.allocate(0x128)
        self.buffer(self.output, [0.0, 0.0])
        self.m.words(self.chat + 0x278, self.root + 0xC6F28)
        self.m.words(self.chat + 0x280, self.stream + 0x190)
        self.m.words(
            self.chat + 0x298, self.strips[2] + 0x338
        )  # USB Chat return (not a microphone)
        self.m.words(self.chat + 0x290, self.strips[2] + 0x338)
        self.word(self.chat + 0x268, recipe["outputState"])
        self.floats(self.chat + 0x23C, recipe["outputGain"][0], recipe["outputGain"][1])
        self.floats(self.chat + 0x160, recipe["outputGain"][2], recipe["outputGain"][3])
        self.byte(self.chat + 0x500, 1)
        self.entries(
            self.main,
            [self.stream + 0x20, self.root + 0xC72A0, self.root + 0xC74F0],
            [[3, 1.0, 1.0, 0.125, 0.875]] * 3,
        )
        self.entries(
            self.chat,
            [
                self.root + 0xC6F28,
                self.chat + 0x2A0,
                self.chat + 0x3C8,
                self.custom + 0x20,
                self.root + 0xC74F0,
                self.root + 0xC72A0,
            ],
            recipe["gains"],
        )

    def word(self, p, v):
        self.m.cpu.mem_write(p, struct.pack("<I", v))

    def byte(self, p, v):
        self.m.cpu.mem_write(p, bytes([v]))

    def floats(self, p, *v):
        self.m.cpu.mem_write(p, struct.pack("<" + "f" * len(v), *v))

    def memset(self):
        p, v, n = [self.m.register(i) for i in range(3)]
        assert n < 0x100000
        self.m.cpu.mem_write(p, bytes([v & 255]) * n)
        return p

    def memcpy(self):
        p, q, n = [self.m.register(i) for i in range(3)]
        assert n < 0x100000
        self.m.cpu.mem_write(p, bytes(self.m.cpu.mem_read(q, n)))
        return p

    def buffer(self, p, values):
        table = self.m.allocate(16)
        data = [self.m.allocate(FRAMES * 4) for _ in range(2)]
        self.word(p, 2)
        self.word(p + 4, FRAMES)
        self.m.words(p + 16, table)
        self.m.words(table, *data)
        for addr, value in zip(data, values):
            self.floats(addr, *([value] * FRAMES))
        self.byte(p + 0x120, 0)
        self.buffers[p] = data
        return p

    def read(self, p):
        return [
            list(struct.unpack("<" + "f" * FRAMES, self.m.cpu.mem_read(a, FRAMES * 4)))
            for a in self.buffers[p]
        ]

    def vector(self, p, items):
        values = self.m.allocate(max(8, len(items) * 8))
        self.m.words(values, *items)
        self.m.words(p + 8, values, values + 8 * len(items), values + 8 * len(items))

    def mix(self, stream):
        mix = self.m.allocate(0x480 if stream else 0x220)
        strips = []
        wrappers = []
        self.buffer(mix + 0x20, [0.0, 0.0])
        if stream:
            self.buffer(mix + 0x190, [0.0, 0.0])
        for i, (source, levels) in enumerate(zip(SOURCES, INPUTS)):
            strip = self.m.allocate(0x858)
            src = self.m.allocate(0x480)
            if (
                self.voice_only
                and source not in VOICE_IDS
                or self.recipe["zeroMics"]
                and source in VOICE_IDS
            ):
                levels = [0.0, 0.0]
            self.word(src + 0xE4, source)
            self.buffer(src + 0x350, levels)
            self.m.words(strip + 0x460, src)
            self.buffer(strip + 0x210, [0.0, 0.0])
            self.buffer(strip + 0x338, [0.0, 0.0])
            muted = not stream and i == 0
            self.word(strip + 0x658, 0 if muted else 1)
            self.word(strip + 0x848, 1)
            self.floats(strip + 0x62C, 1.0 if stream else 0.5)
            self.m.words(mix + 0x148 + i * 8, strip)
            strips.append(strip)
            if stream:
                wrapper = self.m.allocate(0x350)
                wrappers.append(wrapper)
                self.m.words(mix + 0x438 + i * 8, wrapper)
                self.m.words(wrapper + 8, strip)
                self.m.words(wrapper + 0x340, strip + 0x338)
                state = (
                    2
                    if self.recipe["soloRamp"] and i == 0
                    else (1 if i in (0, 1) else 0)
                )
                self.word(wrapper + 0x338, state)
                self.floats(wrapper + 0x30C, 1.0, 0.0 if state == 2 else 1.0)
                self.floats(wrapper + 0x230, 0.125, 0.875)
        self.vector(mix, [s + 0x338 for s in strips])
        return mix, strips, wrappers

    def entries(self, obj, buffers, gains):
        entries = []
        for buffer, (state, target, current, a, b) in zip(buffers, gains):
            entry = self.m.allocate(0x1F0)
            entries.append(entry)
            self.m.words(entry + 0xD0, buffer)
            self.word(entry + 0x1E8, state)
            self.word(entry + 0xCC, 0 if target == 0.0 else 2)
            self.floats(entry + 0x1BC, target, current)
            self.floats(entry + 0xE0, a, b)
            # Suppress observer callbacks in the fixture while letting gain/state transitions run.
            self.byte(entry + 0xC0, 1)
            self.byte(entry + 0x90, 1)
        self.vector(obj, entries)

    def call(self, address, *args):
        for i, arg in enumerate(args):
            self.m.register(i, arg)
        self.m.run(address)

    def run(self):
        _, stream, custom, summation, chat, minus = self.sites
        self.call(stream, self.stream, FRAMES)
        self.call(custom, self.custom, FRAMES)
        self.call(summation, self.main, self.root + 0xC6F28)
        self.call(minus, self.chat, FRAMES)
        self.call(chat, self.chat, self.output, 0)
        return {
            "stream": self.read(self.stream + 0x20),
            "solo": self.read(self.stream + 0x190),
            "main": self.read(self.root + 0xC6F28),
            "chat": self.read(self.output),
        }

    def strip_rows(self, strips):
        result = []
        for source, s in zip(SOURCES, strips):
            pre = self.read(s + 0x210)
            post = self.read(s + 0x338)
            assert all(all(v == channel[0] for v in channel) for channel in pre + post)
            result.append(
                {
                    "source": source,
                    "pre": [c[0] for c in pre],
                    "post": [c[0] for c in post],
                }
            )
        return result


def recipes():
    off = [1, 0.0, 0.0, 0.125, 0.875]
    on = [3, 1.0, 1.0, 0.125, 0.875]
    result = []
    for name, route in [
        ("main", 0),
        ("mix-minus", 1),
        ("solo", 2),
        ("custom", 3),
        ("pads-only", 0),
        ("muted", 0),
    ]:
        gains = [off.copy() for _ in range(6)]
        gains[route] = on.copy()
        gains[4] = on.copy()
        gains[5] = on.copy()
        result.append(
            {
                "name": name,
                "gains": gains,
                "outputState": 0 if name == "muted" else 1,
                "outputGain": [0.5, 0.5, 0.125, 0.875],
                "zeroMics": name == "pads-only",
                "soloRamp": name == "solo",
            }
        )
    gains = [off.copy() for _ in range(6)]
    gains[0] = [0, 0.0, 1.0, 0.125, 0.875]
    gains[3] = [2, 1.0, 0.0, 0.125, 0.875]
    gains[4] = on.copy()
    gains[5] = on.copy()
    result.append(
        {
            "name": "crossfade",
            "gains": gains,
            "outputState": 2,
            "outputGain": [1.0, 0.5, 0.125, 0.875],
            "zeroMics": False,
            "soloRamp": False,
        }
    )
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--baseline", type=Path, required=True)
    parser.add_argument("--candidate", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    images = [Image(args.baseline), Image(args.candidate)]
    for image in images:
        assert image.sha256 in PINS
        # Verify the incoming register (w4) stored to the queued job's frame-count local at sp+0x1b4.
        mixer = PINS[image.sha256][0]
        store = next(i for i in image.instructions(mixer) if i.address == mixer + 0x1C)
        assert store.mnemonic == "str"
        assert image.disassembler.reg_name(store.operands[0].reg) == "w4"
        assert image.disassembler.reg_name(store.operands[1].mem.base) == "sp"
        assert store.operands[1].mem.disp == 0x1B4
    # Verify full relocation-normalized function equivalence, not just 16-byte prefixes.
    for old, new in zip(PINS[images[0].sha256], PINS[images[1].sha256]):
        assert counterpart(images[1], images[0], new) == old, (hex(old), hex(new))
    corpus = []
    for recipe in recipes():
        versions = []
        for image in images:
            full = Fixture(image, recipe)
            full_result = full.run()
            voice = Fixture(image, recipe, True)
            voice_result = voice.run()
            versions.append(
                {
                    "recipe": recipe,
                    "strips": full.strip_rows(full.strips),
                    "custom": full.strip_rows(full.custom_strips),
                    "reference": voice_result,
                    "fullChat": full_result["chat"],
                }
            )
        assert versions[0] == versions[1], recipe["name"]
        corpus.append(versions[0])
        print("PASS", recipe["name"])
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(
        json.dumps(
            {
                "images": [image.sha256 for image in images],
                "frames": FRAMES,
                "mixerFramesRegister": "w4",
                "cases": corpus,
            },
            separators=(",", ":"),
        )
        + "\n"
    )
    print("Verified both pinned images ->", args.output)


if __name__ == "__main__":
    main()
