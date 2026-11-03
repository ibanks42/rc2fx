"""Execute selected vendor instructions under Unicorn, with OS/JUCE services stubbed.

This checks closure/copy/invocation semantics locally, not audio performance or
device transport. PAC/BTI hints are skipped; allocation, strings, events and
thread-pool submission have deterministic fixture implementations.
"""
import struct

from unicorn import Uc, UC_ARCH_ARM64, UC_MODE_ARM, UC_HOOK_CODE
from unicorn import arm64_const as regs

from .canonical import canonical


class Harness:
    def __init__(self, image, sites):
        self.image, self.sites = image, sites
        self.cpu = Uc(UC_ARCH_ARM64, UC_MODE_ARM)
        end = max(p["p_vaddr"] + p["p_memsz"] for p in image.loads)
        self.cpu.mem_map(0, (end + 4095) & ~4095)
        for segment in image.loads:
            self.cpu.mem_write(segment["p_vaddr"], image.data[segment["p_offset"]:segment["p_offset"] + segment["p_filesz"]])
        self.heap, self.stack, self.stop = 0x10000000, 0x20008000, 0x30000000
        self.cpu.mem_map(self.heap, 0x100000)
        self.cpu.mem_map(0x20000000, 0x10000)
        self.cpu.mem_map(self.stop, 4096)
        self.next_heap = self.heap
        self.allocations, self.frees, self.manager_ops, self.queued = [], [], [], []
        self.stubs = {}
        for address, name in image.plt.items():
            if name == "_Znwm":
                self.stubs[address] = self.operator_new
            elif name == "_ZdlPvm":
                self.stubs[address] = self.operator_delete
        enqueue = canonical(image, sites["enqueue"])
        calls = {relative: target for relative, _, target, _ in enqueue["calls"]}
        globals_ = dict(enqueue["globals"])
        self.empty_string = globals_[0x5C]
        self.stubs[calls[0x40]] = lambda: self.empty_string + 16
        self.stubs[calls[0x84]] = lambda: 0  # WaitableEvent construction is outside the copy ABI.
        self.stubs[calls[0xF0]] = self.add_job
        self.cpu.hook_add(UC_HOOK_CODE, self.instruction)

    def read_register(self, index):
        return self.cpu.reg_read(getattr(regs, f"UC_ARM64_REG_X{index}"))

    def allocate(self, size):
        address = self.next_heap
        self.next_heap += (size + 15) & ~15
        if self.next_heap >= self.heap + 0x100000:
            raise AssertionError("fixture heap exhausted")
        self.cpu.mem_write(address, bytes(size))
        return address

    def words(self, address, count=4):
        return struct.unpack("<" + "Q" * count, bytes(self.cpu.mem_read(address, count * 8)))

    def write_words(self, address, *words):
        self.cpu.mem_write(address, struct.pack("<" + "Q" * len(words), *words))

    def operator_new(self):
        size = self.read_register(0)
        address = self.allocate(size)
        self.allocations.append((address, size))
        return address

    def operator_delete(self):
        self.frees.append((self.read_register(0), self.read_register(1)))
        return 0

    def add_job(self):
        pool, job, automatic = (self.read_register(i) for i in range(3))
        self.queued.append((pool, job, automatic, self.words(job + 0x60)))
        return 1

    def instruction(self, cpu, address, size, _):
        if address == self.sites["manager"]:
            self.manager_ops.append(self.read_register(2))
        if address in self.stubs:
            result = self.stubs[address]()
            cpu.reg_write(regs.UC_ARM64_REG_X0, result)
            cpu.reg_write(regs.UC_ARM64_REG_PC, self.read_register(30))
            return
        word = int.from_bytes(cpu.mem_read(address, 4), "little")
        if word in (0xD503233F, 0xD50323BF, 0xD503245F, 0xD503249F, 0xD50324DF):
            cpu.reg_write(regs.UC_ARM64_REG_PC, address + 4)

    def reset(self):
        for index in range(31):
            self.cpu.reg_write(getattr(regs, f"UC_ARM64_REG_X{index}"), 0)
        self.cpu.reg_write(regs.UC_ARM64_REG_SP, self.stack)
        self.cpu.reg_write(regs.UC_ARM64_REG_LR, self.stop)

    def call(self, address, *arguments):
        self.reset()
        for index, value in enumerate(arguments):
            self.cpu.reg_write(getattr(regs, f"UC_ARM64_REG_X{index}"), value)
        self.cpu.emu_start(address, self.stop, count=10000)
        assert self.cpu.reg_read(regs.UC_ARM64_REG_PC) == self.stop, "fixture did not return"
        return self.read_register(0)


def exercise(image, sites):
    machine = Harness(image, sites)
    source, payload, destination = machine.allocate(32), machine.allocate(32), machine.allocate(32)
    seed = bytes(range(32))
    machine.cpu.mem_write(payload, seed)
    machine.write_words(source, payload, 0, sites["manager"], sites["invoker"])
    assert machine.call(sites["manager"], destination, source, 2) == 0
    clone = machine.words(destination)[0]
    machine.cpu.mem_write(payload, b"\xff" * 32)
    assert bytes(machine.cpu.mem_read(clone, 32)) == seed
    query = machine.allocate(32)
    machine.call(sites["manager"], query, destination, 1)
    assert machine.words(query)[0] == clone
    machine.call(sites["manager"], query, destination, 0)
    rtti = machine.words(query)[0]
    type_name = image.type_info[rtti]
    assert type_name == "*ZN5Mixer7ProcessEPPKfiPPfijllEUlvE_"
    machine.call(sites["manager"], destination, destination, 3)
    assert machine.frees[-1] == (clone, 32)

    machine.allocations.clear()
    machine.frees.clear()
    machine.manager_ops.clear()
    machine.cpu.mem_write(payload, seed)
    pool = machine.allocate(64)
    machine.call(sites["enqueue"], pool, source)
    assert machine.manager_ops == [2, 2, 3], machine.manager_ops
    assert [size for _, size in machine.allocations] == [32, 128, 32]
    queued_pool, job, automatic, function = machine.queued[-1]
    capture, unused, manager, invoker = function
    assert (queued_pool, automatic, unused, manager, invoker) == (pool, 1, 0, sites["manager"], sites["invoker"])
    assert bytes(machine.cpu.mem_read(capture, 32)) == seed
    assert machine.frees == [machine.allocations[0]]
    machine.call(sites["manager"], job + 0x60, job + 0x60, 3)
    assert machine.frees[-1] == (capture, 32)

    machine.reset()
    machine.cpu.reg_write(regs.UC_ARM64_REG_X0, source)
    machine.cpu.emu_start(sites["invoker"], sites["body"], count=100)
    assert machine.cpu.reg_read(regs.UC_ARM64_REG_PC) == sites["body"]
    assert machine.read_register(0) == payload
    assert machine.call(sites["callback"], 112, 0) == 0

    references = image.address_references(sites["invoker"])
    assert len(references) == 1, references
    materialization = references[0][0]
    constructor_start, constructor_stop = materialization - 0x2C, materialization + 0x30
    observed = []

    def capture_constructor():
        function_address = machine.read_register(1)
        function = machine.words(function_address)
        observed.append((function_address, function, machine.words(function[0])))
        return 0

    machine.stubs[sites["enqueue"]] = capture_constructor
    machine.reset()
    mixer, output_table = machine.allocate(64), machine.allocate(64)
    machine.cpu.reg_write(regs.UC_ARM64_REG_X27, mixer)
    machine.write_words(machine.stack + 0x98, pool)
    machine.write_words(machine.stack + 0x1B8, output_table)
    machine.cpu.mem_write(machine.stack + 0x1B4, struct.pack("<I", 112))
    machine.cpu.mem_write(machine.stack + 0x1CF, b"\x01")
    machine.cpu.emu_start(constructor_start, constructor_stop, count=1000)
    assert machine.cpu.reg_read(regs.UC_ARM64_REG_PC) == constructor_stop
    function_address, function, captures = observed[0]
    assert function_address == machine.stack + 0x220
    assert function[2:] == (sites["manager"], sites["invoker"])
    assert captures == (mixer, machine.stack + 0x1B8, machine.stack + 0x1B4, machine.stack + 0x1CF)
    return {
        "managerCopies32Bytes": True,
        "managerSizedDelete32Bytes": True,
        "managerTypeName": type_name,
        "enqueueManagerOperations": [2, 2, 3],
        "enqueueAllocationSizes": [32, 128, 32],
        "queuedCaptureSurvivesTemporaryDestruction": True,
        "invokerPassesCaptureToMappedBody": True,
        "callbackNullContextReturnsZero": True,
        "constructorCaptureOffsets": [0, 8, 16, 24],
        "constructorCaptureReferences": ["mixer", "output-table-u64", "frame-count-u32", "flag-u8"],
        "constructorFunctionOffsets": {"manager": 16, "invoker": 24},
        "constructorCallSite": hex(materialization + 0x2C),
        "constructorParent": hex(image.function_at(materialization)[0]),
    }
