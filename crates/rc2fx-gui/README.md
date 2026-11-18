# rc2fx-gui: screen conversion module

`librc2fx_gui.so` is an `LD_AUDIT` module for RØDE's screen program,
`rc_gui_juce` 1.7.7 on the RØDECaster Pro II (SHA-256 `aa92a92d…`). Other
binaries run unmodified.

The GUI draws upright, but the panel is mounted sideways, so each frame is
rotated before display. Stock firmware has the GE2D converter rotate the full
frame every time, and the memory traffic disturbs the audio. This module
converts only the regions that changed, in up to three jobs per frame, and
rotates large redraws on the CPU instead. The size threshold is fixed; there
is no runtime setting for it. It patches no instructions and draws nothing
itself.

## Building

The module is C and is built outside Cargo. `./xtask mixer-build` compiles
`gui_pacing.c` with zig for the mixer, with no feature flags.

`gui_pacing.c` includes `gui_partial.c` (the conversion) and `rotate/` (CPU
rotation). The header comment of each file lists the pinned call sites and
what the module does at each one.

## Startup

The installer adds a shim at `/usr/sbin/rc_gui_juce`. RØDE's display loop
starts `rc_gui_juce` by name with `/usr/sbin` first on `PATH`, so the shim
runs `rc2fx.sh gui-launch`. That script always starts the stock GUI, and adds
this module only when all of these checks pass:

- it is not a first boot or a production boot,
- the module is not switched off,
- the GUI binary is the qualified one,
- the bundle's module matches its checksum,
- the GUI is not crash-looping. Three GUI starts within 30 s switch to the
  stock GUI until the next boot.

Changes take effect from the boot after installing.

| Switch | Effect |
| --- | --- |
| `/opt/rc2fx/data/gui-partial-disabled` | Stock GUI from the next GUI start (persistent) |
| `/run/rc2fx-gui/off` | Stock GUI until the next boot |
| `/run/rc2fx-gui/partial-off` | Full frames from the next conversion, in the running GUI |

`rc2fx.sh status` shows which switch applies. Release builds log to
`/run/rc2fx-gui/pacing.log` sparingly: start, first pass, fallbacks, and one
counters line per minute. Development builds log every frame.

## Tests

```sh
python3 crates/rc2fx-gui/test_gui_pacing.py
```

The script builds the module with `-DRC2FX_PACING_TEST`, which finds the call
sites by symbol name instead of pinned address. It runs the module under
`LD_AUDIT` against `gui_pacing_test.cpp`, a stand-in for the GUI's frame pump,
and `fake_v4l2.c`, a stand-in GE2D device. It doesn't need a mixer, and it is
part of `just test-mixer`.

`RC2FX_GUI_PACING=observe` counts and times frames and paints without
converting anything; the mixer runs `partial-cw`. The test build also has
host-only controls, such as a forced full-frame dose, that the mixer build
does not contain. `partial_plan_test.c` drives `gui_partial.c` as the mixer
builds it, without the LD_AUDIT harness.
