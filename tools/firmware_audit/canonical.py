"""Compare AArch64 semantics while retaining registers, field offsets and CFG edges.

Only direct code addresses, ADRP address materializations and named RTTI
locations are normalized. Stack sizes, capture offsets, widths and operation
constants remain part of the comparison. LSDA exception paths participate in
the register data-flow analysis.
"""
from collections import deque
import hashlib
import json

from capstone.arm64 import ARM64_OP_REG, ARM64_OP_IMM, ARM64_OP_MEM


def reg_name(image, register):
    name = image.disassembler.reg_name(register)
    return "x" + name[1:] if name.startswith("w") and name[1:].isdigit() else name


def is_branch(instruction):
    return instruction.mnemonic in ("b", "bl", "cbz", "cbnz", "tbz", "tbnz") or instruction.mnemonic.startswith("b.")


def register_states(image, address, instructions):
    lookup = {i.address: i for i in instructions}
    exceptions = image.exception_edges(address)
    states = {address: {}}
    pending = deque([address])

    def merge(target, state):
        if target not in lookup:
            return
        if target not in states:
            states[target] = state.copy()
            pending.append(target)
        else:
            merged = {k: v for k, v in states[target].items() if state.get(k) == v}
            if merged != states[target]:
                states[target] = merged
                pending.append(target)

    visits = 0
    while pending:
        visits += 1
        if visits > len(instructions) * 64:
            raise ValueError("register analysis did not converge")
        pc = pending.popleft()
        instruction = lookup[pc]
        operands, state = instruction.operands, states[pc]
        assignments = {}
        if instruction.mnemonic in ("adr", "adrp"):
            assignments[reg_name(image, operands[0].reg)] = (
                "page" if instruction.mnemonic == "adrp" else "address", operands[1].imm)
        elif instruction.mnemonic == "add" and len(operands) == 3 and operands[2].type == ARM64_OP_IMM:
            source = state.get(reg_name(image, operands[1].reg))
            if source:
                assignments[reg_name(image, operands[0].reg)] = (
                    "address", source[1] + (operands[2].imm << operands[2].shift.value))
        elif instruction.mnemonic == "mov" and len(operands) == 2 and operands[1].type == ARM64_OP_REG:
            source = state.get(reg_name(image, operands[1].reg))
            if source:
                assignments[reg_name(image, operands[0].reg)] = source
        outgoing = state.copy()
        for register in instruction.regs_access()[1]:
            outgoing.pop(reg_name(image, register), None)
        outgoing.update(assignments)
        if instruction.mnemonic in ("bl", "blr"):
            for register in range(19):
                outgoing.pop(f"x{register}", None)
            for start, end, landing in exceptions:
                if start <= pc < end:
                    merge(landing, {k: v for k, v in state.items() if k in {f"x{r}" for r in range(19, 29)}})
            if instruction.mnemonic == "bl" and image.plt.get(operands[-1].imm) in {
                "_Unwind_Resume", "__cxa_throw", "__stack_chk_fail", "abort", "_ZSt9terminatev",
            }:
                continue
        if is_branch(instruction) and instruction.mnemonic != "bl":
            merge(operands[-1].imm, outgoing)
            if instruction.mnemonic == "b":
                continue
        if instruction.mnemonic not in ("ret", "br"):
            merge(pc + 4, outgoing)
    return states


def canonical(image, address):
    size = image.functions[address]
    instructions = image.instructions(address)
    if len(instructions) * 4 != size:
        raise ValueError(f"incomplete instruction decode at {address:#x}")
    states = register_states(image, address, instructions)
    output, calls, globals_ = [], [], []
    for instruction in instructions:
        state = states.get(instruction.address, {})
        operands = instruction.operands
        address_add = None
        if instruction.mnemonic == "add" and len(operands) == 3 and operands[2].type == ARM64_OP_IMM:
            source = state.get(reg_name(image, operands[1].reg))
            if source:
                target = source[1] + (operands[2].imm << operands[2].shift.value)
                if source[0] == "page":
                    address_add = "page-relative-address"
                elif target in image.type_info:
                    address_add = ("type-info", image.type_info[target])
                if address_add:
                    globals_.append((instruction.address - address, target))
        values = []
        for index, operand in enumerate(operands):
            if operand.type == ARM64_OP_REG:
                value = ("reg", image.disassembler.reg_name(operand.reg))
            elif operand.type == ARM64_OP_IMM:
                immediate = operand.imm
                if instruction.mnemonic in ("adr", "adrp"):
                    immediate = "address"
                elif address_add and index == 2:
                    immediate = address_add
                elif is_branch(instruction) and index == len(operands) - 1:
                    if address <= immediate < address + size:
                        immediate = ("internal", immediate - address)
                    else:
                        symbol = image.plt.get(immediate)
                        calls.append((instruction.address - address, instruction.mnemonic, immediate, symbol))
                        immediate = ("external", symbol)
                elif instruction.mnemonic.startswith("ldr") and index == 1:
                    globals_.append((instruction.address - address, immediate))
                    immediate = "literal-address"
                value = ("imm", immediate)
            elif operand.type == ARM64_OP_MEM:
                displacement = operand.mem.disp
                base = state.get(reg_name(image, operand.mem.base))
                if base and base[0] == "page":
                    globals_.append((instruction.address - address, base[1] + displacement))
                    displacement = "page-relative-address"
                value = ("mem", image.disassembler.reg_name(operand.mem.base),
                         image.disassembler.reg_name(operand.mem.index), displacement)
            else:
                value = ("other", instruction.op_str)
            values.append((value, operand.shift.type, operand.shift.value, operand.ext,
                           operand.vector_index, operand.vas))
        output.append((instruction.mnemonic, instruction.cc, instruction.update_flags,
                       instruction.writeback, tuple(values)))
    return {"instructions": output, "calls": calls, "globals": globals_,
            "sha256": hashlib.sha256(json.dumps(output, separators=(",", ":")).encode()).hexdigest()}
