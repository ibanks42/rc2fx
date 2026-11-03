from __future__ import annotations

import argparse
import json
from pathlib import Path
import struct

from capstone.arm64 import ARM64_OP_MEM
from unicorn import Uc, UC_ARCH_ARM64, UC_MODE_ARM, UC_HOOK_CODE
from unicorn import arm64_const as regs

from .canonical import canonical
from .elf import Image
from .__main__ import checked_image, ROOT


class Machine:
    def __init__(self, image):
        self.image = image
        self.cpu = Uc(UC_ARCH_ARM64, UC_MODE_ARM)
        end = max(p["p_vaddr"] + p["p_memsz"] for p in image.loads)
        self.cpu.mem_map(0, (end + 4095) & ~4095)
        for segment in image.loads:
            self.cpu.mem_write(segment["p_vaddr"], image.data[
                segment["p_offset"]:segment["p_offset"] + segment["p_filesz"]])
        self.next_heap, self.stack, self.stop = 0x10000000, 0x20010000, 0x30000000
        self.cpu.mem_map(self.next_heap, 0x200000)
        self.cpu.mem_map(self.stack - 0x10000, 0x20000)
        self.cpu.mem_map(self.stop, 0x10000)
        self.stubs = {}
        self.cpu.hook_add(UC_HOOK_CODE, self.instruction)

    def register(self, index, value=None):
        register = getattr(regs, f"UC_ARM64_REG_X{index}")
        if value is None:
            return self.cpu.reg_read(register)
        self.cpu.reg_write(register, value)

    def allocate(self, size):
        address = self.next_heap
        self.next_heap += (size + 15) & ~15
        assert self.next_heap <= 0x10200000
        self.cpu.mem_write(address, bytes(size))
        return address

    def words(self, address, *values):
        self.cpu.mem_write(address, struct.pack("<" + "Q" * len(values), *values))

    def word(self, address):
        return struct.unpack("<Q", self.cpu.mem_read(address, 8))[0]

    def string(self, address):
        data = bytearray()
        while len(data) < 4096:
            byte = bytes(self.cpu.mem_read(address + len(data), 1))
            if byte == b"\0":
                return data.decode()
            data.extend(byte)
        raise ValueError("unterminated fixture string")

    def literal(self, text):
        data = text.encode() + b"\0"
        address = self.allocate(len(data))
        self.cpu.mem_write(address, data)
        return address

    def instruction(self, cpu, address, size, _):
        if address in self.stubs:
            result = self.stubs[address]()
            self.register(0, 0 if result is None else result)
            cpu.reg_write(regs.UC_ARM64_REG_PC, self.register(30))
        elif int.from_bytes(cpu.mem_read(address, 4), "little") in (
                0xD503233F, 0xD50323BF, 0xD503245F, 0xD503249F, 0xD50324DF):
            cpu.reg_write(regs.UC_ARM64_REG_PC, address + 4)

    def run(self, address, end=None):
        end = self.stop if end is None else end
        self.cpu.reg_write(regs.UC_ARM64_REG_SP, self.stack)
        self.register(30, self.stop)
        self.cpu.emu_start(address, end, count=50000)
        assert self.cpu.reg_read(regs.UC_ARM64_REG_PC) == end, "fixture did not finish"


def counterpart(baseline, candidate, address):
    signature = canonical(baseline, address)["sha256"]
    matches = [start for start, size in candidate.functions.items()
               if size == baseline.functions[address]
               and canonical(candidate, start)["sha256"] == signature]
    if len(matches) != 1:
        raise ValueError(f"no unique relocated counterpart for {address:#x}: {matches}")
    return matches[0]


def call_at(image, address):
    instruction = next(image.disassembler.disasm(image.code(address, 4), address))
    assert instruction.mnemonic == "bl"
    return instruction.operands[0].imm


def named_usb_connections(image, port_events):
    m = Machine(image)
    # Execute the complete vendor port handler, stubbing only JUCE strings,
    # deallocation, and the virtual JACK connection operation.
    starts, compare = call_at(image, port_events + 0x1C), call_at(image, port_events + 0x3C)
    construct, to_string = call_at(image, port_events + 0x68), call_at(image, port_events + 0x74)
    destroy = call_at(image, port_events + 0x9C)
    m.stubs[starts] = lambda: int(m.string(m.word(m.register(0))).startswith(m.string(m.register(1))))
    m.stubs[compare] = lambda: 0 if m.string(m.word(m.register(0))) == m.string(m.register(1)) else 1
    m.stubs[construct] = lambda: m.words(m.register(0), m.register(1))
    m.stubs[to_string] = lambda: m.words(m.register(8), m.word(m.register(0)))
    m.stubs[destroy] = lambda: 0
    for address, symbol in image.plt.items():
        if symbol in ("_ZdlPvm", "_ZdlPv", "free"):
            m.stubs[address] = lambda: 0
    connections = []
    connect = m.stop + 0x100
    m.stubs[connect] = lambda: connections.append([
        m.string(m.word(m.register(1))), m.string(m.word(m.register(2)))])
    closure, callback, manager = m.allocate(24), m.allocate(0x30), m.allocate(0xE0)
    device, vtable = m.allocate(8), m.allocate(0xE0)
    m.words(closure + 16, callback)
    m.words(callback + 0x18, manager)
    m.words(manager + 0xD8, device)
    m.words(device, vtable)
    m.words(vtable + 0xC8, connect)
    expected = []
    for client, first in (("usb1out", 15), ("usb1-chat-out", 17), ("usb2out", 19)):
        for channel in (1, 2):
            name = f"{client}:playback_{channel}"
            m.words(closure, m.literal(name), m.literal(name))
            m.register(0, closure)
            m.run(port_events)
            expected.append([f"JUCEJack:out_{first + channel - 1}", name])
    assert connections == expected, connections
    return connections


def monitor_fields(image, board):
    # These two stores identify the independently initialized physical-output
    # count / monitor-left index and the board-specific right-index override.
    text = image.elf.get_section_by_name(".text")
    stores, overrides = [], []
    for address, _, operation, operands in image.disassembler.disasm_lite(text.data(), text["sh_addr"]):
        if operation == "str" and operands == "d1, [x26, #0x7d38]":
            stores.append(address)
        if operation == "bl" and operands == f"#{board:#x}":
            sequence = list(image.disassembler.disasm(image.code(address, 28), address))
            if sequence[-1].mnemonic == "str" and sequence[-1].op_str == "w0, [x2, #0x3d40]":
                overrides.append(sequence)
    assert len(stores) == 1 and len(overrides) == 1
    store = stores[0]
    preceding = list(image.disassembler.disasm(image.code(store - 160, 160), store - 160))
    loads = [i for i, instruction in enumerate(preceding)
             if instruction.mnemonic == "ldr" and instruction.op_str.startswith("d1, [x0,")]
    assert len(loads) == 1
    load = loads[0]
    page, instruction = preceding[load - 1], preceding[load]
    assert page.mnemonic == "adrp" and page.op_str.startswith("x0,")
    address = page.operands[1].imm + instruction.operands[1].mem.disp
    defaults = struct.unpack("<II", image.code(address, 8))
    assert defaults == (10, 4), defaults
    sequence = overrides[0]
    assert sequence[1].mnemonic == "cbz" and sequence[1].operands[0].reg == regs_cap(image, "w0")
    assert sequence[2].mnemonic == "ldr" and sequence[2].operands[1].type == ARM64_OP_MEM
    assert sequence[3].op_str == "w1, #8" and sequence[4].op_str == "w0, #5"
    assert sequence[5].op_str == "w1, [x2, #0x3d38]"
    values = {}
    for model in (0, 1):
        m = Machine(image)
        mixer = m.allocate(0xC8000)
        m.register(26, mixer + 0xC0000)
        m.run(page.address, instruction.address + 4)
        m.run(store, store + 4)
        m.cpu.mem_write(mixer + 0xC7D40, struct.pack("<I", 7))
        m.register(29, m.stack)
        m.words(m.stack + sequence[2].operands[1].mem.disp, mixer + 0xC4000)
        m.stubs[board] = lambda: model
        m.run(sequence[0].address, sequence[-1].address + 4)
        values[model] = struct.unpack("<III", m.cpu.mem_read(mixer + 0xC7D38, 12))
    assert values == {0: (10, 4, 7), 1: (8, 4, 5)}, values
    return {"defaultStore": hex(store),
            "duoOverride": hex(sequence[0].address),
            "proIi": values[0], "duo": values[1]}


def regs_cap(image, name):
    # Register IDs belong to Capstone, not Unicorn.
    from capstone import arm64
    return getattr(arm64, "ARM64_REG_" + name.upper())


def monitor_write(image, body):
    m = Machine(image)
    mixer, table, reference = m.allocate(0xC8000), m.allocate(8 * 8), m.allocate(8)
    frame_count, capture = m.allocate(4), m.allocate(32)
    m.cpu.mem_write(frame_count, struct.pack("<I", 4))
    outputs = [m.allocate(16) for _ in range(8)]
    for index, output in enumerate(outputs):
        m.cpu.mem_write(output, bytes([index + 10]) * 16)
    m.words(table, *outputs)
    m.words(reference, table)
    m.words(capture, mixer, reference, frame_count, 0)
    m.cpu.mem_write(mixer + 0xC7D38, struct.pack("<III", 8, 4, 5))
    source_table = m.allocate(16)
    left, right = m.allocate(16), m.allocate(16)
    m.cpu.mem_write(left, struct.pack("<4f", 0.1, 0.2, 0.3, 0.4))
    m.cpu.mem_write(right, struct.pack("<4f", -0.1, -0.2, -0.3, -0.4))
    m.words(source_table, left, right)
    m.words(mixer + 0xC7050 + 0x10, source_table)
    constructor = call_at(image, body + 0x2A4)
    m.stubs[constructor] = lambda: m.words(m.register(0) + 0x10, m.register(1))

    def memcpy():
        destination, source, size = (m.register(i) for i in range(3))
        m.cpu.mem_write(destination, bytes(m.cpu.mem_read(source, size)))
        return destination

    for address, name in image.plt.items():
        if name == "memcpy":
            m.stubs[address] = memcpy
    m.register(19, capture)
    m.run(body + 0x270, body + 0x2D4)
    assert m.cpu.mem_read(outputs[4], 16) == m.cpu.mem_read(left, 16)
    assert m.cpu.mem_read(outputs[5], 16) == m.cpu.mem_read(right, 16)
    for index in (0, 1, 2, 3, 6, 7):
        assert bytes(m.cpu.mem_read(outputs[index], 16)) == bytes([index + 10]) * 16
    return {"start": hex(body + 0x270), "packedOutputIndices": [4, 5],
            "samplesCopiedToExpectedBuffers": True, "headphoneAndHeadsetBuffersUntouched": True}


def source_table(image, source_change):
    table = dict(canonical(image, source_change)["globals"])[8]
    initializers = []
    for _, reference in image.address_references(table):
        code = list(image.disassembler.disasm(image.code(reference, 0x300), reference))
        starts = [i for i, instruction in enumerate(code)
                  if instruction.mnemonic == "mov" and instruction.op_str == "x2, #0x174"]
        if starts:
            assert len(starts) == 1
            start = starts[0]
            ends = [instruction.address for instruction in code[start:]
                    if instruction.mnemonic == "cbz" and instruction.op_str.startswith("x21,")]
            assert ends
            initializers.append((code[0], code[start].address, ends[0]))
    assert len(initializers) == 1
    global_reference, start, end = initializers[0]
    m = Machine(image)
    table_register = image.disassembler.reg_name(global_reference.operands[0].reg)
    m.register(int(table_register[1:]), table)
    m.register(20, m.stack + 0xA0)
    m.register(24, table + 8)

    def memset():
        address, byte, size = (m.register(i) for i in range(3))
        m.cpu.mem_write(address, bytes([byte & 255]) * size)
        return address

    for address, name in image.plt.items():
        if name == "memset":
            m.stubs[address] = memset
    m.run(start, end)
    rows = list(struct.iter_unpack("<iii", m.cpu.mem_read(m.stack + 0xA0, 0x174)))
    expected = [(0, 0, -1), (1, 1, -1), (2, 2, -1), (3, 3, -1),
                (4, 0, 1), (5, 1, 2), (6, 2, 3), (7, 4, 5), (8, 6, 7),
                (9, 8, 9), (10, 10, 11), (11, 12, 13), (12, 22, 23),
                (13, 24, 25), (14, 26, 27), (15, 28, 29), (16, 14, 15),
                (17, 16, 17), (18, 18, 19), (19, 20, -1), (20, 21, -1),
                (21, 4, 5), (22, 22, 23), (23, 24, 25), (24, 26, 27),
                (25, 28, 29), (26, 30, 31), (27, 32, 33), (28, 34, 35),
                (29, 36, 37), (-1, -1, -1)]
    assert rows == expected, rows
    return {"initializer": hex(start), "lookupFunction": hex(source_change),
            "table": hex(table), "sourceIdAndInternalSlots": rows,
            "note": "Internal source slots are not JACK port indices. Unexposed enum values do not grant independent processing paths."}


def audit(baseline_path, candidate_path, catalog):
    if not __debug__:
        raise ValueError("audit assertions must be enabled")
    profiles = {p["id"]: p for p in catalog["profiles"]}
    baseline, old_sites = checked_image(baseline_path, profiles["rodecaster-duo-1.7.6"])
    candidate, new_sites = checked_image(candidate_path, profiles["rodecaster-duo-1.7.7"])
    roles = {"board": 0x2572F0, "portEvents": 0x3716E0,
             "jackActiveMask": 0x61CC90, "callbackBridge": 0x370A70,
             "sourceChange": 0x40A360}
    mapped = {name: counterpart(baseline, candidate, address) for name, address in roles.items()}
    results = {}
    for version, image, locations, sites in (("1.7.6", baseline, roles, old_sites),
                                            ("1.7.7", candidate, mapped, new_sites)):
        results[version] = {
            "archiveSha256": image.archive_sha256, "mixerSha256": image.sha256,
            "locations": {name: hex(value) for name, value in locations.items()},
            "usbConnections": named_usb_connections(image, locations["portEvents"]),
            "monitorFields": monitor_fields(image, locations["board"]),
            "monitorWrite": monitor_write(image, sites["body"]),
            "sourceTable": source_table(image, locations["sourceChange"]),
        }
    return {"schemaVersion": 1, "firmware": results,
            "limitations": [
                "Offline ARM64 fixtures stub JUCE strings, allocations and JACK connections; PAC/BTI hints skipped.",
                "Monitor fixture uses the Duo's eight active hardware buffers. Port numbers and packed indices are not interchangeable when connections change.",
                "The native pipeline processes stereo outputs only when BOTH named ports are observed in the vendor callback; it never substitutes another packed index.",
                "The shared legacy startup callback also contains Pro II balanced-monitor rewiring. This audit does not claim live Duo connection/lifecycle validation.",
                "No device has been flashed or used for audio, timing, recovery or USB tests.",
            ]}


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--baseline", required=True, type=Path)
    parser.add_argument("--candidate", required=True, type=Path)
    parser.add_argument("--catalog", type=Path, default=ROOT / "config/firmware-profiles.json")
    parser.add_argument("--output", required=True, type=Path)
    args = parser.parse_args()
    report = audit(args.baseline, args.candidate, json.loads(args.catalog.read_text()))
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(f"PASS: both Duo images have matching USB sends, monitor buffer writes and source enum tables. {args.output}")


if __name__ == "__main__":
    main()
