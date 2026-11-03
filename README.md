# RC2FX

RC2FX adds audio processing to the RØDECaster Pro II: a parametric EQ with up
to 32 bands, a compressor, a limiter, a high-pass filter, a gain stage and
noise suppression. The processing runs on the mixer inside RØDE's audio
engine, so every device connected to the mixer hears the result. A desktop
app for Windows, Linux and macOS changes the settings over USB. The settings
are stored on the mixer and stay in effect after the app is closed.

## Features

- Parametric EQ with up to 32 bands per channel. It can import Equalizer APO
  and AutoEQ text and EasyEffects presets.
- Noise suppression with DeepFilterNet3, retrained for keyboard and mouse
  noise, with an optional voice gate.
- Suppression on Chat Mix Out, which cleans up the microphones that people on
  a call hear and leaves Smart Pads and music alone.
- Compressor, limiter, high-pass and gain, in any order.
- 25 processing paths on the Pro II: microphones, USB and Chat inputs,
  CallMe, wireless, Bluetooth, the virtual USB inputs, all four headphone
  outputs, the monitors and the USB mix outputs.

A fresh install processes nothing until you turn a channel on. Flashing
stock RØDE firmware removes RC2FX.

## Quick start

1. Download the app for your computer from the releases page, extract the
   whole archive and run it.
2. Connect the computer to the mixer's **USB 1** port.
3. Choose **Install RC2FX**. The app downloads official RØDE firmware, adds
   RC2FX to it and flashes it through the mixer's USB update mode. The
   computer needs an internet connection for the download.
4. Pick a channel, turn on **Processing**, switch on some effects and press
   **Save to mixer**.

The [user guide](docs/introduction.md) covers each step in more detail.

RC2FX supports Pro II firmware 1.7.7 and 1.7.6. RØDECaster Duo support is
[experimental and untested on hardware](docs/reference/duo.md) and is off
unless you enable it.

## Building from source

You need Rust ([rustup](https://rustup.rs)), [just](https://just.systems),
[zig](https://ziglang.org/download/) 0.13 or newer and your platform's C build
tools ([details](docs/developer/building.md)).

```sh
git clone https://github.com/ibanks42/rc2fx
cd rc2fx
just setup            # once: the mixer's Rust target and cargo-zigbuild
just app              # build the mixer runtime and run the desktop app
just test             # tests and Clippy
```

The build works on Windows, Linux and macOS. The
[developer guide](docs/developer/index.md) covers setup, working on a real
mixer and the project layout.

## License

RC2FX is licensed under either [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option. Third-party components and
their licences are listed in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

RC2FX is an unofficial project and is not affiliated with or endorsed by
RØDE. It needs root access to the mixer and only runs on firmware it has been
checked against. It is provided as is, without warranty. Installing it may
void your warranty.
