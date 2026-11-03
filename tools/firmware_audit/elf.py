"""Offline ELF/function analysis. No firmware is extracted or executed natively."""
from __future__ import annotations

import hashlib
import io
from pathlib import Path
import struct
import tarfile

from capstone import Cs, CS_ARCH_ARM64, CS_MODE_LITTLE_ENDIAN
from elftools.elf.elffile import ELFFile
from elftools.dwarf.callframe import FDE


class Image:
    def __init__(self, archive_path: Path):
        self.archive_sha256 = hashlib.sha256(archive_path.read_bytes()).hexdigest()
        self.assets = {}
        with tarfile.open(archive_path, "r:gz") as archive:
            self.data = archive.extractfile("./usr/bin/rc_audio_mixer").read()
            for path in (
                "usr/bin/audio-start.sh", "usr/bin/rode-update.sh", "etc/init.d/uac2_gadget.sh",
                "lib/libc.so.6", "usr/lib/libstdc++.so.6", "usr/lib/libjack.so.0",
            ):
                try:
                    self.assets[path] = hashlib.sha256(archive.extractfile("./" + path).read()).hexdigest()
                except (KeyError, AttributeError):
                    continue
        self.sha256 = hashlib.sha256(self.data).hexdigest()
        self.elf = ELFFile(io.BytesIO(self.data))
        self.loads = [p for p in self.elf.iter_segments() if p["p_type"] == "PT_LOAD"]
        self.fdes = {e["initial_location"]: e for e in self.elf.get_dwarf_info().EH_CFI_entries()
                     if isinstance(e, FDE)}
        self.functions = {address: e["address_range"] for address, e in self.fdes.items()}
        self.disassembler = Cs(CS_ARCH_ARM64, CS_MODE_LITTLE_ENDIAN)
        self.disassembler.detail = True
        self.relocations = {}
        for section in self.elf.iter_sections():
            if section["sh_type"] != "SHT_RELA":
                continue
            symbols = self.elf.get_section(section["sh_link"])
            for entry in section.iter_relocations():
                name = symbols.get_symbol(entry["r_info_sym"]).name if entry["r_info_sym"] else ""
                self.relocations[entry["r_offset"]] = (name, entry["r_addend"])
        self.type_info = {}
        for address, (name, _) in self.relocations.items():
            if name.startswith("_ZTVN10__cxxabiv1") and "type_info" in name:
                label = self.relocations.get(address + 8)
                if label and not label[0]:
                    self.type_info[address] = self.code(label[1], 256).split(b"\0")[0].decode("ascii")
        self.plt = {}
        relocations = self.elf.get_section_by_name(".rela.plt")
        plt = self.elf.get_section_by_name(".plt")
        if relocations is not None and plt is not None:
            symbols = self.elf.get_section(relocations["sh_link"])
            for index, entry in enumerate(relocations.iter_relocations()):
                self.plt[plt["sh_addr"] + 32 + index * 16] = symbols.get_symbol(entry["r_info_sym"]).name

    def code(self, address: int, size: int) -> bytes:
        for segment in self.loads:
            if segment["p_vaddr"] <= address and address + size <= segment["p_vaddr"] + segment["p_filesz"]:
                offset = segment["p_offset"] + address - segment["p_vaddr"]
                return self.data[offset:offset + size]
        raise ValueError(f"unmapped file-backed address {address:#x}+{size:#x}")

    def function_at(self, address: int) -> tuple[int, int]:
        matches = [(start, size) for start, size in self.functions.items() if start <= address < start + size]
        if len(matches) != 1:
            raise ValueError(f"no unique unwind function at {address:#x}")
        return matches[0]

    def instructions(self, address: int):
        return list(self.disassembler.disasm(self.code(address, self.functions[address]), address))

    def address_references(self, target: int) -> list[tuple[int, int]]:
        """Find ADRP + nearby ADD address materializations, with register-clobber checks."""
        text = self.elf.get_section_by_name(".text")
        data, base = text.data(), text["sh_addr"]
        hits = []
        for offset in range(0, len(data) - 36, 4):
            word = struct.unpack_from("<I", data, offset)[0]
            if word & 0x9F000000 != 0x90000000:
                continue
            immediate = ((word >> 29) & 3) | (((word >> 5) & 0x7FFFF) << 2)
            if immediate & (1 << 20):
                immediate -= 1 << 21
            page = ((base + offset) & ~4095) + (immediate << 12)
            if page != target & ~4095:
                continue
            register = word & 31
            for delta in range(4, 36, 4):
                next_word = struct.unpack_from("<I", data, offset + delta)[0]
                if (next_word & 0xFFC00000 == 0x91000000 and (next_word >> 5) & 31 == register
                        and page + ((next_word >> 10) & 0xFFF) == target):
                    hits.append((base + offset, base + offset + delta))
                    break
                instruction = next(self.disassembler.disasm(data[offset + delta:offset + delta + 4], base + offset + delta))
                names = {self.disassembler.reg_name(r) for r in instruction.regs_access()[1]}
                if f"x{register}" in names or f"w{register}" in names or instruction.mnemonic in ("b", "ret", "br"):
                    break
        return hits

    def exception_edges(self, address: int) -> list[tuple[int, int, int]]:
        """Decode the LSDA call-site table so exception landing pads retain the right register facts."""
        pointer = self.fdes[address].lsda_pointer
        if pointer is None:
            return []
        data = self.code(pointer, 4096)
        cursor = 0

        def byte():
            nonlocal cursor
            value = data[cursor]
            cursor += 1
            return value

        def leb(signed=False):
            value = shift = 0
            while True:
                part = byte()
                value |= (part & 127) << shift
                shift += 7
                if not part & 128:
                    return value - (1 << shift) if signed and part & 64 else value

        def encoded(encoding):
            form = encoding & 15
            if form == 1:
                return leb()
            if form == 9:
                return leb(True)
            sizes = {2: (2, False), 3: (4, False), 4: (8, False), 10: (2, True), 11: (4, True), 12: (8, True)}
            size, signed = sizes[form]
            return int.from_bytes(bytes(byte() for _ in range(size)), "little", signed=signed)

        lp_encoding = byte()
        if lp_encoding != 255:
            raise ValueError("audit does not support an explicit LSDA LPStart")
        if byte() != 255:
            leb()  # type table offset; not needed for landing-pad analysis
        call_encoding = byte()
        length = leb()
        end = cursor + length
        result = []
        while cursor < end:
            start, size, landing = (encoded(call_encoding) for _ in range(3))
            leb()  # exception action
            if landing:
                result.append((address + start, address + start + size, address + landing))
        return result
