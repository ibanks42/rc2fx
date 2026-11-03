"""Offline audit of the USB virtual paths on the shared Pro II / Duo mixer executable.

Answers, from official archives only, how source IDs 12-15 (internal slots 22-29)
and the USB sends (JUCEJack out_15/16, out_19/20) reach JACK ports, and who
writes those ports inside the audio callback:

1. Port-event handler: every named JACK connection it makes, inputs and outputs.
2. Mixer::Process input gather: the 38-entry internal slot table is built from
   the packed JACK input array under the 36-bit active-input mask. Emulated with
   several masks on both board results.
3. Mixer::Process output gather: packed JACK outputs after the hardware ones are
   assigned to fixed stack slots under the 46-bit active-output mask. Emulated.
4. Mixer::Process output dispatch: which processor member receives which output
   pair, synchronously, inside the callback. Emulated with recording vtables.
5. Static facts: the callback bridge iterates exactly 36 input / 46 output
   channels; the active masks come from jack_port_connected; the JACK device
   registers in_N / out_N sequentially; the queued job touches only the monitor
   outputs.

Run: python -m tools.firmware_audit.usb_paths --archive A.tar.gz [--archive ...] --output report.json
The output contains addresses, hashes and fixture results, never vendor code.
No device is contacted.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import struct

from .__main__ import ROOT, checked_image
from .canonical import canonical
from .duo_routes import Machine, call_at, counterpart, source_table
from unicorn import arm64_const as regs

FRAMES = 112
# Analyst roles per mixer executable hash (ELF virtual addresses). The
# relocation-normalized comparison in `cross_check` ties the two sets together.
ROLES = {
    "897b9e25919d826ade563cad89a5acd140541b274b74524f655273488089f48c": {  # 1.7.6
        "callbackBridge": 0x370A70, "portEvents": 0x3716E0,
        "sourceChange": 0x40A360, "activeMask": 0x61CC90,
    },
    "5151d0eb843dab46f441da4c2cca66f1636bfe522b2302345c17f28ecbffd0dd": {  # 1.7.7
        "callbackBridge": 0x3737A0, "portEvents": 0x374410,
        "sourceChange": 0x40CFD0, "activeMask": 0x61D900,
    },
}
# Offsets inside Mixer::Process (identical in both releases; `cross_check`
# proves the whole function relocation-equivalent).
INPUT_GATHER = (0x270, 0xB94)
OUTPUT_GATHER = (0x12D4, 0x14CC)
OUTPUT_DISPATCH = (0x1BA4, 0x1A68)
CALLS = {
    "prepareA": 0x2FC, "prepareB": 0x364, "prepareC": 0x378, "board": 0x448,
    "property": 0xB90, "wrapper": 0xCC0, "dispatchWrapper": 0x1D24,
}
# Internal slot -> 1-based JUCEJack input port, read from the gather.
SLOT_PORTS = {0: 1, 1: 2, 2: 3, 3: 4, 10: 5, 11: 6, 4: 7, 5: 8, 8: 9, 9: 10, 6: 11, 7: 12,
              **{22 + i: 13 + i for i in range(16)}, **{14 + i: 29 + i for i in range(6)},
              20: 35, 21: 36}
PAD_SLOTS = (12, 13)
# Stack slot pairs of the output gather -> 1-based JUCEJack output ports.
OUTPUT_SLOTS = {(0x148, 0x150): (13, 14), (0x108, 0x110): (15, 16), (0x130, 0x138): (17, 18),
                (0xC8, 0xD0): (19, 20), (0xD8, 0xE0): (21, 22), (0xE8, 0xF0): (23, 24),
                (0xF8, 0x100): (25, 26)}
# Named port connections the handler must produce (direction follows the handler).
INPUT_CONNECTIONS = {
    **{f"usb1in:capture_{i}": (7, 8)[i - 1] if i <= 2 else 10 + i for i in range(1, 19)},
    "usb2in:capture_1": 9, "usb2in:capture_2": 10, "usb2dev:capture_1": 9, "usb2dev:capture_2": 10,
    "usb1-chat-in:capture_1": 11, "usb1-chat-in:capture_2": 12,
    "btphone:capture_1": 5, "btphone:capture_2": 6,
    **{f"sip-audio-in:capture_{i}": 28 + i for i in range(1, 7)},
    "RodeRfSPI:TX1": 35, "RodeRfSPI:TX2": 36,
}
OUTPUT_CONNECTIONS = {
    "usb1out:playback_1": 15, "usb1out:playback_2": 16,
    **{f"usb1out:playback_{i}": 24 + i for i in range(3, 21)},
    "usb2out:playback_1": 19, "usb2out:playback_2": 20,
    "usb1-chat-out:playback_1": 17, "usb1-chat-out:playback_2": 18,
    "btspeaker:playback_1": 13, "btspeaker:playback_2": 14,
    **{f"sip-audio-out:playback_{i}": 20 + i for i in range(1, 7)},
    "sip-audio-out:playback_7": 45, "sip-audio-out:playback_8": 46,
}
# Processor members of the mixer that receive each output pair in the dispatch.
EXPECTED_DISPATCH = {0x2088: (17, 18), 0x2080: (15, 16), 0x2090: (19, 20),
                     0x2098: (21, 22), 0x20A0: (23, 24), 0x20A8: (25, 26)}
BT_DISPATCH = {0x2078: (13, 14)}
MIXER = 0xC0000  # the mixer object's second page group, x28 in Process


def popcount_below(mask, bit):
    return bin(mask & ((1 << bit) - 1)).count("1")


def port_connections(image, port_events):
    """Run the real port-event handler once per named port and record connects."""
    m = Machine(image)
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
    m.stubs[connect] = lambda: connections.append(
        [m.string(m.word(m.register(1))), m.string(m.word(m.register(2)))])
    closure, callback, manager = m.allocate(24), m.allocate(0x30), m.allocate(0xE0)
    device, vtable = m.allocate(8), m.allocate(0xE0)
    m.words(closure + 16, callback)
    m.words(callback + 0x18, manager)
    m.words(manager + 0xD8, device)
    m.words(device, vtable)
    m.words(vtable + 0xC8, connect)
    result = {}
    for name in list(INPUT_CONNECTIONS) + list(OUTPUT_CONNECTIONS):
        before = len(connections)
        m.words(closure, m.literal(name), m.literal(name))
        m.register(0, closure)
        m.run(port_events)
        made = connections[before:]
        assert len(made) == 1, (name, made)
        source, sink = made[0]
        if name in INPUT_CONNECTIONS:
            assert source == name and sink == f"JUCEJack:in_{INPUT_CONNECTIONS[name]}", made
        else:
            assert sink == name and source == f"JUCEJack:out_{OUTPUT_CONNECTIONS[name]}", made
        result[name] = made[0]
    return result


class ProcessMachine(Machine):
    """A Machine with a mixer object, prepared for Mixer::Process excerpts."""

    def __init__(self, image, process):
        super().__init__(image)
        self.process = process
        self.mixer = self.allocate(0xC8000)
        for role in ("prepareA", "prepareB", "prepareC", "property"):
            self.stubs[call_at(image, process + CALLS[role])] = lambda: 0
        for address, name in image.plt.items():
            if name == "free":
                self.stubs[address] = lambda: 0
            if name == "memcpy":
                self.stubs[address] = self.memcpy
        self.copies = []

    def memcpy(self):
        destination, source, size = (self.register(i) for i in range(3))
        assert size < 0x10000
        self.cpu.mem_write(destination, bytes(self.cpu.mem_read(source, size)))
        self.copies.append((destination, source, size))
        return destination

    def pointer_table(self, count):
        """Distinct, recognisable pointers standing in for sample buffers."""
        return [self.allocate(FRAMES * 4) for _ in range(count)]

    def frame(self):
        self.cpu.reg_write(regs.UC_ARM64_REG_SP, self.stack)
        self.register(29, self.stack)
        self.register(27, self.mixer)
        self.register(28, self.mixer + MIXER)

    def stack_word(self, offset, value=None):
        if value is None:
            return self.word(self.stack + offset)
        self.words(self.stack + offset, value)


def input_gather(image, process, board, mask):
    """Emulate the slot-table construction for one active-input mask."""
    assert mask & 0xF == 0xF, "the gather reads the first four inputs unconditionally"
    m = ProcessMachine(image, process)
    m.stubs[call_at(image, process + CALLS["board"])] = lambda: board
    inputs = m.pointer_table(bin(mask).count("1"))
    packed = m.allocate(8 * len(inputs))
    m.words(packed, *inputs)
    silence_table, silence = m.allocate(16), m.allocate(0x40)
    m.words(silence_table, silence)
    m.words(m.mixer + 0xC6CD8 + 0x10, silence_table)
    # The SMART Pad buffer object lives at mixer+0xc73c8; +0x10 is its channel table.
    pads = [m.allocate(0x40), m.allocate(0x40)]
    pad_table = m.allocate(16)
    m.words(pad_table, *pads)
    m.words(m.mixer + 0xC73C8 + 0x10, pad_table)
    m.cpu.mem_write(m.mixer + 0xC7800 + 0x1F8, struct.pack("<Q", 1))
    source_objects = [m.allocate(0x1400) for _ in range(2)]
    m.words(m.mixer + 0xC78B0, *source_objects)
    m.cpu.mem_write(m.mixer + 0xC4000 + 0x3A94, struct.pack("<ii", 0, 1))
    flags = m.allocate(0x200)
    m.words(m.mixer + MIXER + 0x19F8, flags)
    m.frame()
    m.register(20, packed)
    m.register(22, mask)
    m.run(process + INPUT_GATHER[0], process + INPUT_GATHER[1])
    table = [m.word(m.stack + 0x930 + 8 * slot) for slot in range(38)]
    labels = []
    active = [bit for bit in range(36) if mask >> bit & 1]
    for slot, value in enumerate(table):
        if value == silence:
            labels.append("silence")
        elif value in pads:
            labels.append(f"pad{'LR'[pads.index(value)]}")
        elif value in inputs:
            labels.append(f"in_{active[inputs.index(value)] + 1}")
        else:
            raise ValueError(f"slot {slot} holds an unexpected pointer {value:#x}")
    for slot, label in enumerate(labels):
        if slot in PAD_SLOTS:
            assert label == f"pad{'LR'[slot - 12]}", (slot, label)
            continue
        port = SLOT_PORTS[slot]
        if board == 1 and slot == 3:
            assert label == "silence", "the Duo silences the fourth hardware input"
        elif mask >> (port - 1) & 1:
            assert label == f"in_{port}", (slot, label, port)
        else:
            assert label == "silence", (slot, label, port)
    return labels


def output_gather(image, process, mask, hardware_count, owned_mask):
    """Emulate the assignment of packed outputs to fixed stack slots."""
    m = ProcessMachine(image, process)
    outputs = m.pointer_table(bin(mask).count("1"))
    packed = m.allocate(8 * len(outputs))
    m.words(packed, *outputs)
    scratch_table = m.allocate(16)
    scratch = [m.allocate(0x40), m.allocate(0x40)]
    m.words(scratch_table, *scratch)
    # Inactive outputs are pointed at the mixer's scratch buffer object at mixer+0xc6e00.
    m.words(m.mixer + 0xC6E00 + 0x10, scratch_table)
    # Hardware output count / monitor indices live at mixer+0xc7d38.. (see duo_routes.monitor_fields).
    m.cpu.mem_write(m.mixer + 0xC4000 + 0x3D38, struct.pack("<i", hardware_count))
    m.cpu.mem_write(m.mixer + MIXER + 0x7D48, struct.pack("<Q", owned_mask))
    m.frame()
    m.stack_word(0x1B8, packed)
    m.stack_word(0x1A0, m.mixer + 0xC4000)
    m.cpu.mem_write(m.stack + 0x1B4, struct.pack("<I", FRAMES))
    m.register(26, mask)
    m.run(process + OUTPUT_GATHER[0], process + OUTPUT_GATHER[1])
    active = [bit for bit in range(46) if mask >> bit & 1]
    assignment = {}
    for (left, right), ports in OUTPUT_SLOTS.items():
        pair = []
        for offset, port in zip((left, right), ports):
            value = m.stack_word(offset)
            if value in scratch:
                label = "scratch"
                assert not mask >> (port - 1) & 1, (port, "inactive port expected")
            else:
                index = outputs.index(value)
                label = f"out_{active[index] + 1}"
                assert mask >> (port - 1) & 1 and label == f"out_{port}", (offset, label, port)
                assert index == hardware_count + popcount_below(mask >> 12, port - 13), (port, index)
            pair.append(label)
        assignment[f"out_{ports[0]}/out_{ports[1]}"] = pair
    next_index = m.register(19)
    assert next_index == hardware_count + popcount_below(mask >> 12, 14), next_index
    flags = {
        "ownedOutputsActive": m.word(m.stack + 0x118) & 0xFFFFFFFF,
        "usb1Channel18Active": m.stack_word(0x120) != 0,
        "rawStripCopyMode": bool(m.cpu.mem_read(m.stack + 0x1CF, 1)[0]),
    }
    assert flags["ownedOutputsActive"] == int(mask & owned_mask != 0)
    assert flags["usb1Channel18Active"] == bool(mask >> 41 & 1)
    assert flags["rawStripCopyMode"] == (flags["usb1Channel18Active"] and not flags["ownedOutputsActive"])
    return m, outputs, active, assignment, flags


def output_dispatch(image, process, mask, hardware_count, owned_mask):
    """Continue after the gather: record which processor member gets which pair."""
    m, outputs, active, assignment, flags = output_gather(image, process, mask, hardware_count, owned_mask)
    wrappers = {}

    def wrapper():
        table, channels = m.register(1), m.register(2)
        wrappers[m.register(0)] = tuple(m.word(table + 8 * i) for i in range(channels))

    m.stubs[call_at(image, process + CALLS["dispatchWrapper"])] = wrapper
    calls = []
    vtable = m.allocate(0x100)
    members = {}
    for offset in range(0x2000, 0x2700, 8):
        member = m.allocate(16)
        m.words(member, vtable)
        m.words(m.mixer + MIXER + offset, member)
        members[member] = offset

    def recorder(index):
        def record():
            # Wrapper objects are reused on the stack: resolve the buffers now.
            calls.append((members[m.register(0)], index, wrappers.get(m.register(1)), m.register(2)))
        return record

    for index in range(32):
        stub = m.stop + 0x1000 + 16 * index
        m.words(vtable + 8 * index, stub)
        m.stubs[stub] = recorder(index)
    multitrack = [m.allocate(FRAMES * 4) for _ in range(18)]
    for index, buffer in enumerate(multitrack):
        m.cpu.mem_write(buffer, bytes([index + 1]) * FRAMES * 4)
    m.words(m.mixer + 0xC7BD8, *multitrack)
    for address, name in image.plt.items():
        if name == "printf":
            m.stubs[address] = lambda: 0
    context = m.allocate(0x40)
    m.stack_word(0x88, context)
    m.stack_word(0xB0, m.stack + 0x6E0)
    m.stack_word(0x128, m.mixer + 0xC7050)
    m.stack_word(0xA0, m.mixer + 0xC7000)
    m.stack_word(0xA8, mask >> 12 & 1)
    m.stack_word(0xB8, 0)
    m.stack_word(0xC0, 0)
    m.register(22, mask & owned_mask)
    m.register(19, hardware_count + popcount_below(mask >> 12, 14))
    m.register(26, m.stack + 0x808)
    m.register(3, FRAMES)  # loaded at Process+0x16e8 before this branch
    if flags["rawStripCopyMode"]:
        # Actual raw-mode branch copies the nine strips' source buffers. The
        # first strip pair uses out_15/16; the other eight use out_27..42.
        stream = m.allocate(0x480)
        m.words(m.mixer + MIXER + 0x1FB0, stream)
        for index in range(9):
            entry, strip, source, table = (m.allocate(n) for n in (0x350, 0x858, 0x480, 16))
            m.words(stream + 0x438 + index * 8, entry)
            m.words(entry + 8, strip)
            m.words(strip + 0x460, source)
            m.words(source + 0x360, table)
            m.words(table, *multitrack[2 * index:2 * index + 2])
        m.run(process + 0x16F0, process + OUTPUT_DISPATCH[1])
    else:
        m.run(process + OUTPUT_DISPATCH[0], process + OUTPUT_DISPATCH[1])

    def ports(pointers):
        return tuple(active[outputs.index(p)] + 1 if p in outputs else None for p in pointers)

    dispatch = {}
    other = []
    for member, index, buffers, _ in calls:
        if index == 3 and buffers is not None:
            assert member not in dispatch, hex(member)
            dispatch[member] = ports(buffers)
        else:
            other.append({"member": hex(member), "vtableIndex": index})
    expected = dict(EXPECTED_DISPATCH)
    if flags["rawStripCopyMode"]:
        # Raw strips replace Main send; Chat's synchronous Process call is
        # skipped here too (its mix-minus preparation is separately gated).
        del expected[0x2080]
        del expected[0x2088]
        assert 0x2080 not in dispatch and 0x2088 not in dispatch
    if mask >> 12 & 1:
        expected.update(BT_DISPATCH)
    for member, pair in expected.items():
        if all(mask >> (port - 1) & 1 for port in pair):
            assert dispatch.get(member) == pair, (hex(member), dispatch.get(member), pair)
        else:
            assert dispatch.get(member) == tuple(port if mask >> (port - 1) & 1 else None for port in pair), (hex(member), dispatch.get(member))
    copies = []
    if flags["ownedOutputsActive"] or flags["rawStripCopyMode"]:
        first = hardware_count + popcount_below(mask >> 12, 14)
        destinations = list(outputs[first:first + 18])
        if flags["rawStripCopyMode"]:
            destinations = [outputs[active.index(p - 1)] for p in (15, 16)] + destinations[:16]
        for i, (destination, source, size) in enumerate(m.copies):
            assert size == FRAMES * 4 and source == multitrack[i], (i, hex(source))
            assert destination == destinations[i], (i, hex(destination))
            assert m.cpu.mem_read(destination, size) == m.cpu.mem_read(source, size)
            copies.append(f"out_{active[outputs.index(destination)] + 1}")
        assert len(m.copies) == 18, len(m.copies)
    else:
        assert not m.copies
    return {
        "assignment": assignment, "flags": flags,
        "dispatch": {hex(member): [f"out_{p}" if p is not None else "scratch" for p in pair] for member, pair in dispatch.items()},
        "otherVirtualCalls": other,
        "multitrackCopies": copies,
    }


def bridge_masks(image, bridge):
    """The caller of Mixer::Process folds exactly 36 input and 46 output channel bits."""
    instructions = image.instructions(bridge)
    bounds = [i.op_str for i in instructions if i.mnemonic == "cmp" and i.op_str in ("x0, #0x24", "x0, #0x2e")]
    assert bounds == ["x0, #0x24", "x0, #0x2e"], bounds
    virtual = [i.op_str for i in instructions if i.mnemonic == "ldr" and i.op_str in ("x1, [x1, #0x90]", "x1, [x1, #0x88]")]
    assert virtual == ["x1, [x1, #0x90]", "x1, [x1, #0x88]"], virtual
    calls = [i for i in instructions if i.mnemonic == "bl"]
    assert calls[0].operands[0].imm == image.function_at(calls[0].operands[0].imm)[0]
    return {"inputBits": 36, "outputBits": 46, "process": hex(calls[0].operands[0].imm),
            "activeChannelGetters": ["vtable+0x90", "vtable+0x88"]}


def string_address(image, text):
    data = image.data
    needle = b"\0" + text.encode() + b"\0"
    offset = data.find(needle)
    assert offset >= 0 and data.find(needle, offset + 1) < 0, text
    offset += 1
    for segment in image.loads:
        if segment["p_offset"] <= offset < segment["p_offset"] + segment["p_filesz"]:
            return segment["p_vaddr"] + offset - segment["p_offset"]
    raise ValueError(text)


def referenced_by(image, text, function):
    address = string_address(image, text)
    functions = {image.function_at(reference)[0] for reference, _ in image.address_references(address)}
    assert function in functions, (text, hex(function), sorted(map(hex, functions)))
    return hex(address)


def active_mask_source(image, active_mask, registration):
    """The active channel sets are rebuilt from jack_port_connected after registration."""
    connected = referenced_by(image, "jack_port_connected", active_mask)
    callers = [i.address for i in image.instructions(registration)
               if i.mnemonic == "bl" and i.operands[0].imm == active_mask]
    assert len(callers) == 1, callers
    return {"dlsym": connected, "calledFromRegistration": hex(callers[0])}


def registration(image, function):
    """in_N / out_N are registered sequentially as 32-bit float mono audio ports."""
    strings = {text: referenced_by(image, text, function) for text in ("in_", "out_", "32 bit float mono audio")}
    instructions = image.instructions(function)
    flags = [i.op_str for i in instructions if i.mnemonic == "mov" and i.op_str in ("x3, #1", "x3, #2")]
    assert flags == ["x3, #1", "x3, #2"], flags
    increments = [i.op_str for i in instructions if i.mnemonic == "str" and i.op_str in ("w0, [x19, #0x88]", "w0, [x19, #0x8c]")]
    assert increments == ["w0, [x19, #0x88]", "w0, [x19, #0x8c]"], increments
    return {"strings": strings, "portFlags": {"in_": 1, "out_": 2},
            "note": "Registers in_<n>/out_<n> for n = registered+1 .. size of the channel-name array passed to open; the array sizes are not pinned offline."}


def job_output_use(image, body):
    """The queued job reads the output table once, for the two monitor indices."""
    instructions = image.instructions(body)
    expected = ["ldp x3, x4, [x19]", "ldrsw x5, [x3, #0x3d3c]", "ldrsw x6, [x3, #0x3d40]",
                "ldr x5, [x4, x5, lsl #3]", "ldr x4, [x4, x6, lsl #3]"]
    seen = [f"{i.mnemonic} {i.op_str}" for i in instructions if f"{i.mnemonic} {i.op_str}" in expected]
    assert seen == expected, seen
    table_reads = [i for i in instructions if "[x19, #8]" in i.op_str or i.op_str.startswith("x3, x4, [x19]")]
    assert len(table_reads) == 1, [hex(i.address) for i in table_reads]
    return {"outputTableReads": 1, "indices": ["mixer+0xc7d3c", "mixer+0xc7d40"],
            "note": "Monitor left/right packed indices only; the USB sends are written in Mixer::Process before it returns."}


def source_chain(image, source_change):
    table = source_table(image, source_change)
    rows = {row[0]: row[1:] for row in table["sourceIdAndInternalSlots"] if row[0] >= 0}
    chain = {}
    for source in list(range(7, 12)) + list(range(12, 30)):
        slots = rows[source]
        ports = []
        for slot in slots:
            if slot < 0:
                ports.append(None)
            elif slot in PAD_SLOTS:
                ports.append("internal:smartpad")
            else:
                ports.append(f"JUCEJack:in_{SLOT_PORTS[slot]}")
        chain[source] = {"slots": slots, "ports": ports}
    for source, alias in zip(range(12, 16), range(22, 26)):
        assert chain[source] == chain[alias], (source, alias)
    return {"table": table, "sourceToPorts": chain,
            "aliases": {str(source): source + 10 for source in range(12, 16)}}


def analyse(image, sites, roles):
    process = sites["mixerProcess"]
    full_in = (1 << 36) - 1
    stereo_in = full_in & ~(((1 << 16) - 1) << 12)
    full_out = (1 << 46) - 1
    pro_hardware = full_out & ~(1 << 5) & ~(1 << 9) & ~(((1 << 34) - 1) << 12)
    stereo_out = pro_hardware | (0xF << 14) | (0x3 << 18) | (0x3F << 20) | (0x3 << 44)
    multitrack_out = stereo_out | (((1 << 18) - 1) << 26)
    owned = ((1 << 18) - 1) << 26
    inputs = {
        "allInputsProII": input_gather(image, process, 0, full_in),
        "stereoUsbProII": input_gather(image, process, 0, stereo_in),
        "noBluetoothNoUsb2ProII": input_gather(image, process, 0, full_in & ~(0x3 << 4) & ~(0x3 << 8)),
        "micsOnlyProII": input_gather(image, process, 0, 0xF),
        "allInputsDuo": input_gather(image, process, 1, full_in),
    }
    outputs = {
        "stereoUsbProII": output_dispatch(image, process, stereo_out, 10, owned),
        "stereoUsbNoUsb2ProII": output_dispatch(image, process, stereo_out & ~(0x3 << 18), 10, owned),
        "stereoUsbWithBluetoothSpeakerProII": output_dispatch(image, process, stereo_out | (0x3 << 12), 10, owned),
        "multitrackUsbProII": output_dispatch(image, process, multitrack_out, 10, owned),
        "multitrackRawUsbProII": output_dispatch(image, process, multitrack_out, 10, 0),
        "stereoUsbDuo": output_dispatch(image, process, (stereo_out & ~0xFFF) | 0xFF, 8, owned),
    }
    return {
        "mixerSha256": image.sha256,
        "locations": {"mixerProcess": hex(process), "jobBody": hex(sites["body"]),
                      "jackRegistrationActive": hex(sites["jackRegistrationActive"]),
                      **{name: hex(value) for name, value in roles.items()}},
        "processOffsets": {"inputGather": [hex(o) for o in INPUT_GATHER],
                           "outputGather": [hex(o) for o in OUTPUT_GATHER],
                           "outputDispatch": [hex(o) for o in OUTPUT_DISPATCH]},
        "portConnections": port_connections(image, roles["portEvents"]),
        "slotPorts": {str(slot): f"JUCEJack:in_{port}" for slot, port in sorted(SLOT_PORTS.items())},
        "inputGather": inputs,
        "outputGather": outputs,
        "bridge": bridge_masks(image, roles["callbackBridge"]),
        "activeMask": active_mask_source(image, roles["activeMask"], sites["jackRegistrationActive"]),
        "registration": registration(image, sites["jackRegistrationActive"]),
        "jobOutputUse": job_output_use(image, sites["body"]),
        "sources": source_chain(image, roles["sourceChange"]),
    }


def cross_check(images):
    """Every role maps across releases by relocation-normalized identity."""
    by_sha = {}
    for image, sites, roles in images:
        by_sha.setdefault(image.sha256, (image, sites, roles))
    if len(by_sha) < 2:
        return {"compared": False}
    (old, old_sites, old_roles), (new, new_sites, new_roles) = sorted(
        by_sha.values(), key=lambda item: item[2]["portEvents"])
    mapped = {}
    for name, address in old_roles.items():
        mapped[name] = counterpart(old, new, address)
        assert mapped[name] == new_roles[name], (name, hex(mapped[name]))
    for name in ("mixerProcess", "body", "jackRegistrationActive"):
        assert canonical(old, old_sites[name])["sha256"] == canonical(new, new_sites[name])["sha256"], name
    return {"compared": True, "baseline": old.sha256, "candidate": new.sha256,
            "roles": {name: hex(value) for name, value in mapped.items()}}


def audit(catalog, archives):
    if not __debug__:
        raise ValueError("audit assertions must be enabled")
    profiles = {profile["archiveSha256"]: profile for profile in catalog["profiles"]}
    images, report = [], {}
    for archive in archives:
        import hashlib
        sha = hashlib.sha256(archive.read_bytes()).hexdigest()
        profile = profiles.get(sha)
        if profile is None:
            raise ValueError(f"{archive} is not a catalogued official archive")
        image, sites = checked_image(archive, profile)
        roles = ROLES[image.sha256]
        images.append((image, sites, roles))
        report[profile["id"]] = {"archiveSha256": sha, **analyse(image, sites, roles)}
    return {
        "schemaVersion": 1,
        "profiles": report,
        "crossRelease": cross_check(images),
        "method": "Unicorn execution of the vendor port-event handler and of three excerpts of Mixer::Process "
                  "(input gather, output gather, output dispatch) with buffers replaced by recognisable pointers "
                  "and processors replaced by recording vtables; static instruction checks for the bridge, "
                  "active-mask source, registration loop and queued job.",
        "findings": [
            "Source IDs 12-15 and 22-25 share internal slots 22-29, which the gather fills from JUCEJack in_13..in_20 "
            "when those channels are active; the port-event handler connects usb1in:capture_3..10 to exactly those ports. "
            "These are four physical stereo pairs, not eight independent paths: IDs 12/22, 13/23, 14/24, 15/25 alias.",
            "Slots 30-37 (IDs 26-29) are in_21..in_28 (usb1in:capture_11..18). Slots 12-13 (ID 11, SMART Pads) are two "
            "mixer-internal buffer channels (object mixer+0xc73c8, channel-table pointer at +0x10), never a JACK port.",
            "JUCEJack out_15/16 (usb1out:playback_1/2) and out_19/20 (usb2out:playback_1/2) are written inside Mixer::Process, "
            "before it returns, by the processor members at mixer+0xc2080 and mixer+0xc2090; the queued job only writes the "
            "monitor outputs. Inactive (unconnected) outputs receive a mixer scratch buffer instead. In raw-strip multitrack "
            "mode, the first source strip is copied to out_15/16 instead of the main mix processor running: the port is "
            "still callback-owned, but its audio is not the stereo main mix in that mode. The synchronous Chat output "
            "processor dispatch is also skipped in that branch; Secondary continues to be dispatched.",
            "The active channel masks are rebuilt from jack_port_connected: an unconnected port is neither requested from "
            "jackd nor processed by the mixer in that period, so a path bound to it stays unobserved.",
        ],
        "limitations": [
            "The bridge consumes up to 36 input / 46 output bits; the engine construction of the channel-name arrays passed "
            "to JACK device open was not located. Sequential registration of old catalog in_35/36 implies registration of "
            "in_13..20 whenever the old plan successfully binds. Old out_18 does not by itself prove out_19/20 exists. "
            "The native pipeline requires every plan port registered before it can attach; absent new ports would prevent "
            "the whole pipeline from attaching, not merely skip those paths. Live port counts and USB-mode startup tests "
            "remain separate evidence.",
            "Both raw-strip and table-copy multitrack branches are emulated with synthetic active masks; the writer of the "
            "mask at mixer+0xc7d48 and its relationship to GUI multitrack mode settings were not located.",
            "Fixtures stub buffer constructors, processors, allocation, printf and memcpy; PAC/BTI hints are skipped.",
            "No device was flashed, probed or used for audio; live port snapshots are separate evidence.",
        ],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--archive", type=Path, action="append", required=True, help="official os.tar.gz (repeatable)")
    parser.add_argument("--catalog", type=Path, default=ROOT / "config/firmware-profiles.json")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    report = audit(json.loads(args.catalog.read_text(encoding="utf-8")), args.archive)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    print(f"PASS: {len(report['profiles'])} profiles; USB virtual paths and sends mapped. Report: {args.output}")


if __name__ == "__main__":
    main()
