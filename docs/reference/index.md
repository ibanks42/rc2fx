# Technical reference

These docs cover RC2FX's protocols, file formats, firmware support, and individual crates. If you haven't read [Architecture](../developer/architecture.md) yet, start there.

- [Noise suppression internals](suppression.md): the models, delay numbers, worker thread setup, Chat Mix Out handling, and voice gate.
- [Installer internals](installer.md): what gets added to the firmware image and how the flashing process works.
- [USB control](usb-control.md): sharing RØDE's HID interface, message framing, and restarting the audio engine without dropping USB.
- [Controller API and settings](api.md): the endpoints `rc2fxd` exposes and the saved settings schema.
- [EQ format and EasyEffects](eq-format.md): how EQ bands are stored and computed, plus EasyEffects preset conversion.
- [Firmware support](firmware.md): the firmware catalog, how releases get qualified, and how to add a new one.
- [RØDECaster Duo](duo.md): experimental Duo support and what still needs testing on real hardware.
- [Stock source names](source-name-evidence.md): where input labels come from in the public protocol and pinned firmware (separate from which paths RC2FX can actually process).
- [Virtual USB paths](usb-paths.md): buffer identities for the virtual USB inputs and USB sends, offline qualification, and remaining hardware checks.
- [Hardware](hardware.md): CPU, memory, and hardware quirks on the mixer.

Each crate section pulls directly from that crate's `README.md`, aimed at anyone working inside that crate. The unfinished on-screen panel is covered in [`experiments/device-panel`](../../experiments/device-panel/README.md).
