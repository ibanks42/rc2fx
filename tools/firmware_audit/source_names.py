"""Read the vendor's source-name switch from pinned firmware, entirely offline.

This establishes enum labels, not live availability or processing qualification.
Uses only capstone and pyelftools from requirements.txt; no mixer is contacted.
"""
from __future__ import annotations

import argparse
import json
from pathlib import Path
import struct

from .elf import Image

ROOT = Path(__file__).resolve().parents[2]
EXPECTED = {
    7: "USB 1 Main", 8: "USB 1 Chat", 9: "USB 2", 10: "Bluetooth",
    11: "Sounds", 12: "Game", 13: "Music", 14: "Virtual A", 15: "Virtual B",
    16: "CallMe 1", 17: "CallMe 2", 18: "CallMe 3",
    19: "Wireless 1", 20: "Wireless 2",
    **{i: f"RCV Channel {i - 20}" for i in range(21, 30)},
}


def virtual_address(image, offset):
    return next(p["p_vaddr"] + offset - p["p_offset"] for p in image.loads
                if p["p_offset"] <= offset < p["p_offset"] + p["p_filesz"])


def audit(archive: Path, profile: dict) -> dict:
    image = Image(archive)
    if (image.archive_sha256 != profile["archiveSha256"]
            or image.sha256 != profile["mixerSha256"]):
        raise ValueError("archive or mixer differs from the selected catalog profile")
    game = virtual_address(image, image.data.index(b"Game\0"))
    functions = {image.function_at(a)[0] for a, _ in image.address_references(game)}
    if len(functions) != 1:
        raise ValueError("expected one source-label switch containing Game")
    function = functions.pop()
    instructions = image.instructions(function)
    dispatches = []
    for index in range(len(instructions) - 5):
        a, b, c, d, e, f = instructions[index:index + 6]
        if ([i.mnemonic for i in (a, b, c, d, e, f)]
                == ["adrp", "add", "ldrh", "adr", "add", "br"]
                and a.op_str.startswith("x0, ")
                and b.op_str.startswith("x0, x0, ")
                and c.op_str == "w0, [x0, w25, uxtw #1]"
                and d.op_str.startswith("x1, ")
                and e.op_str == "x0, x1, w0, sxth #2"
                and f.op_str == "x0"):
            dispatches.append((a, b, d))
    if len(dispatches) != 1:
        raise ValueError("source-name jump-table dispatch changed")
    page, add, anchor = dispatches[0]
    table = page.operands[1].imm + add.operands[2].imm
    base = anchor.operands[1].imm
    # The index is the channel's source enum + 1, not its fader position.
    guards = []
    for index in range(len(instructions) - 3):
        a, b, c, d = instructions[index:index + 4]
        if (a.mnemonic == "ldr" and a.op_str == "w28, [x0, #0xe4]"
                and b.mnemonic == "add" and b.op_str == "w25, w28, #1"
                and c.mnemonic == "cmp" and c.op_str == "w25, #0x1e"
                and d.mnemonic == "b.ls" and d.operands[0].imm == page.address):
            guards.append(a.address)
    if len(guards) != 1:
        raise ValueError("source index or bounds guard changed")
    labels = []
    for source, expected in EXPECTED.items():
        relative, = struct.unpack("<h", image.code(table + 2 * (source + 1), 2))
        target = base + relative * 4
        block = list(image.disassembler.disasm(image.code(target, 28), target))
        if ([i.mnemonic for i in block] != ["bti", "add", "adrp", "mov", "add", "bl", "b"]
                or not block[2].op_str.startswith("x1, ")
                or block[4].op_str.split(", ")[:2] != ["x1", "x1"]):
            raise ValueError(f"source {source}: literal-name block changed")
        address = block[2].operands[1].imm + block[4].operands[2].imm
        label = image.code(address, 64).split(b"\0", 1)[0].decode("ascii")
        if label != expected:
            raise ValueError(f"source {source}: expected {expected!r}, got {label!r}")
        labels.append({"sourceId": source, "name": label,
                       "caseAddress": hex(target), "stringAddress": hex(address)})
    return {
        "profile": profile["id"], "archiveSha256": image.archive_sha256,
        "mixerSha256": image.sha256, "function": hex(function),
        "sourceLoad": hex(guards[0]), "table": hex(table), "branchBase": hex(base),
        "labels": labels,
        "limitations": ["Names only: this does not admit audio processing routes.",
                        "RCV/virtual enum records alone do not prove active hardware mode.",
                        "No device was contacted; no personal saved-show labels were used."],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archive", type=Path, required=True)
    parser.add_argument("--profile", required=True)
    parser.add_argument("--catalog", type=Path, default=ROOT / "config/firmware-profiles.json")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    catalog = json.loads(args.catalog.read_text())
    profile = next(p for p in catalog["profiles"] if p["id"] == args.profile)
    report = audit(args.archive, profile)
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(report, indent=2) + "\n")
    print(f"PASS: {args.profile}: {len(report['labels'])} source labels verified; {args.output}")


if __name__ == "__main__":
    main()
