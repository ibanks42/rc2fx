# rc2fx-gui: screen conversion module

`librc2fx_gui.so` is an `LD_AUDIT` module for RØDE's screen program,
`rc_gui_juce` 1.7.7 on the RØDECaster Pro II (SHA-256 `aa92a92d…`). All other
binaries run untouched.

The GUI renders upright, but the physical display panel is mounted sideways, so
every frame has to be rotated before it hits the screen. Stock firmware asks
the GE2D hardware converter to rotate the entire frame every time, and that
memory traffic can disturb audio. This module converts only the regions that
actually changed (up to three jobs per frame) and handles large redraws on the
CPU instead. The size threshold is fixed with no runtime toggle. The module
doesn't patch any instructions or draw anything itself.

## Building

This is a standalone C module built outside Cargo. `./xtask mixer-build`
compiles `gui_pacing.c` for the mixer using zig, with no feature flags.

`gui_pacing.c` includes `gui_partial.c` (partial-region conversion) and
`rotate/` (CPU rotation). The header comment in each file documents its pinned
call sites and what the module does at each one.

## Startup

The installer places a shim at `/usr/sbin/rc_gui_juce`. Because RØDE's display
loop launches `rc_gui_juce` by name with `/usr/sbin` first on `PATH`, the shim
hands off to `rc2fx.sh gui-launch`. That script always runs the stock GUI, and
only attaches `librc2fx_gui.so` when every one of these checks passes:

- it is neither a first boot nor a production boot,
- the module isn't disabled by a switch,
- the GUI binary matches the qualified hash,
- the bundle's module matches its checksum,
- the GUI isn't crash-looping (three starts within 30 s fall back to the stock
  GUI until the next reboot).

Changes take effect on the boot after installing.

| Switch | Effect |
| --- | --- |
| `/opt/rc2fx/data/gui-partial-disabled` | Run stock GUI from the next GUI start (persistent) |
| `/run/rc2fx-gui/off` | Run stock GUI until the next reboot |
| `/run/rc2fx-gui/partial-off` | Convert full frames from the next conversion in the running GUI |

`rc2fx.sh status` reports which switch is active. Release builds write minimal
logs to `/run/rc2fx-gui/pacing.log` (startup, first pass, fallbacks, and one
counters summary per minute). Development builds log every frame.

## Tests

```sh
python3 mixer/gui/test_gui_pacing.py
```

This builds the module with `-DRC2FX_PACING_TEST` so call sites are resolved by
symbol name instead of pinned addresses, then runs it under `LD_AUDIT` against
`gui_pacing_test.cpp` (a mock GUI frame pump) and `fake_v4l2.c` (a mock GE2D
device). It runs locally with no mixer needed and is included in
`just test-mixer`.

Setting `RC2FX_GUI_PACING=observe` counts and times frames and paints without
performing conversions; on the mixer it runs in `partial-cw` mode. The test
build also exposes host-only controls (such as a forced full-frame dose) that
aren't compiled into the mixer build. `partial_plan_test.c` exercises
`gui_partial.c` directly as compiled for the mixer, without the `LD_AUDIT`
harness.
