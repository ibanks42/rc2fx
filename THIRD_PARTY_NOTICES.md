# Third-party notices

RC2FX's own code is licensed under MIT or Apache-2.0, at your option (see
[LICENSE-MIT](LICENSE-MIT) and [LICENSE-APACHE](LICENSE-APACHE)). It also
includes or builds the third-party components listed below. Distribute this
file, and the licence files it refers to, with any binaries.

## Mixer build

`librc2fx_native.so` and the `rc2fx-bench` benchmark include one noise
suppression model, built from Rust source.

DeepFilterNet 0.5.6 (Rikorose/DeepFilterNet). MIT or Apache-2.0. Copyright (c)
2021 Hendrik Schröter. `crates/rc2fx-dfn` ports libDF's framing and features
and reimplements the DeepFilterNet3 networks. Its weights were produced with
DeepFilterNet's own model code. Full text:
[crates/rc2fx-dfn/LICENSE-DEEPFILTERNET](crates/rc2fx-dfn/LICENSE-DEEPFILTERNET).

## Rust crates

The desktop app, `rc2fxd`, `librc2fx_native.so` and the benchmarks link several
hundred crates from crates.io. Nearly all are MIT and/or Apache-2.0, including
GPUI and gpui-component. A few use other permissive licences (BSD-2/3, ISC,
Zlib, Unicode-3.0, CC0, 0BSD, BSL-1.0). `dwrote`, `option-ext` and the
build-time `cbindgen` are MPL-2.0. Their unmodified source is available on
crates.io.

To generate the full licence text for every crate in a binary release, use
[cargo-about](https://github.com/EmbarkStudios/cargo-about):

```sh
cargo install --locked cargo-about
cargo about generate about.hbs > THIRD_PARTY_LICENSES.html
```

`cargo xtask package` runs cargo-about for the desktop workspace and the mixer
library and puts both reports in the package's `licenses/` folder.

## RØDE

RC2FX is unofficial and is not affiliated with or endorsed by RØDE. It contains
no RØDE code or firmware. RØDE and RØDECaster are trademarks of Freedman
Electronics Pty Ltd. They are used here only to identify the hardware RC2FX
works with.
