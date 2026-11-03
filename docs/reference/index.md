# Technical reference

These pages cover protocols, file formats, firmware support and the crates.
Read [Architecture](../developer/architecture.md) first.

- [Noise suppression internals](suppression.md): the models, delay figures,
  the worker thread, Chat Mix Out and the voice gate.
- [Installer internals](installer.md): the files the installer adds to the
  firmware image and the steps it takes to flash it.
- [USB control](usb-control.md): how RC2FX shares RØDE's HID interface, the
  message framing, and engine restarts that keep USB connected.
- [Controller API and settings](api.md): the requests `rc2fxd` answers and
  the saved settings format.
- [EQ format and EasyEffects](eq-format.md): how EQ bands are stored and
  calculated, and how they convert to and from EasyEffects presets.
- [Firmware support](firmware.md): the firmware catalog, how a release is
  qualified, and how to add one.
- [RØDECaster Duo](duo.md): the experimental Duo support and the work that
  still needs a real Duo.
- [Stock source names](source-name-evidence.md): where input labels come
  from in the public protocol and the pinned firmware. This is separate from
  which paths can be processed.
- [Virtual USB paths](usb-paths.md): buffer identities for the virtual USB
  inputs and USB sends, how they were qualified offline, and what hardware
  testing remains.
- [Hardware](hardware.md): the mixer's processor, memory and quirks.

Each crate chapter is that crate's `README.md`, written for people changing
the crate. The unfinished on-screen panel is documented in
[`experiments/device-panel`](../../experiments/device-panel/README.md).
