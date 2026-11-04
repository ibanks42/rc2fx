# Third-party notices

RC2FX's own code and its trained noise suppression weights are licensed under
MIT (see [LICENSE-MIT](LICENSE-MIT)). It also includes or builds the
third-party components listed below, each under its own licence. Distribute this
file, and the licence texts it refers to, with any binaries. The desktop app
shows these notices, the training-data credits and every licence text under
**Settings › Versions › Licenses › View licenses**.

## Mixer build

`librc2fx_native.so` embeds two sets of DeepFilterNet3 noise suppression
weights (LA0 and LA1), run by an engine built from Rust source.

DeepFilterNet 0.5.6 (Rikorose/DeepFilterNet), used under its MIT option.
Copyright (c) 2021 Hendrik Schröter. `crates/rc2fx-dfn` ports libDF's framing
and features and reimplements the DeepFilterNet3 networks. The weights were
trained by RC2FX with DeepFilterNet's own model code. Full text:
[crates/rc2fx-dfn/LICENSE-DEEPFILTERNET](crates/rc2fx-dfn/LICENSE-DEEPFILTERNET).

The weights were trained on third-party speech, noise and impulse-response
datasets, credited with their licences in
[crates/rc2fx-dfn/model/PROVENANCE.md](crates/rc2fx-dfn/model/PROVENANCE.md).
No dataset audio is included in RC2FX. Releasing the weights under MIT is the
project's decision; it does not relicense those datasets, whose own terms
still apply to them.

## Rust crates

The desktop app, `rc2fxd` and `librc2fx_native.so` link several hundred crates
from crates.io. Nearly all are MIT and/or Apache-2.0, including GPUI and
gpui-component. A few use other permissive licences (BSD-2/3, ISC, Zlib,
Unicode-3.0, CC0, 0BSD). `option-ext` is MPL-2.0; its unmodified source is
available on crates.io.

## Rust standard library

Every binary includes the Rust standard library from Rust 1.97.1 (the
toolchain pinned in `rust-toolchain.toml`). It is dual-licensed under MIT or
Apache-2.0; copyrights are retained by its contributors
(https://thanks.rust-lang.org). Some of its components and dependencies carry
other notices, including Apache-2.0 WITH LLVM-exception, Unicode-3.0 and
BSD-2-Clause. The Rust project's own notice for the standard library,
`COPYRIGHT-library.html` from that toolchain's documentation, lists them all
with their licence texts; the app's licence notices include its full text.

## Lucide icons

The desktop app embeds icons from [Lucide](https://lucide.dev) through
`gpui-kit-assets`. ISC License, Copyright (c) 2026 Lucide Icons and
Contributors. The icons Lucide derives from Feather are also under the MIT
License, Copyright (c) 2013-present Cole Bemis. Full text:
[licenses/LICENSE-LUCIDE](licenses/LICENSE-LUCIDE).

## Full licence texts

`cargo xtask package` runs [cargo-about](https://github.com/EmbarkStudios/cargo-about)
for the desktop workspace and the mixer library and combines its reports with
these notices, the training-data credits and the project licence texts. The
result is built into the app, under **Settings › Versions › Licenses › View licenses**. The Linux AppImage
also carries it as `LICENSES.html` in its system licence directory. To write
the same document as HTML without packaging, install cargo-about
(`cargo install --locked cargo-about`) and run:

```sh
cargo xtask licenses LICENSES.html
```

## RØDE

RC2FX is unofficial and is not affiliated with or endorsed by RØDE. It contains
no RØDE code or firmware. RØDE and RØDECaster are trademarks of Freedman
Electronics Pty Ltd. They are used here only to identify the hardware RC2FX
works with.
