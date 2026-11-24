# GUI 1.7.7 frame and input map (offline)

This offline reverse-engineering note preceded the [device panel](README.md). It maps the frame and touch-input paths of RØDE's 1.7.7 beta GUI binary (referred to as UI-B here) and documents what a C-import shim could and couldn't safely do at the time. Several open questions listed here were later solved in the panel implementation; the write-up below reflects what was known during the initial audit.

## Summary

A C-import shim looked viable on paper, though offline evidence alone wasn't enough to run it on hardware:

- The cleanest hook point for modifying the source buffer is right before `ioctl(VIDIOC_QBUF)` at `0x7d0634`. `drmModeAtomicCommit` is too late and doesn't expose a writable CPU buffer on its own.
- The matching restore point is after a successful OUTPUT `VIDIOC_DQBUF` at `0x7d081c` and before unlocking the repaint mutex at `0x7d09e0`.
- DMA completion, cache coherency, and safe failure recovery were still unproven offline.
- The GUI uses a single persistent MMAP paint source and two alternating scanout destinations, with no spare buffer or copy path. Because JUCE repaints dirty regions incrementally, leaving overlay pixels in the source causes ghosting, so restoring the source is mandatory.

Touch input could be intercepted around the GUI's imported `read` (opening a second evdev reader won't work). Any filter has to claim a contact before stock code sees any of its events and suppress the entire gesture. Since the stock callback dispatches before a full `SYN_REPORT` frame arrives, passing single records through behaves differently from the host prototype's complete-contact gate.

Although `AGENTS.md` and `rodecaster-on-device-ui.md` were consulted for context, every address below was derived directly from UI-B's bytes, relocations, RTTI, unwind FDEs, and verified PLT/GOT entries. The audit ran offline and wrote only the three audit files.

## Reproducing

From the repo root with Python 3.11+:

```sh
.deps/duo-investigation/audit-env/bin/python -B -c \
  'import capstone, elftools; print(capstone.__version__, elftools.__version__)'
.deps/duo-investigation/audit-env/bin/python -B -m tools.firmware_audit.gui \
  .deps/downloads/rodecaster-pro-ii-1.7.7-beta-os.tar.gz
# Optional NEW output file (--out deliberately refuses an existing file):
.deps/duo-investigation/audit-env/bin/python -B -m tools.firmware_audit.gui \
  .deps/downloads/rodecaster-pro-ii-1.7.7-beta-os.tar.gz --out /tmp/ui-b-audit.json
```

Tested with Capstone 5.0.7 and pyelftools 0.33 (`tools/firmware_audit/requirements.txt` pins Capstone 5.0.9, pyelftools 0.33, and Unicorn 2.1.4, though this audit doesn't use Unicorn).

What the auditor does:

- reads archive members directly in memory,
- rejects any GUI binary whose SHA-256 isn't `aa92a92d384bdb24bc1cdb01615ac280d4d4867e661158df5c7f86ff98b605ca` before parsing or disassembling anything,
- checks ELF architecture,
- cross-checks every AArch64 PLT stub against its `R_AARCH64_JUMP_SLOT` GOT relocation,
- locates function boundaries via `.eh_frame`.

It outputs deterministic JSON containing tool versions, archive/member metadata, RTTI and vtables, imports, every imported call site inside the selected functions, direct call edges, function SHA-256 hashes, and disassembly snippets, keeping analyst labels separate from raw observations. It is an offline auditor.

Target binary: `/usr/bin/rc_gui_juce` (19,761,224 bytes, ELF64 LE AArch64 PIE, entry `0x400f40`), from archive SHA-256 `29028734fb2fcc26433bd9f519004c58526c195392ad704826df2fc2909fe57e`. All addresses below are ELF virtual addresses (live pointers add the PIE load bias). File offsets for writable RTTI/vtables are tracked separately, and no offsets were ported over from UI-A. `/etc/rotation` in this archive is `270\n`.

Sanity checks verified:

- output is deterministic across runs,
- all 14 function hashes match,
- all 26 snippets round-trip cleanly from their instruction bytes,
- all 36 instructions transcribed below match the ELF,
- the UI-A archive is rejected with exit code 2,
- an existing `--out` file is never overwritten.

No vendor code was executed. Machine-readable evidence lives in [`docs/firmware-audits/rodecaster-pro-ii-ui-1.7.7.json`](../../docs/firmware-audits/rodecaster-pro-ii-ui-1.7.7.json) under `functions`, `snippets`, `boundary_annotations`, `rtti`, `imports`, and `imported_calls_in_selected_functions`.

## RTTI, relocation, and unwind anchors

Typeinfo and vtable RTTI-reference slots (virtual addresses):

| RTTI name suffix | Typeinfo | Name bytes | Vtable RTTI slot | Following function targets |
| --- | --- | --- | --- | --- |
| `RcInputThreadE` | `0x12e04e0` | `0xb3f240` | `0x12b24d8` | `0x7d7020`, `0x7cf8c0`, **`0x7cf9f0`** |
| `RcRepaintManagerE` | `0x12e04f8` | `0xb3f270` | `0x12b2500` | `0x7d6f60`, `0x7d71f0`, **`0x7cf5d0`** |
| `RodecasterDRMThreadE` | `0x12e0560` | `0xb3f328` | `0x12b21b0` | `0x758d90`, `0x758de0`, **`0x7d08a0`** |
| `RodecasterWindowSystemE` | `0x12e1fa8` | `0xb423e8` | `0x12b7e70` | `0x7d0cc0`, `0x7d0d40` |

Typeinfo objects have named `__cxxabiv1` vtable relocations, while their name and vtable pointers use relative relocations. Treating the first two slots as destructors and the third as `run` or a timer callback follows Itanium C++ ABI conventions and the disassembled bodies (exact method names are stripped).

Separately, `0x92e030` is the `WindowSystem` accessor/lazy constructor, writing vptr `0x12b7e78` at `0x92e100`. RTTI for `V4L2PixelData` (`0x12e1fd8`) and `V4L2ImageType` (`0x12e2000`) ties bitmap helper `0x7cf020` and image allocator `0x7d0480` into the same init path.

Whole-function digests (end address exclusive):

| Function VA range | SHA-256 |
| --- | --- |
| `0x7cf5d0–0x7cf8b8` repaint | `186cc32f8ba080846d5ef502512a426d0c338532609fc91e6fb77414fd1cca9e` |
| `0x7cf9f0–0x7cfc60` input wait | `02be87d0ba9e6daa232d5e8b34dfd9dc27c494900f2508a478c2836e71cf9bdb` |
| `0x7d08a0–0x7d0cb4` frame loop | `f99908ceb417789a8df395bf41ec756bc138b39f43158910c3b372639491fa43` |
| `0x7e06d0–0x7e0a44` input closure | `62506ce673557a42288a55b532145b3cd5b00cf4fefc1e8edb3c8252f60e3339` |
| `0x7d0560–0x7d089c` conversion | `d095d02c113414c214210285f82a24d15929c4af22088083a5af9bc1f5ad5855` |
| `0x7d7710–0x7d78f0` DRM allocation | `da30acda0d63b9967885c605e8c7691b083901a84b8cb87f5da919553241579a` |

These hashes fingerprint the function bytes (they aren't a list of patch sites). The binary uses PAC and BTI; the JSON also records the 4-byte instruction and SHA-256 for every imported branch and snippet.

## Frame path

### Allocation and formats

These reflect what the binary requests; actual driver return values couldn't be seen offline.

1. Window init opens `/dev/dri/card0` (`0x92e170`), negotiates atomic mode-setting (`0x92e1cc`), and copies the connector mode's width/height into globals at `0x92e398` (loaded from mode `+4` and `+0xe`). It writes `0x34325258` (`XR24`) at `0x92e74c` and calls dumb-buffer allocator `0x7d7710` twice (`0x92e7a4`, `0x92e7b4`) for descriptors `0x12ec5b8` and `0x12ec5e0` (0x28 bytes apart). No third buffer is allocated.
2. The allocator calls `drmIoctl` for `CREATE_DUMB` (`0xc02064b2` at `0x7d7770`) and `MAP_DUMB` (`0xc01064b3` at `0x7d77a0`), then `mmap`s it (`0x7d77c4`, prot 3, flags 1) at 32 bpp. `drmModeAddFB2` (`0x7d7810`) registers the framebuffer with mode dimensions, `XR24`, handle, driver-returned pitch (which may differ from `width*4`), and zero offsets/flags. `PRIME_HANDLE_TO_FD` (`0xc00c642d` at `0x7d7834`, flags 0) exports the handle. Observed descriptor offsets: CPU mapping at `+0x18`, FB ID at `+0x20`, export FD at `+0x24`.
3. Peer construction (`0x7dc9d4–0x7dc9ec`) calls V4L2 init `0x7cfca0` with 1280×720, rotation 270, and clear-source = 1. It opens `/dev/video0` (`0x7cfcec`), queries capabilities (`0x7cfd18`), sets control `0x00980922` (`0x7cfd4c`), and configures `BA24` (`0x34324142`) on both OUTPUT (`0x7cfdfc`) and CAPTURE (`0x7cff00`) in the 90/270 branch (non-swapped branch at `0x7d02d4`). OUTPUT stride is checked against `width*4`, and CAPTURE dimensions are swapped for 90/270.
4. Cross-checking Linux UAPI headers: control `0x00980922` is `V4L2_CID_ROTATE`, `BA24` is `V4L2_PIX_FMT_ARGB32`, and DRM `XR24` is `DRM_FORMAT_XRGB8888`. Because V4L2 and DRM pixel format naming don't match 1:1, the host prototype's synthetic XR24 pixels can't be assumed valid for this V4L2 source without verifying channel order and alpha. Init verifies the driver-returned dimensions, format, and stride.
5. One OUTPUT buffer is requested (`0xb411d0` holds count 1, type 2; `VIDIOC_REQBUFS` at `0x7cff88` uses memory type 1 / `MMAP`), and one CAPTURE buffer (`0x7cffd4`: count 1, type 1, memory type 4 / `DMABUF`). OUTPUT `QUERYBUF` (`0x7d0028`) + `mmap` (`0x7d0048`) yields the persistent source pointer returned at `0x7d0074`. Bitmap helper `0x7cf020` indexes `base + y*stride + x*pixelStride` from pixel-data offsets `+0x38`, `+0x40`, and `+0x44`.

### Paint, convert, commit, and reuse

Repaint callback `0x7cf5d0` iterates a list of 16-byte dirty rects and skips painting if the list is empty or zero-sized. It locks the manager mutex at `+0x1e0` (`0x7cf7ac`), invokes the indirect renderer (`0x7cf808`) and a second paint helper (`0x7cf814`), unlocks (`0x7cf81c`), and signals (`0x7cf824`). Because this is incremental dirty-region rendering, the source buffer isn't cleared before each frame.

The constructor installs the DRM-thread vptr at manager `+0x50` (`0x7dc944`), meaning the DRM-thread mutex at `+0x190` and the repaint mutex at `+0x1e0` are the exact same mutex.

Frame loop `0x7d08a0` waits, drains the prior DRM event if an atomic commit was pending, loads the active destination from `0x12ec610`, locks that shared mutex (`0x7d09d0`), passes destination `+0x24` (export FD) at `0x7d09d4`, and calls conversion helper `0x7d0560` (`0x7d09d8`).

Key imported calls inside conversion and unlock:

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

At `0x7d0634` the source buffer isn't queued yet, so a wrapper could save a region, overlay onto it, let conversion run, and restore it after a successful OUTPUT `DQBUF`, before JUCE can repaint. Note that vendor code skips checking `DQBUF` return values on the normal path after `select` reports ready, and timeout/error branches can return without dequeuing both buffers (while the caller still unlocks the mutex), so restoring blindly on error paths would race DMA.

After conversion, the loop builds a DRM atomic request setting `FB_ID` (from destination `+0x20`), `CRTC_ID`, `SRC_X/Y/W/H` (16.16 fixed-point), `CRTC_X/Y/W/H`, and initial mode/active properties via `0x93ce40` and `0x93cf40`:

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

Flags are `PAGE_FLIP_EVENT` (`1`), plus `ALLOW_MODESET` (`0x400`) on the first pass (`NONBLOCK` / `0x200` is unused). The page-flip callback registered at `0x92e75c` is `0x7cf230`, which is just `bti c; ret` (`5f2403d5c0035fd6`), so the loop waits for events but does no per-buffer completion tracking. After commit, globals `0x12ec608` and `0x12ec610` swap the two destination pointers (falling back to the second descriptor on pass one).

### Presentation and input gating

Input quarantine can't lift when `drmModeAtomicCommit` returns; it has to wait until the frame is actually presented. Stock code passes `0` as commit user data and uses a no-op flip handler, so nothing tied a panel generation to its completed flip.

Buffered evdev events crossing that flip boundary also need a contact snapshot or ordering barrier; the stock input callback only looks at event type, code, and value and ignores timestamps. Closing the panel has the mirror requirement: retire the overlay and drain active contacts before handing touch back to stock.

### Scanout ownership

An atomic commit request only carries IDs and properties (no CPU pointers), and hardware conversion has already finished by then. Writing directly to a scanout destination would avoid dirtying the source, except both destinations are actively reused for scanout (no spare exists) and offline analysis couldn't prove every subsequent conversion completely overwrites the destination. Restoring the persistent source after a successful OUTPUT `DQBUF` looked like the cleanest option, though still needing proof on error paths and DMA coherency.

## Input path

Window init opens `/dev/input/event0` (`0x92e150`), stores the FD at `WindowSystem+8` (flags `0x902`), and sets `0x2800` via `fcntl` (`0x92e160`). Peer construction opens `/dev/input/event1` (`0x7dcbec`, stored at `+0xc`) for haptics, so any touchscreen filter must leave `event1` and other FDs alone.

`RcInputThread` (`0x7cf9f0`) reads `+8`, waits in `select` (`0x7cfac8`, timeout `{0, 500000}` at `0xb423c0`), builds closure invoker `0x7e06d0`, and posts it via local dispatcher `0x890e80` (`0x7cfb4c`), followed by a local wait of 1000 (`0x7cfb6c`). That looks like a JUCE message-thread dispatch, though there's no recovered public ABI for it.

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

A second `read` at `0x7e07cc` also reads 24 bytes: standard AArch64 Linux `struct input_event` (two 64-bit timeval words, `u16`, `u16`, `s32`). The loop only checks `ret > 0`, not `ret == 24`, so a shim must never synthesize partial records.

`EV_KEY` handles `0x110` (`BTN_LEFT`) and `0x14a` (`BTN_TOUCH`), releasing through `0x904450` at `0x7e0938`. `EV_REL` is logged, and other types (including `EV_SYN`) simply loop back to `read`. Stock dispatch can fire as soon as X, Y, and key state are populated, even before `SYN_REPORT`.

Coordinates are swapped and inverted to roughly `logical = (rawY, 720 - rawX)` on the default path, with bounds checked against 1279 and 719 at `0x7e0804–0x7e0808` (note the 720 vs. 719 asymmetry, observed in code and not calibrated on hardware). There's no handling for MT slots, tracking IDs, or `SYN_DROPPED`.

### Filtering

Wrapping imported `read` works as an observation/filter hook if scoped tightly to the verified GUI binary, touchscreen FD lifetime, call site, and owning thread/closure. Opening a second FD won't suppress stock input, and draining extra records from the shared FD would steal them from stock.

Whole-contact ownership has to be decided before the first visible button or axis record leaks to stock, held across reads/frames, and drained through release on close or error. Doing that cleanly requires buffering that preserves `select` readiness, `errno`/`EAGAIN`, and the closure's wake handshake; sloppy `0`/`EAGAIN` returns or assuming `select` means a complete gesture will break it.

## Imported C functions and type limits

All symbols below are undefined `STT_FUNC` / `STB_GLOBAL` imports reached through verified `R_AARCH64_JUMP_SLOT` PLT/GOT entries (no debug types or C headers are present in the binary; signatures come from Linux/libdrm APIs):

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

The JSON file records every relevant DRM discovery/property/free call along with GOT addresses, request immediates, relocation kinds, instruction bytes, and function digests. Because the binary has no `.symtab` and exports no dynamic functions, `LD_PRELOAD` can't override internal JUCE C++ calls by symbol name.

UAPI constants were verified against `/usr/include/linux/{videodev2.h,v4l2-controls.h,input-event-codes.h}` and `/usr/include/drm/{drm_mode.h,drm_fourcc.h}` (covering `QBUF`/`DQBUF`, `v4l2_buffer`, `OUTPUT=2`, `CAPTURE=1`, `MMAP=1`, `DMABUF=4`, `BA24`/`ARGB32`, `ROTATE=0x00980922`, `XR24`/`XRGB8888`, and atomic flags). Those explain the userspace requests, though they don't reveal vendor kernel DMA or cache behavior.

## Next steps and open requirements

At this stage, wiring directly into the old host prototype (`tools/ui_probe/prototype.c`, since removed) wasn't on the table: it assumed a fresh, CPU-exclusive, unrotated XR24 frame, which doesn't match a persistent, dirty-painted, rotated BA24 V4L2 source (or the two reused DRM destinations), and it lacked a raw evdev cancellation adapter.

The planned next step was a small offline Unicorn harness exercising only the lifted bitmap, repaint, and conversion functions against synthetic memory and mocked imports to test:

- tracking one source, two destinations, and every source write,
- saving before `QBUF` and restoring after a successful `DQBUF`,
- pointer reuse, errors/timeouts, and repaint lock behavior,
- separating flip completion events from commit submission,
- modeling buffered touch input crossing a presentation ack to pin down the quarantine release barrier.

Using the digest-verified functions from the audit JSON, that would validate control flow and ghost-free restoration without real DRM/evdev I/O (though still without proving real kernel coherency or authorizing hardware injection).

Open requirements remaining before running a shim on hardware:

- verified DMA completion, cache coherency, and fence semantics,
- actual pitch, channel order, alpha, and rotation accepted by the driver,
- sound restore/copy ownership across failed `QBUF`, `DQBUF`, `select`, and commit paths,
- complete raw contact ownership and cancellation with no tap-through, plus FD reuse, wake, and `errno` handling,
- tying each frame to its presentation ack and ordering buffered input against it,
- bounded latency,
- separately reviewed launch and recovery integration.

Just having an atomic commit hook and two `mmap` pointers doesn't satisfy any of those on its own.
