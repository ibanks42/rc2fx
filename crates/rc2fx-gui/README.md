# rc2fx-gui: screen conversion module

`librc2fx_gui.so` is an `LD_AUDIT` module for RØDE's screen program,
`rc_gui_juce` 1.7.7 on the RØDECaster Pro II (SHA-256 `aa92a92d…`). Other
binaries run unmodified.

The GUI draws upright, but the panel is mounted sideways, so each frame is
rotated before display. Stock firmware has the GE2D converter rotate the full
frame every time, and the memory traffic disturbs the audio. This module
converts only the regions that changed, in up to three jobs per frame. With
the app's
[screen conversion](../../docs/user/settings-and-diagnostics.md#screen-conversion)
setting it can also rotate large redraws on the CPU, fully or partly. It
patches no instructions and draws nothing itself.

## Building

The module is C and is built outside Cargo. `cargo xtask mixer-build` compiles
it with zig for the mixer:

```text
gui_pacing.c -DRC2FX_PACING_DIRTY -DRC2FX_PACING_PARTIAL -DRC2FX_GUI_RECT_PLAN3
```

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

The source also contains earlier trial builds that the tests still exercise:
the presentation-pacing modes (`RC2FX_GUI_PACING=observe`, `30` … `2`), the
observe-only dirty-region build, the two-job planner (built without
`-DRC2FX_GUI_RECT_PLAN3`), and a forced full-frame test dose read from
`/opt/rc2fx/data/gui-force-full`. The mixer build uses none of them.
