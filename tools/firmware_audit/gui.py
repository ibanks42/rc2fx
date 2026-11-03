"""Read-only, version-pinned UI-B ELF audit; never loads or executes firmware.

Run: python -m tools.firmware_audit.gui ARCHIVE [--out PATH]
Requires host capstone and pyelftools (see requirements.txt). Unicorn is unused.
Addresses are link-time ELF virtual addresses, NOT live pointers or file offsets.
The selected ranges/interpretations below are manually mapped UI-B evidence, not
symbols recovered from .symtab, type layouts, hook qualification, or authorization.
"""
from __future__ import annotations

import argparse
from bisect import bisect_right
import hashlib
import io
import json
from pathlib import Path
import struct
import sys
import tarfile

try:
    import capstone
    import elftools
    from capstone import Cs, CS_ARCH_ARM64, CS_MODE_LITTLE_ENDIAN
    from elftools.elf.elffile import ELFFile
    from elftools.dwarf.callframe import FDE
except ImportError as error:
    raise SystemExit("Host audit dependencies missing: install tools/firmware_audit/requirements.txt "
                     "in a host virtualenv; no firmware execution is needed.") from error

GUI_SHA256 = "aa92a92d384bdb24bc1cdb01615ac280d4d4867e661158df5c7f86ff98b605ca"
TYPES = (
    "N4juce23RodecasterComponentPeer13RcInputThreadE",
    "N4juce23RodecasterComponentPeer16RcRepaintManagerE",
    "N4juce19RodecasterDRMThreadE",
    "N4juce22RodecasterWindowSystemE",
    "N4juce13V4L2PixelDataE",
    "N4juce13V4L2ImageTypeE",
)
# Labels are analyst interpretations, explicitly not vendor debug symbols.
FUNCTIONS = {
    0x7CF020: "V4L2PixelData bitmap-address calculation",
    0x7CF230: "registered DRM page-flip callback (no-op)",
    0x7CF5D0: "RcRepaintManager third virtual target / repaint callback",
    0x7CF9F0: "RcInputThread third virtual target / select loop",
    0x7CFCA0: "V4L2 initialization",
    0x7D0480: "V4L2ImageType allocation virtual target",
    0x7D0560: "V4L2 queue / wait / dequeue",
    0x7D08A0: "RodecasterDRMThread third virtual target / frame loop",
    0x7D7710: "DRM dumb-buffer allocation / mmap / AddFB2 / PRIME export",
    0x7DC7F0: "peer construction / renderer + input setup",
    0x7E06D0: "input closure invoker / evdev read + stock dispatch",
    0x92E030: "RodecasterWindowSystem accessor + lazy initialization",
    0x93CE40: "plane atomic-property helper",
    0x93CF40: "CRTC atomic-property helper",
}
# Decisive bounded snippets; full selected-function digests are also emitted.
RANGES = (
    ("input_wait_and_closure", 0x7CFA48, 0x128),
    ("input_read_and_event_type", 0x7E0750, 0x90),
    ("input_abs_and_key_mapping", 0x7E0828, 0x90),
    ("input_stock_dispatch", 0x7E09FC, 0x48),
    ("input_coordinate_bounds", 0x7E07F0, 0x38),
    ("bitmap_address", 0x7CF030, 0x38),
    ("repaint_lock_render_notify", 0x7CF798, 0x90),
    ("v4l2_fixed_init_call", 0x7DC99C, 0x68),
    ("v4l2_rotation", 0x7CFD30, 0x20),
    ("v4l2_output_format", 0x7CFDD0, 0x30),
    ("v4l2_capture_format_rotation_branch", 0x7CFECC, 0x38),
    ("v4l2_capture_and_output_memory", 0x7CFF58, 0x80),
    ("v4l2_mmap_and_export", 0x7D0000, 0xBC),
    ("v4l2_capture_queue", 0x7D0568, 0x5C),
    ("v4l2_output_queue", 0x7D05F0, 0x4C),
    ("v4l2_success_dequeue", 0x7D07F4, 0x30),
    ("drm_create_map_addfb_export", 0x7D7718, 0x12C),
    ("frame_lock_and_convert", 0x7D09CC, 0x44),
    ("atomic_properties_and_commit", 0x7D0A14, 0xEC),
    ("initial_commit_flags", 0x7D0BF4, 0x2C),
    ("page_flip_wait", 0x7D0958, 0x74),
    ("page_flip_handle_event", 0x7D0C20, 0x0C),
    ("page_flip_noop_callback", 0x7CF230, 8),
    ("window_evdev_and_drm_open", 0x92E144, 0x90),
    ("mode_dimensions", 0x92E378, 0x24),
    ("drm_format_callback_two_buffers", 0x92E720, 0xC4),
)


def sha(data):
    return hashlib.sha256(data).hexdigest()


def hx(address):
    return f"0x{address:x}"


class GuiImage:
    """GUI-specific identity and ELF reader, deliberately not mixer Image."""
    def __init__(self, archive_path):
        archive_path = Path(archive_path)
        with archive_path.open("rb") as stream:
            self.archive_sha256 = hashlib.file_digest(stream, "sha256").hexdigest()
        with tarfile.open(archive_path, "r:gz") as archive:
            members = [m for m in archive.getmembers()
                       if m.name.removeprefix("./").lstrip("/") == "usr/bin/rc_gui_juce"]
            if len(members) != 1 or not members[0].isfile():
                raise ValueError("expected one regular usr/bin/rc_gui_juce archive member")
            self.data = archive.extractfile(members[0]).read()
            self.rotation = None
            for m in archive.getmembers():
                if m.name.removeprefix("./").lstrip("/") == "etc/rotation" and m.isfile():
                    self.rotation = archive.extractfile(m).read()
        self.sha256 = sha(self.data)
        if self.sha256 != GUI_SHA256:
            raise ValueError(f"unqualified GUI SHA256 {self.sha256}; expected {GUI_SHA256}")
        self.elf = ELFFile(io.BytesIO(self.data))
        if self.elf["e_machine"] != "EM_AARCH64" or not self.elf.little_endian or self.elf.elfclass != 64:
            raise ValueError("expected little-endian ELF64 AArch64")
        self.loads = [p for p in self.elf.iter_segments() if p["p_type"] == "PT_LOAD"]
        self.functions = {e["initial_location"]: e["address_range"]
                          for e in self.elf.get_dwarf_info().EH_CFI_entries() if isinstance(e, FDE)}
        self.starts = sorted(self.functions)
        self.cs = Cs(CS_ARCH_ARM64, CS_MODE_LITTLE_ENDIAN)
        self.relocs = {}
        for section in self.elf.iter_sections():
            if section["sh_type"] != "SHT_RELA":
                continue
            symbols = self.elf.get_section(section["sh_link"])
            for r in section.iter_relocations():
                symbol = symbols.get_symbol(r["r_info_sym"]) if r["r_info_sym"] else None
                self.relocs[r["r_offset"]] = {
                    "symbol": symbol.name if symbol else "", "addend": r["r_addend"],
                    "type": r["r_info_type"],
                }
        self.plt = {}
        self.plt_evidence = {}
        section = self.elf.get_section_by_name(".rela.plt")
        plt = self.elf.get_section_by_name(".plt")
        symbols = self.elf.get_section(section["sh_link"])
        for index, r in enumerate(section.iter_relocations()):
            address = plt["sh_addr"] + 32 + index * 16
            data = self.code(address, 16)
            adrp, ldr, add, branch = struct.unpack("<4I", data)
            # Verify this exact AArch64 PLT layout rather than trusting slot order.
            if (adrp & 0x9F00001F != 0x90000010 or ldr & 0xFFC003FF != 0xF9400211
                    or add & 0xFFC003FF != 0x91000210 or branch != 0xD61F0220):
                raise ValueError(f"unexpected PLT instructions at {address:#x}")
            immediate = ((adrp >> 29) & 3) | (((adrp >> 5) & 0x7FFFF) << 2)
            if immediate & (1 << 20):
                immediate -= 1 << 21
            got = (address & ~4095) + (immediate << 12) + (((ldr >> 10) & 0xFFF) * 8)
            if got != r["r_offset"] or r["r_info_type"] != 1026:
                raise ValueError(f"PLT/GOT relocation mismatch at {address:#x}")
            symbol = symbols.get_symbol(r["r_info_sym"])
            self.plt[address] = symbol.name
            self.plt_evidence[symbol.name] = {
                "plt_va": hx(address), "got_va": hx(got), "plt_bytes": data.hex(),
                "relocation_type": "R_AARCH64_JUMP_SLOT", "relocation_type_number": 1026,
                "symbol_type": symbol["st_info"]["type"], "binding": symbol["st_info"]["bind"],
                "section_index": symbol["st_shndx"],
            }
        self.calls = []
        section = self.elf.get_section_by_name(".text")
        data, base = section.data(), section["sh_addr"]
        for offset in range(0, len(data) - 3, 4):
            word = struct.unpack_from("<I", data, offset)[0]
            if word & 0xFC000000 in (0x94000000, 0x14000000):
                immediate = word & 0x3FFFFFF
                if immediate & (1 << 25):
                    immediate -= 1 << 26
                address = base + offset
                self.calls.append((address, address + immediate * 4, "bl" if word >> 31 else "b"))

    def file_offset(self, address):
        for p in self.loads:
            if p["p_vaddr"] <= address < p["p_vaddr"] + p["p_filesz"]:
                return p["p_offset"] + address - p["p_vaddr"]
        raise ValueError(f"unmapped file-backed address {address:#x}")

    def code(self, address, size):
        start = self.file_offset(address)
        if size and self.file_offset(address + size - 1) != start + size - 1:
            raise ValueError("range crosses load mapping")
        return self.data[start:start + size]

    def function_at(self, address):
        index = bisect_right(self.starts, address) - 1
        if index >= 0:
            start = self.starts[index]
            if address < start + self.functions[start]:
                return start
        return None

    def disasm(self, address, size=None):
        return list(self.cs.disasm(self.code(address, size or self.functions[address]), address))

    def instruction_record(self, instruction):
        result = {"va": hx(instruction.address), "bytes": instruction.bytes.hex(),
                  "mnemonic": instruction.mnemonic, "operands": instruction.op_str}
        if instruction.mnemonic in ("b", "bl") and instruction.op_str.startswith("#"):
            target = int(instruction.op_str[1:], 16)
            if target in self.plt:
                result["import"] = self.plt[target]
        return result

    def range_record(self, address, size):
        data = self.code(address, size)
        return {"va": hx(address), "file_offset": hx(self.file_offset(address)),
                "size": size, "sha256": sha(data), "bytes": data.hex()}

    def relocation_record(self, address):
        r = self.relocs[address]
        return {"slot_va": hx(address), "slot_file_offset": hx(self.file_offset(address)),
                "raw_bytes": self.code(address, 8).hex(), "symbol": r["symbol"],
                "addend": hx(r["addend"]), "type_number": r["type"],
                "type": {1027: "R_AARCH64_RELATIVE", 257: "R_AARCH64_ABS64"}.get(r["type"], "other")}

    def type_map(self):
        result = []
        for address, r in sorted(self.relocs.items()):
            if not (r["symbol"].startswith("_ZTVN10__cxxabiv1") and "type_info" in r["symbol"]):
                continue
            name_r = self.relocs.get(address + 8)
            if not name_r or name_r["symbol"]:
                continue
            name_bytes = self.code(name_r["addend"], 256).split(b"\0", 1)[0]
            name = name_bytes.decode("ascii")
            if name not in TYPES:
                continue
            vtables = []
            for slot, v in sorted(self.relocs.items()):
                if v["symbol"] or v["addend"] != address or v["type"] != 1027:
                    continue
                entries = []
                cursor = slot + 8
                while cursor in self.relocs:
                    entry = self.relocs[cursor]
                    if entry["symbol"] or entry["addend"] not in self.functions:
                        break
                    entries.append(self.relocation_record(cursor))
                    cursor += 8
                vtables.append({"rtti_reference": self.relocation_record(slot),
                                "preceding_offset_to_top_bytes": self.code(slot - 8, 8).hex(),
                                "entries": entries})
            result.append({"name": name, "name_evidence": self.range_record(name_r["addend"], len(name_bytes) + 1),
                           "typeinfo_va": hx(address),
                           "typeinfo_relocations": [self.relocation_record(address), self.relocation_record(address + 8)],
                           "vtables": vtables})
        if {t["name"] for t in result} != set(TYPES):
            raise ValueError("required GUI RTTI anchors not recovered")
        return result


def build_report(image):
    types = image.type_map()
    for address in FUNCTIONS:
        if address not in image.functions:
            raise ValueError(f"mapped function {address:#x} lacks an unwind FDE")
    snippets = []
    for name, address, size in RANGES:
        instructions = image.disasm(address, size)
        if sum(i.size for i in instructions) != size:
            raise ValueError(f"incomplete disassembly: {name}")
        snippets.append({"name": name, **image.range_record(address, size),
                         "instructions": [image.instruction_record(i) for i in instructions]})
    imported_calls = []
    local_edges = []
    for site, target, mnemonic in image.calls:
        start = image.function_at(site)
        if start not in FUNCTIONS:
            continue
        record = {"site_va": hx(site), "function_va": hx(start), "target_va": hx(target),
                  "branch": mnemonic, "bytes": image.code(site, 4).hex(),
                  "sha256": sha(image.code(site, 4))}
        if target in image.plt:
            imported_calls.append({**record, "import": image.plt[target]})
        elif target in FUNCTIONS:
            local_edges.append(record)
    used_imports = {c["import"] for c in imported_calls}
    dynsym = image.elf.get_section_by_name(".dynsym")
    strings = []
    for address in (0xBD7770, 0xBD7788, 0xB41CA0, 0xB42318, 0xB42300, 0xB42308,
                    0xB42120, 0xB420A8, 0xB42128, 0xB42130, 0xB42138, 0xB42140,
                    0xB42148, 0xB42150, 0xB42158, 0xB42160):
        data = image.code(address, 256).split(b"\0", 1)[0]
        strings.append({"text": data.decode("ascii"), **image.range_record(address, len(data) + 1)})
    return {
        "schema": "rc2fx.offline-gui-audit.v1",
        "scope": "UI-B only; one frame and one input chain; no device I/O, firmware execution or emulation",
        "address_basis": "link-time ELF virtual addresses; apply no UI-A offsets; ASLR base unknown",
        "identity": {"archive_sha256": image.archive_sha256, "member": "/usr/bin/rc_gui_juce",
                     "gui_sha256": image.sha256, "gui_size": len(image.data),
                     "machine": image.elf["e_machine"], "elf_type": image.elf["e_type"],
                     "entry_va": hx(image.elf["e_entry"]),
                     "rotation_member": None if image.rotation is None else {
                         "bytes": image.rotation.hex(), "sha256": sha(image.rotation)}},
        "host_tools": {"capstone": capstone.__version__, "pyelftools": elftools.__version__,
                       "unicorn": "not used"},
        "elf": {"needed": [t.needed for t in image.elf.get_section_by_name(".dynamic").iter_tags()
                            if t.entry.d_tag == "DT_NEEDED"],
                "has_symtab": image.elf.get_section_by_name(".symtab") is not None,
                "dynsym_entries": dynsym.num_symbols(),
                "defined_dynamic_functions": [s.name for s in dynsym.iter_symbols()
                                              if s["st_info"]["type"] == "STT_FUNC" and s["st_shndx"] != "SHN_UNDEF"],
                "unwind_fde_count": len(image.functions)},
        "rtti": types,
        "functions": [{"va": hx(a), "label_inferred": label, "extent_source": ".eh_frame FDE",
                       "size": image.functions[a], "end_exclusive_va": hx(a + image.functions[a]),
                       "file_offset": hx(image.file_offset(a)), "sha256": sha(image.code(a, image.functions[a])),
                       "first_16_bytes": image.code(a, min(16, image.functions[a])).hex()}
                      for a, label in sorted(FUNCTIONS.items())],
        "imports": {name: image.plt_evidence[name] for name in sorted(used_imports)},
        "imported_calls_in_selected_functions": imported_calls,
        "direct_edges_between_selected_functions": local_edges,
        "strings": strings,
        "data_constants": [image.range_record(0xBD7E50, 16), image.range_record(0xB423C0, 32),
                           image.range_record(0xB411D0, 8), image.range_record(0xB303E8, 8)],
        "snippets": snippets,
        "boundary_annotations": [
            {"sites": ["0x7e075c", "0x7e07cc"], "import": "read",
             "observed": "x0 from WindowSystem+8; x1=sp+0x98; x2=0x18; type/code/value read at +16/+18/+20",
             "uapi_interpretation": "one AArch64 Linux input_event, not an array batch; repeated calls drain/dispatch"},
            {"sites": ["0x7cfd4c"], "import": "ioctl", "request": "0xc008561c",
             "observed": "control id 0x00980922; value from rotation argument (fixed path 270)",
             "uapi_interpretation": "VIDIOC_S_CTRL / V4L2_CID_ROTATE"},
            {"sites": ["0x7cfdfc", "0x7cff00", "0x7d02d4"], "import": "ioctl", "request": "0xc0d05605",
             "observed": "BA24=0x34324142; output width*4 stride; capture swaps dimensions for 90 or 270",
             "uapi_interpretation": "VIDIOC_S_FMT, V4L2_PIX_FMT_ARGB32 (not the same naming convention as DRM XR24)"},
            {"sites": ["0x7d0048"], "import": "mmap",
             "observed": "x0=0, x1=queried output length, w2=3, w3=1, w4=video FD, w5=queried offset; result stored at global+0xb8 and returned to pixel-data object",
             "uapi_interpretation": "PROT_READ|PROT_WRITE, MAP_SHARED; establishes mapping, not exclusive DMA ownership"},
            {"sites": ["0x7d05bc", "0x7d0634"], "import": "ioctl", "request": "0xc058560f",
             "observed": "first copy from global+0x60, substitute destination export FD at buffer+0x40; second copy from global+8",
             "uapi_interpretation": "VIDIOC_QBUF: CAPTURE type1 DMABUF memory4, then OUTPUT type2 MMAP memory1"},
            {"sites": ["0x7d0808", "0x7d081c"], "import": "ioctl", "request": "0xc0585611",
             "observed": "capture then output dequeue after positive select; these return codes are not checked by the caller here",
             "uapi_interpretation": "VIDIOC_DQBUF; only successful output dequeue is a candidate source restore boundary"},
            {"sites": ["0x7d7770", "0x7d77a0", "0x7d7834"], "import": "drmIoctl",
             "requests_in_site_order": ["0xc02064b2", "0xc01064b3", "0xc00c642d"],
             "uapi_interpretation": "CREATE_DUMB (bpp32), MAP_DUMB, PRIME_HANDLE_TO_FD (flags0)"},
            {"sites": ["0x7d77c4"], "import": "mmap",
             "observed": "size from CREATE_DUMB; offset from MAP_DUMB; protection3; flags1; result saved at destination+0x18",
             "uapi_interpretation": "shared read/write CPU mapping of a dumb buffer; no ownership conclusion"},
            {"sites": ["0x7d7810"], "import": "drmModeAddFB2",
             "observed": "dimensions from mode globals; format from global+0x130 (XR24=0x34325258); first handles/pitches from CREATE_DUMB; offsets zero; flags0",
             "uapi_interpretation": "single-plane DRM_FORMAT_XRGB8888; byte order/driver acceptance still needs validation"},
            {"sites": ["0x7d0ae4"], "import": "drmModeAtomicCommit",
             "observed": "w0=DRM FD; x1=request; w2=1 or 0x401; x3=0; conversion has returned",
             "uapi_interpretation": "PAGE_FLIP_EVENT, optionally ALLOW_MODESET; not NONBLOCK; not a pointer-bearing CPU-write API"},
        ],
        "interpretations_not_runtime_proofs": {
            "input": [
                "0x7cf9f0 waits on WindowSystem+8 FD and queues closure 0x7e06d0 via local 0x890e80.",
                "0x7e075c and 0x7e07cc read exactly 24 requested bytes per call; not a multi-record read buffer.",
                "Type/code/value loads at record+16/+18/+20 fit AArch64 Linux input_event; code accepts any positive read length.",
                "EV_ABS code 0/0x35 stores WindowSystem+0x44 minus value; code 1/0x36 stores value; output pair is swapped.",
                "EV_KEY 0x110/0x14a contributes press/release state; stock dispatch direct calls include 0x904450/0x904aa0.",
                "This bounded callback is single-pointer-like; no SYN_REPORT/SYN_DROPPED or MT_SLOT/TRACKING_ID handling established.",
                "Filtering must wrap the existing read result, not open/read a second evdev stream, and own the whole contact before stock sees it.",
            ],
            "frame": [
                "Mapped fixed constructor path asks V4L2 for 1280x720, rotation 270, output BA24, stride width*4.",
                "0x7cfca0 uses type2 OUTPUT with MMAP memory1 and type1 CAPTURE with DMABUF memory4 (Linux UAPI interpretation).",
                "Dumb allocations request bpp32; AddFB2 uses mode-derived dimensions, XR24 and returned pitch, not assumed width*4.",
                "PRIME_HANDLE_TO_FD exports each dumb handle; frame loop passes current destination+0x24 to 0x7d0560.",
                "V4L2 capture QBUF 0x7d05bc precedes output QBUF 0x7d0634; success path dequeues capture 0x7d0808 and output 0x7d081c.",
                "ioctl at output QBUF is a credible pre-conversion CPU-source boundary if tracked mmap identity and ownership are later proven.",
                "drmModeAtomicCommit at 0x7d0ae4 is after conversion; it has no CPU buffer pointer and does not itself establish writable ownership.",
                "Commit flags are 1 normally, 0x401 on initial modeset branch; 0x200 NONBLOCK is not set in this chain.",
                "Next iteration waits for DRM select, calls drmHandleEvent, then frees prior atomic request; registered page-flip callback is no-op.",
                "The two destination pointers are alternated after commit; runtime completion, errors, cache coherence and scanout exclusion remain unproven.",
                "Only one output MMAP buffer is requested (count1/type2 at rodata 0xb411d0); two scanout destinations are allocated. No spare scratch/copy path established.",
                "Repaint manager contains DRM thread subobject at +0x50 (constructor stores vptr there); repaint mutex +0x1e0 equals DRM-thread +0x190.",
                "Source save-before-output-QBUF and restore-after-successful-output-DQBUF (0x7d081c) before mutex unlock 0x7d09e0 is a candidate, NOT implemented or ownership-proven.",
            ],
        },
        "unresolved_safety_gates": [
            "No driver/coherency/fence proof for V4L2 MMAP source or PRIME DMABUF destination; no safe DMA ownership claim.",
            "Validate driver-accepted pixel format, pitch, channels, alpha, rotation and bounds, not just requested constants.",
            "Prove full paint/queue/dequeue/reuse ordering under errors/timeouts; vendor success path does not check DQBUF results here.",
            "Persistent source-buffer overlay may contaminate stock partial repaints; hard gate: prove save/restore before next vendor write, a fresh copy, or full repaint on close. No spare buffer established.",
            "A source restore after timeout/failed dequeue could race DMA. A successful-dequeue restore candidate is insufficient for a fail-safe shim.",
            "Host prototype tools/ui_probe/prototype.c only accepts caller-proven CPU-exclusive fresh FULL frames, XR24 rotation0; this mapped dirty/rotated pipeline does not meet that contract.",
            "Input shim must preserve one-reader semantics, FD close/reuse identity, select readiness, errno, cancellation, all contacts and no tap-through.",
            "Stock ignores SYN in this mapped callback; suppressing one record or one read is not complete gesture suppression.",
            "Input quarantine must span actual presentation ACK, not atomic submission; commit userdata is0 and callback is no-op. No per-frame ACK association established.",
            "Buffered evdev across presentation ACK needs a contact snapshot/ordering barrier; this callback does not consume record timestamps as a frame-generation boundary.",
            "Separate explicit approval and recovery plan required before any live loading/injection or GUI lifecycle change.",
        ],
        "next_offline_experiment": "Lift only these bounded GUI functions in a host emulator with mocked imports and synthetic memory: log buffer identities, source save/QBUF/DQBUF/restore, reuse, repaint races and failure paths; model DRM event completion separately from submission, including buffered input ordering across presentation ACK. No real DRM/evdev I/O. Keep separate from the panel/input-policy harness. Device shim remains blocked on ownership/coherency, presentation association and error-path gates.",
    }


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("archive", type=Path)
    parser.add_argument("--out", type=Path, help="new JSON file; existing files are never overwritten")
    args = parser.parse_args(argv)
    try:
        report = build_report(GuiImage(args.archive))
        encoded = json.dumps(report, indent=2, sort_keys=True) + "\n"
        if args.out is None:
            sys.stdout.write(encoded)
        else:
            # No directory creation, extraction, shell, subprocess, native loader or device commands.
            with args.out.open("x", encoding="utf-8") as stream:
                stream.write(encoded)
    except (OSError, ValueError, tarfile.TarError) as error:
        parser.exit(2, f"GUI audit refused: {error}\n")


if __name__ == "__main__":
    main()
