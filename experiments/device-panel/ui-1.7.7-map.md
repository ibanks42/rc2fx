# GUI 1.7.7 frame and input map (offline)

This is the offline investigation that preceded the
[device panel](README.md). It maps the frame and input paths of RØDE's GUI
program in the 1.7.7 beta (called UI-B here) and records what a shim could
and could not safely do at the time. Some of its open questions were later
addressed by the panel implementation. The text below describes the state at
the time of the investigation.

## Summary

A C-import shim looked feasible, but the evidence was not enough to run one
on a device.

- The best place to modify the source buffer is just before
  `ioctl(VIDIOC_QBUF)` at `0x7d0634`. `drmModeAtomicCommit` is too late, as on
  its own it does not identify a writable CPU buffer.
- The matching place to restore the source is after a successful output
  `VIDIOC_DQBUF` at `0x7d081c` and before the repaint mutex is unlocked at
  `0x7d09e0`.
- Neither DMA completion and cache coherency nor safe recovery from failures
  had been shown.
- There is one persistent MMAP paint source, two alternating scanout
  destinations, and no spare buffer or copy path. Because the vendor repaints
  partial regions, any overlay left in the source would ghost, so restoring the
  source is a hard requirement.

Input could be filtered around the GUI's existing imported `read`. A second
evdev reader would not work. The filter has to claim a contact before stock
code sees any part of it, and then suppress the whole gesture. The stock
callback acts before a complete `SYN_REPORT` frame arrives, so passing records
through one at a time is a different behaviour from the host prototype's
complete-contact gate.

The investigation read `AGENTS.md` and `rodecaster-on-device-ui.md`, but every
address below was derived fresh from UI-B bytes, relocations, RTTI, unwind
FDEs and verified PLT/GOT pairs. There was no device access, firmware
execution or emulation, SSH, launch, flash, GUI or audio restart, or change to
existing sources. Only the investigation's own three audit files were
written. No JUCE objects were constructed and no live adapter was built. The
main report was left unchanged.

## Reproducing

From the repository root, with Python 3.11 or newer:

```sh
.deps/duo-investigation/audit-env/bin/python -B -c \
  'import capstone, elftools; print(capstone.__version__, elftools.__version__)'
.deps/duo-investigation/audit-env/bin/python -B -m tools.firmware_audit.gui \
  .deps/downloads/rodecaster-pro-ii-1.7.7-beta-os.tar.gz
# Optional NEW output file (--out deliberately refuses an existing file):
.deps/duo-investigation/audit-env/bin/python -B -m tools.firmware_audit.gui \
  .deps/downloads/rodecaster-pro-ii-1.7.7-beta-os.tar.gz --out /tmp/ui-b-audit.json
```

It was tested with Capstone 5.0.7 and pyelftools 0.33.
`tools/firmware_audit/requirements.txt` pins Capstone 5.0.9, pyelftools 0.33
and Unicorn 2.1.4. This audit does not use Unicorn. Nothing had to be
installed, and no mixer `Image` was reused or modified.

The auditor:

- reads archive members into memory,
- refuses any GUI whose SHA-256 is not
  `aa92a92d384bdb24bc1cdb01615ac280d4d4867e661158df5c7f86ff98b605ca`, before
  any parsing or disassembly,
- checks the ELF architecture,
- verifies each AArch64 PLT stub against its `R_AARCH64_JUMP_SLOT` GOT
  relocation,
- finds functions through `.eh_frame`.

It writes deterministic JSON: tool versions, archive and member identity,
RTTI and vtables, imports and types, every imported call site in the selected
functions, direct edges, function SHA-256s, and the disassembly ranges that
carry the key bytes. Analyst labels for ranges and UAPI interpretations are
kept apart from the observations. The auditor is not a hook installer or a
general GUI analyzer.

The checked file is `/usr/bin/rc_gui_juce`: 19,761,224 bytes, ELF64 LE
AArch64 PIE, entry `0x400f40`, from the archive with SHA-256
`29028734fb2fcc26433bd9f519004c58526c195392ad704826df2fc2909fe57e`. All
addresses are ELF virtual addresses. Live pointers add a load bias, since the
file is `ET_DYN`. File offsets of writable RTTI and vtables are recorded
separately. No offsets were translated from UI-A. `/etc/rotation` in this
archive contains `270\n`.

Verification passed on all of these:

- regeneration is deterministic,
- all 14 function digests match,
- 26 snippets are rebuilt exactly from their instruction bytes,
- all 36 instructions transcribed below match the ELF,
- the UI-A archive is refused with exit code 2,
- an existing `--out` file is left unchanged.

No vendor instructions were executed.

The evidence file is
[`docs/firmware-audits/rodecaster-pro-ii-ui-1.7.7.json`](../../docs/firmware-audits/rodecaster-pro-ii-ui-1.7.7.json).
Its `functions`, `snippets`, `boundary_annotations`, `rtti`, `imports` and
`imported_calls_in_selected_functions` fields are the machine-readable
sources for the citations below.

## RTTI, relocation and unwind anchors

Typeinfo and vtable RTTI-reference slots (all virtual addresses):

| RTTI name suffix | Typeinfo | Name bytes | Vtable RTTI slot | Following function targets |
| --- | --- | --- | --- | --- |
| `RcInputThreadE` | `0x12e04e0` | `0xb3f240` | `0x12b24d8` | `0x7d7020`, `0x7cf8c0`, **`0x7cf9f0`** |
| `RcRepaintManagerE` | `0x12e04f8` | `0xb3f270` | `0x12b2500` | `0x7d6f60`, `0x7d71f0`, **`0x7cf5d0`** |
| `RodecasterDRMThreadE` | `0x12e0560` | `0xb3f328` | `0x12b21b0` | `0x758d90`, `0x758de0`, **`0x7d08a0`** |
| `RodecasterWindowSystemE` | `0x12e1fa8` | `0xb423e8` | `0x12b7e70` | `0x7d0cc0`, `0x7d0d40` |

The typeinfo objects carry named `__cxxabiv1` type-info vtable relocations.
Their name pointers and the listed vtable pointers are relative relocations.
Reading the first two targets as destructor variants and the third as the
`run` or timer callback is an inference from Itanium C++ conventions and from
the function bodies. The method names were not recovered.

The WindowSystem accessor and lazy constructor, found separately, is
`0x92e030`. It installs vptr `0x12b7e78` at `0x92e100`. The RTTI for
`V4L2PixelData` (`0x12e1fd8`) and `V4L2ImageType` (`0x12e2000`) ties the
bitmap helper `0x7cf020` and the image allocator `0x7d0480` to the same
initialization chain.

Whole-function identity checks (end address exclusive):

| Function VA range | SHA-256 |
| --- | --- |
| `0x7cf5d0–0x7cf8b8` repaint | `186cc32f8ba080846d5ef502512a426d0c338532609fc91e6fb77414fd1cca9e` |
| `0x7cf9f0–0x7cfc60` input wait | `02be87d0ba9e6daa232d5e8b34dfd9dc27c494900f2508a478c2836e71cf9bdb` |
| `0x7d08a0–0x7d0cb4` frame loop | `f99908ceb417789a8df395bf41ec756bc138b39f43158910c3b372639491fa43` |
| `0x7e06d0–0x7e0a44` input closure | `62506ce673557a42288a55b532145b3cd5b00cf4fefc1e8edb3c8252f60e3339` |
| `0x7d0560–0x7d089c` conversion | `d095d02c113414c214210285f82a24d15929c4af22088083a5af9bc1f5ad5855` |
| `0x7d7710–0x7d78f0` DRM allocation | `da30acda0d63b9967885c605e8c7691b083901a84b8cb87f5da919553241579a` |

These digests identify function bytes. They do not mark approved patch sites.
The binary uses PAC and BTI. The JSON includes the four bytes and SHA-256 of
each imported branch, and the bytes and digest of each whole snippet.

## Frame path

### Allocation and formats

These are the requests the code makes. The values the driver returns at run
time could not be observed offline.

1. Window initialization opens `/dev/dri/card0` at `0x92e170`, negotiates the
   atomic capability at `0x92e1cc`, and copies the connector mode's width and
   height into backend globals at `0x92e398` (the mode structure is loaded at
   `+4` and `+0xe`). It writes `0x34325258` (`XR24`) at `0x92e74c` and then
   calls the dumb-buffer allocator `0x7d7710` twice, at `0x92e7a4` and
   `0x92e7b4`, for the descriptors `0x12ec5b8` and `0x12ec5e0`, which are
   0x28 bytes apart. This initialization allocates no third or spare buffer.
2. The allocator calls `drmIoctl` for CREATE_DUMB `0xc02064b2` at `0x7d7770`
   and MAP_DUMB `0xc01064b3` at `0x7d77a0`, then `mmap` at `0x7d77c4` with
   protection 3, flags 1, and the returned size and offset. It requests 32
   bits per pixel. `drmModeAddFB2` at `0x7d7810` gets the mode dimensions,
   XR24, the returned handle and pitch, and zero offsets and flags. The pitch
   is taken from the driver and may differ from width×4. PRIME_HANDLE_TO_FD
   `0xc00c642d` at `0x7d7834` exports the handle with flags 0. The code stores
   the CPU mapping at descriptor `+0x18`, the FB ID at `+0x20` and the export
   FD at `+0x24`. These are observed accesses. They are not a proposed C
   struct.
3. Peer construction at `0x7dc9d4–0x7dc9ec` calls the V4L2 initializer
   `0x7cfca0` with 1280×720, rotation 270, and a clear-source flag of 1. The
   initializer opens `/dev/video0` at `0x7cfcec`, queries capabilities at
   `0x7cfd18`, sends control ID `0x00980922` at `0x7cfd4c`, and sets the
   format BA24 (`0x34324142`) for OUTPUT at `0x7cfdfc` and CAPTURE at
   `0x7cff00`. That is the 90/270 branch. The non-swapped branch is at
   `0x7d02d4`. The requested OUTPUT stride is width×4 and is checked. The
   CAPTURE dimensions are swapped for 90 and 270.
4. Decoded through the UAPI headers, the rotation control is
   `V4L2_CID_ROTATE`. BA24 is `V4L2_PIX_FMT_ARGB32` and DRM XR24 is
   `DRM_FORMAT_XRGB8888`. The two naming schemes differ, so the host
   prototype's synthetic XR24 bytes cannot be used for this V4L2 source
   without checking channel order and alpha. On success, initialization checks
   the dimensions, format and stride the driver returns.
5. One OUTPUT buffer is requested. Rodata `0xb411d0` holds count 1 and type 2,
   and `VIDIOC_REQBUFS` at `0x7cff88` uses memory type 1 (MMAP). The CAPTURE
   request at `0x7cffd4` uses count 1, type 1 and memory type 4 (DMABUF).
   OUTPUT QUERYBUF at `0x7d0028` gets the size and offset, and `mmap` at
   `0x7d0048` produces the persistent source pointer, returned at `0x7d0074`.
   The bitmap helper `0x7cf020` computes
   `base + y*stride + x*pixelStride` from loads at pixel-data `+0x38`, `+0x40`
   and `+0x44`.

### Paint, convert, commit and reuse

The repaint callback `0x7cf5d0` walks a list of 16-byte rectangles and paints
only when the list is non-empty and the size is positive. It locks the
manager at `+0x1e0` at `0x7cf7ac`, calls the indirect renderer at `0x7cf808`
and another paint helper at `0x7cf814`, unlocks at `0x7cf81c` and notifies at
`0x7cf824`. This is dirty-region rendering. It gives no guarantee that the
source holds a freshly cleared full frame.

The constructor installs the DRM-thread vptr at manager `+0x50`
(`0x7dc944`). In this construction, the DRM-thread mutex at `+0x190` and the
repaint mutex at `+0x1e0` are therefore the same mutex.

The frame loop `0x7d08a0` waits, then drains the previous DRM event if there
was a previous atomic request. It loads the current destination from the
global `0x12ec610`, locks the same mutex at `0x7d09d0`, passes destination
`+0x24` (the export FD) at `0x7d09d4`, and calls the conversion helper
`0x7d0560` at `0x7d09d8`.

The key imported-call instructions (hex bytes as observed in the file):

```text
7d05b0 e4a300b9 str w4, [sp,#0xa0]   ; supplied destination FD -> capture buffer+0x40
7d05bc d1cee497 bl  0x104100          ; ioctl, request 0xc058560f (capture QBUF)
7d0600 e1c18ad2 mov x1,#0x560f
7d0604 010bb8f2 movk x1,#0xc058,lsl#16
7d060c e20315aa mov x2,x21            ; local copy of OUTPUT MMAP descriptor
7d0634 b3cee497 bl  0x104100          ; ioctl, output QBUF: candidate patch BEFORE call
7d06bc 49cee497 bl  0x103fe0          ; select(video fd), timeval {2,0}
7d0808 3ecee497 bl  0x104100          ; ioctl capture DQBUF, request 0xc0585611
7d081c 39cee497 bl  0x104100          ; ioctl output DQBUF: candidate restore AFTER success
7d09dc e0031baa mov x0,x27
7d09e0 d8cbe497 bl  0x103940          ; pthread_mutex_unlock after conversion helper returns
```

At `0x7d0634` the source has not been queued yet, so a wrapper could track its
MMAP identity and wrap that single original call. After a successful source
dequeue, a wrapper could restore a saved region before the vendor repaints
it. That points to a bounded experiment: save, patch, convert, restore. It
does not mean writing to the source was safe at this stage.

The vendor code does not check dequeue return codes on the normal path where
`select` reports ready. The timeout and error paths can return without both
dequeues, and restoring in those cases could race the DMA. The caller unlocks
the mutex even when the helper exits through one of those errors.

After conversion, the frame loop allocates an atomic request and sets `FB_ID`
from destination `+0x20`. It builds `CRTC_ID`, `SRC_X/Y/W/H` (16.16
dimensions), `CRTC_X/Y/W/H`, and the initial mode and active properties
through the helpers `0x93ce40` and `0x93cf40`. The commit and wait sequence:

```text
7d0a24 38008052 mov w24,#1
7d0c00 38808052 mov w24,#0x401        ; initial modeset branch only
7d0ad8 e203182a mov w2,w24
7d0adc 61c640f9 ldr x1,[x19,#0x188]   ; atomic request, NOT pixels
7d0ae0 030080d2 mov x3,#0
7d0ae4 d3cde497 bl 0x104230           ; drmModeAtomicCommit
7d0984 97cde497 bl 0x103fe0           ; next iteration: select DRM fd, timeout NULL
7d0c24 b7c9e497 bl 0x103300           ; drmHandleEvent
7d09c4 77cae497 bl 0x1033a0           ; drmModeAtomicFree
```

The flags are `PAGE_FLIP_EVENT` (1), plus `ALLOW_MODESET` (0x400) on the
first modeset. `NONBLOCK` (0x200) is not set. The callback registered at
`0x92e75c` is `0x7cf230`, which is only `bti c; ret`
(`5f2403d5c0035fd6`). So the loop waits for and handles events, but the
callback does no per-buffer completion accounting. After the commit the two
destination pointers are swapped through the globals `0x12ec608` and
`0x12ec610`. On the first pass the fallback picks the second descriptor.

### Presentation and input gating

Input quarantine has to last until the frame is actually presented. Commit
submission or return is too early. This chain passes commit user data 0 and
a page-flip callback that does nothing. The `select` and `drmHandleEvent`
calls show that the loop waits for an event, but there is no frame-tagged
acknowledgement, and nothing linked a panel generation to its completed
presentation.

Evdev records already buffered across that acknowledgement also need a
contact snapshot or ordering barrier. Opening input as soon as a commit
function returns is not enough. The mapped input callback uses event type,
code and value, and does not use the record's timeval as a
presentation-generation boundary. Closing the panel likewise needs the display
retired and whole contacts drained before stock resumes.

### Scanout ownership

An atomic commit does not give safe ownership of a writable scanout buffer.
The request holds IDs and properties with no CPU pointers, and conversion has
already happened by then. A shim would need to associate mapping, FB and
export FD, and prove completion and coherency.

The source is persistent and partly repainted, so patching it and leaving the
panel there risks ghosts after close. Writing only to a destination might
keep the source clean, but it had not been shown that each later conversion
fully overwrites the destination, because driver crop, format, completion
and error behaviour were unknown. Both destinations are already part of
scanout reuse, so neither is a spare. The bounded chain contains no full-frame
copy and no callback that restores before the vendor writes. The window after
a successful output dequeue was the best candidate for restoring the source,
but it did not settle the requirement.

## Input path

Window initialization opens `/dev/input/event0` at `0x92e150`, stores the FD
at WindowSystem `+8` with flags `0x902`, then sets flags `0x2800` through
`fcntl` at `0x92e160`. Peer construction separately opens
`/dev/input/event1` at `0x7dcbec` and stores it at `+0xc`. That device is
used to upload and write haptics. A filter must leave other evdev FDs alone
and must not treat event1 as the touchscreen.

The `RcInputThread` target `0x7cf9f0` loads `+8`, waits in `select` at
`0x7cfac8` (timeout constant `{0,500000}` at `0xb423c0`), then builds the
closure invoker `0x7e06d0` and calls the local dispatch helper `0x890e80` at
`0x7cfb4c`. A later local wait uses 1000 at `0x7cfb6c`. That this is
scheduled on the message thread is an inference from the closure and JUCE
context. There is no recovered public ABI for it.

```text
7e0750 a00a40b9 ldr w0,[x21,#8]       ; same WindowSystem FD
7e0754 e10317aa mov x1,x23            ; x23 = sp+0x98
7e0758 020380d2 mov x2,#0x18
7e075c 058ce497 bl 0x103770           ; read: one requested 24-byte record
7e0778 e3534179 ldrh w3,[sp,#0xa8]    ; record+16: type
7e0784 7f0c0071 cmp w3,#3             ; EV_ABS
7e0828 e1574179 ldrh w1,[sp,#0xaa]    ; record+18: code
7e0834 3fd40071 cmp w1,#0x35          ; accepts ABS_X=0 or MT_POSITION_X=0x35
7e0844 e1af40b9 ldr w1,[sp,#0xac]     ; record+20: value
7e084c a04640b9 ldr w0,[x21,#0x44]
7e0850 0000014b sub w0,w0,w1
7e0854 200300b9 str w0,[x25]          ; store height - rawX
7e088c 3fd80071 cmp w1,#0x36          ; accepts ABS_Y=1 or MT_POSITION_Y=0x36
7e08ac 200700b9 str w0,[x25,#4]       ; store rawY
7e0a10 24900494 bl 0x904aa0           ; direct stock dispatch, not an imported C function
7e0a3c 19900494 bl 0x904aa0           ; alternate stock dispatch
```

A second `read` at `0x7e07cc` also asks for 24 bytes. The loops read one
record at a time. The record matches the AArch64 Linux `input_event` layout:
two 64-bit timeval words, then u16, u16 and s32. It is unrelated to any JUCE
layout. The code checks only that the read length is positive and does not
require exactly 24, so a wrapper must not return made-up short records.

EV_KEY handling recognizes `0x110` and `0x14a` (BTN_LEFT and BTN_TOUCH).
Release eventually calls the stock helper `0x904450` at `0x7e0938`. EV_REL is
logged. Other event types, EV_SYN included, go back to the read loop in this
function. Dispatch can happen once enough X, Y and key state has arrived,
before `SYN_REPORT`.

The stored coordinates come out in reversed order, roughly
`logical=(rawY,720-rawX)` on the fixed initial-dimensions path. Bounds are
compared against 1279 and 719 at `0x7e0804–0x7e0808`. The subtraction uses
720, while the bound is 719. This is an observation only. It is neither a
calibration nor evidence of behaviour at the screen edges. The callback does not implement MT
slots, tracking IDs or `SYN_DROPPED` recovery, so it is not a complete
multitouch protocol.

### Filtering

The imported `read` can serve as an observation and filter point for one
original stock read, provided it is scoped to the exact GUI identity, the FD
identity and lifetime, the call site, and the owning thread and closure.
Reading a second FD or stream would neither suppress stock input nor settle
ownership. Reading extra records from the same FD would take input away from
stock.

Ownership of a whole contact has to be decided before its first visible
button or axis record is returned to stock. It must then be kept across reads
and frames, and drained through the release on close or error. That may need
buffering that preserves readiness, `errno`/`EAGAIN` and the closure's wake
handshake. Returning zero or `EAGAIN` carelessly, or treating `select`
readiness as a complete gesture, would break it. The investigation produced
no raw cancellation encoding and no ready-made adapter.

## Imported C functions and type limits

All of the following are undefined `STT_FUNC`, `STB_GLOBAL` dynamic symbols,
reached through verified PLT/GOT `R_AARCH64_JUMP_SLOT` relocations. These are
symbol categories. No C prototypes or debug types were recovered, and the
interpretations above come from the public Linux and libdrm APIs.

| Import | PLT | Relevant sites |
| --- | --- | --- |
| `read` | `0x103770` | `0x7e075c`, `0x7e07cc` |
| `select` | `0x103fe0` | `0x7cfac8` input, `0x7d06bc` video, `0x7d0984` DRM |
| `ioctl` | `0x104100` | V4L2 init `0x7cfd18` onward; QBUF `0x7d05bc/0x7d0634`; DQBUF `0x7d0808/0x7d081c` |
| `mmap` | `0x1030d0` | `0x7d0048` video source, `0x7d77c4` DRM destination |
| `drmIoctl` | `0x1030a0` | `0x7d7770/0x7d77a0/0x7d7834` |
| `drmModeAddFB2` | `0x1030f0` | `0x7d7810` |
| `drmModeAtomicCommit` | `0x104230` | `0x7d0ae4` |
| `drmHandleEvent` | `0x103300` | `0x7d0c24` |

The JSON lists every relevant DRM discovery, property and free call, with GOT
addresses, request immediates, relocation kinds, instruction bytes and
function digests. The binary has no defined dynamic function exports and no
`.symtab`, so a preload cannot replace local direct JUCE calls by naming a C++
symbol.

The UAPI interpretations were cross-checked against the host's headers:
`/usr/include/linux/{videodev2.h,v4l2-controls.h,input-event-codes.h}` and
`/usr/include/drm/{drm_mode.h,drm_fourcc.h}`. That covers the QBUF and DQBUF
request numbers and `v4l2_buffer` fields, OUTPUT=2 and CAPTURE=1, MMAP=1 and
DMABUF=4, BA24/ARGB32, ROTATE=`0x00980922`, DRM XR24/XRGB8888 and the atomic
flags. These stable constants explain the bytes. They do not identify the
vendor's kernel implementation or show how it handles DMA and caches.

## Next steps and open requirements

At the time, the plan was not to connect this directly to the host
prototype (`tools/ui_probe/prototype.c`, since removed from the tree). That
prototype required a CPU-exclusive, fresh, full XR24 frame at rotation 0. A
persistent, dirty-painted, rotated BA24 V4L2 source does not meet that, and it
had not been established for the two reused DRM targets either. The
prototype's whole-contact gate also had no adapter for raw evdev
cancellation. The separate host harness for the panel and input ownership was
meant to stay hardware-free and was not duplicated here.

The proposed next step was a small offline experiment on lifted functions.
A broader reverse-engineering pass over the UI, menus or IPC was not needed.
It would run only the mapped bitmap, repaint and conversion control flow in
host Unicorn, against synthetic memory and mocked imports, and would:

- track one source and two destinations, and every source write,
- save before QBUF and restore after a successful DQBUF,
- cover pointer reuse, errors and timeouts, and the repaint locks,
- model completion events separately from commit submission,
- model queued input that crosses a presentation acknowledgement, and work
  out what frame and contact snapshot would be needed before releasing
  quarantine.

It would use the byte- and digest-qualified functions from the evidence file.
It would test control flow and the mechanics of ghost-free restoration with
no real DRM or evdev I/O. It would not settle real driver coherency and would
not authorize injecting anything on hardware.

Requirements for a device shim that were still open:

- a demonstrated DMA completion, cache and fence contract,
- the pitch, channel order, alpha and rotation the driver actually accepts,
- restore and copy ownership through failed queue, dequeue, select and commit
  paths,
- complete raw contact ownership and cancellation with no tap-through, FD
  reuse, and wake and `errno` semantics,
- associating each frame with its presentation acknowledgement, and
  snapshotting and ordering buffered input,
- bounded latency,
- separately approved launch and recovery integration.

Having an atomic commit and two mmap pointers meets none of these.
