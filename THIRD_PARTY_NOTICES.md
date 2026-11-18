# Third-party notices

RC2FX's own code and its trained noise suppression weights are licensed under
MIT (see [LICENSE-MIT](LICENSE-MIT)). It also includes or builds the
third-party components listed below, each under its own licence. Distribute this
file, and the licence texts it refers to, with any binaries. The desktop app
shows these notices, the training-data credits and every licence text under
**Settings › About › Licenses › View licenses**.

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

The mixer's programs (`rc2fxd`, `rc2fx-supervisor`, `rc2fx-firmware` and
`librc2fx_native.so`) link crates from crates.io. Nearly all are MIT and/or
Apache-2.0; a few use other permissive licences (BSD-2/3, ISC, Zlib,
Unicode-3.0, CC0, 0BSD).

The desktop app is written in Go. Its interface library, libgpuikit, is built
from Rust crates, including GPUI and gpui-component, and is linked into the
app; the gpuikit download lists those crates with their licences, and the
app's licence notices include that list and its licence texts.

## Go

The desktop app includes the Go standard library (BSD-3-Clause) and the Go
modules it links, each under its own permissive licence. The app's licence
notices include each module's licence text.

## Rust standard library

The mixer's programs include the Rust standard library from Rust 1.100.0 (the
toolchain pinned in `rust-toolchain.toml`); libgpuikit and Velopack's C
library in the desktop app include it from the toolchains they were built
with. It is dual-licensed under MIT or
Apache-2.0; copyrights are retained by its contributors
(https://thanks.rust-lang.org). Some of its components and dependencies carry
other notices, including Apache-2.0 WITH LLVM-exception, Unicode-3.0 and
BSD-2-Clause. The Rust project's own notice for the standard library,
`COPYRIGHT-library.html` from that toolchain's documentation, lists them all
with their licence texts; the app's licence notices include its full text.

## Lucide icons

The desktop app embeds icons from [Lucide](https://lucide.dev) through
the gpuikit library. ISC License, Copyright (c) 2026 Lucide Icons and
Contributors. The icons Lucide derives from Feather are also under the MIT
License, Copyright (c) 2013-present Cole Bemis. Full text:
[licenses/LICENSE-LUCIDE](licenses/LICENSE-LUCIDE).

## Velopack

The Windows and macOS packages and the Linux AppImage are built with
[Velopack](https://github.com/velopack/velopack) 1.3.39 and include its
updater (`Update.exe` on Windows, `UpdateMac` on macOS, `UpdateNix` in the
AppImage); the Windows and macOS packages also include its launcher and
installer. On Windows and macOS, Velopack's packager gives them RC2FX's
icon and version details, appends the app to Setup and signs them; their code
is unchanged. The app also links Velopack's C library (`velopack_libc`,
statically on Linux and macOS; `velopack_libc.dll` beside `rc2fx.exe` on
Windows). MIT License, Copyright
© 2021 Caelan Sayler, Copyright © 2024 Velopack Ltd. Full text:
[licenses/LICENSE-VELOPACK](licenses/LICENSE-VELOPACK). The notices of the
crates those programs are built from are in
[licenses/velopack-1.3.39-g8d2844f.json](licenses/velopack-1.3.39-g8d2844f.json),
generated from Velopack's source for its Windows, macOS and Linux builds,
and in the app's licence notices. `UpdateNix` is Velopack's static musl
build, so it also includes musl (MIT License), whose text the AppImage's
licence notices carry.

## e2fsprogs (libext2fs and libcom_err)

Repair mixer, in the desktop app on Linux, uses libext2fs and libcom_err from
[e2fsprogs](https://e2fsprogs.sourceforge.net/) 1.47.4, built from the sources
vendored in `desktop/ext2fs/e2fsprogs` and linked statically into the
app through cgo. libext2fs is licensed under the GNU Library General Public License,
version 2; libcom_err under an MIT-style licence (its source file headers).
The full texts and the notice are in `desktop/ext2fs/e2fsprogs/NOTICE`.
The rest of RC2FX's source is public, so the app can be rebuilt with a
modified libext2fs, as the LGPL requires.

## AppImage runtime

The Linux AppImage starts with the
[AppImage type 2 runtime](https://github.com/AppImage/type2-runtime), the
build that Velopack's packager supplies (type2-runtime commit `3d17002`). It
is a static executable that links a modified libfuse 3.15.0, licensed under
the GNU Lesser General Public License 2.1, with squashfuse 0.5.2, zstd, zlib,
musl and mimalloc, each under its own licence. The AppImage's licence
notices include all of their licence texts. Each release publishes the source
of the runtime and of these libraries, the libfuse patch and the build
scripts as
`rc2fx-VERSION-appimage-runtime-sources.tar.gz`, beside the AppImage, so you
can rebuild the runtime with a modified libfuse.

## Full licence texts

`./xtask package` runs [cargo-about](https://github.com/EmbarkStudios/cargo-about)
for the mixer workspace and the mixer library and combines its reports with
these notices, the training-data credits, the project licence texts, the Go
standard library's and Go modules' licences, and libgpuikit's notices. The
result is built into the app, under **Settings › About › Licenses › View
licenses**. The AppImage also carries it as `LICENSES.html` in its system
licence directory. To write
the same document as HTML without packaging, less the Go and libgpuikit
notices, which only packaging adds, install cargo-about
(`cargo install --locked cargo-about`) and run:

```sh
./xtask licenses LICENSES.html
```

## RØDE

RC2FX is unofficial and is not affiliated with or endorsed by RØDE. It contains
no RØDE code or firmware. RØDE and RØDECaster are trademarks of Freedman
Electronics Pty Ltd. They are used here only to identify the hardware RC2FX
works with.
