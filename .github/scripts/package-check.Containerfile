# Build environment for the Linux AppImage, in releases and in
# `just package-check`. The AppImage runs on any distribution with at least
# the glibc it was built against, and bundles libraries from here too, so this
# is AlmaLinux 8 (glibc 2.28, as RHEL 8, Debian 10 and Ubuntu 18.10): the
# oldest maintained base, and the oldest glibc GitHub's JavaScript actions
# run on. It has the release's Rust tools at their pinned versions (the
# mixer, and cargo-about for the notices), Go for ./xtask and the desktop app
# (cgo against gpuikit's static library, whose system libraries the AppImage
# bundles from here),
# and .NET and mksquashfs for vpk, which makes the AppImage.
FROM docker.io/library/almalinux:8
ARG RUST_VERSION
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
RUN dnf install -y dnf-plugins-core \
    && dnf config-manager --set-enabled powertools \
    && dnf install -y \
        gcc gcc-c++ make cmake clang perl pkgconf-pkg-config \
        fontconfig-devel wayland-devel libxkbcommon-devel libxkbcommon-x11-devel \
        libX11-devel libxcb-devel vulkan-loader alsa-lib-devel \
        openssl-devel systemd-devel dbus-devel \
        ca-certificates curl diffutils file findutils git python3.12 unzip which xz zstd \
        dotnet-sdk-8.0 zlib-devel xz-devel lz4-devel libzstd-devel \
    && alternatives --set python3 /usr/bin/python3.12 \
    && dnf clean all
# vpk passes mksquashfs -mkfs-time, which needs squashfs-tools 4.4 or newer;
# AlmaLinux 8 has 4.3. Build the pinned release instead.
RUN curl --fail --location --proto '=https' -o /tmp/squashfs-tools.tar.gz \
        https://github.com/plougher/squashfs-tools/releases/download/4.6.1/squashfs-tools-4.6.1.tar.gz \
    && echo '94201754b36121a9f022a190c75f718441df15402df32c2b520ca331a107511c  /tmp/squashfs-tools.tar.gz' | sha256sum -c - \
    && mkdir /tmp/squashfs-tools \
    && tar -xzf /tmp/squashfs-tools.tar.gz --strip-components=1 -C /tmp/squashfs-tools \
    && make -C /tmp/squashfs-tools/squashfs-tools -j"$(nproc)" \
        GZIP_SUPPORT=1 XZ_SUPPORT=1 LZ4_SUPPORT=1 ZSTD_SUPPORT=1 \
    && install -m 755 /tmp/squashfs-tools/squashfs-tools/mksquashfs /tmp/squashfs-tools/squashfs-tools/unsquashfs /usr/local/bin/ \
    && rm -rf /tmp/squashfs-tools /tmp/squashfs-tools.tar.gz \
    && mksquashfs -version | head -1
# gpuikit's static library calls xkb_utf32_to_keysym, which libxkbcommon has
# from 1.0; AlmaLinux 8 has 0.9.1. Build the pinned release into /usr/local,
# where the linker (LIBRARY_PATH) and the loader find it before the system's,
# so the app links against it and the AppImage bundles it, with that
# release's licence text in place of the system package's.
RUN dnf install -y meson ninja-build bison xkeyboard-config && dnf clean all \
    && curl --fail --location --proto '=https' -o /tmp/libxkbcommon.tar.xz \
        https://xkbcommon.org/download/libxkbcommon-1.7.0.tar.xz \
    && echo '65782f0a10a4b455af9c6baab7040e2f537520caa2ec2092805cdfd36863b247  /tmp/libxkbcommon.tar.xz' | sha256sum -c - \
    && mkdir /tmp/libxkbcommon \
    && tar -xJf /tmp/libxkbcommon.tar.xz --strip-components=1 -C /tmp/libxkbcommon \
    && meson setup /tmp/libxkbcommon/build /tmp/libxkbcommon \
        --prefix=/usr/local --libdir=lib64 --buildtype=release \
        -Dxkb-config-root=/usr/share/X11/xkb -Dx-locale-root=/usr/share/X11/locale \
        -Denable-wayland=false -Denable-xkbregistry=false -Denable-docs=false -Denable-tools=false \
    && ninja -C /tmp/libxkbcommon/build install \
    && install -m 644 /tmp/libxkbcommon/LICENSE /usr/share/licenses/libxkbcommon/LICENSE \
    && rm -rf /tmp/libxkbcommon /tmp/libxkbcommon.tar.xz \
    && echo /usr/local/lib64 > /etc/ld.so.conf.d/00-local.conf && ldconfig \
    && ldconfig -p | grep -q 'libxkbcommon.so.0 .*/usr/local/lib64/'
ENV LIBRARY_PATH=/usr/local/lib64
RUN curl --fail --location --proto '=https' -o /tmp/rustup-init \
        https://static.rust-lang.org/rustup/archive/1.29.3/x86_64-unknown-linux-gnu/rustup-init \
    && echo 'b1c8cb14e30889e7e2999e6b69e62d9e9b9ffc0f4df8180564d306d9680e6c55  /tmp/rustup-init' | sha256sum -c - \
    && chmod +x /tmp/rustup-init \
    && /tmp/rustup-init -y --no-modify-path --profile minimal --default-toolchain "$RUST_VERSION" \
        --component rustfmt --component clippy --component rust-docs \
        --target aarch64-unknown-linux-gnu \
    && rm /tmp/rustup-init
# Go, at least the version desktop/go.mod names. AlmaLinux's own Go is older,
# so it only fetches the pinned release as Go's toolchain module, which the go
# command checks against the Go checksum database, and that copy becomes
# /usr/local/go. GOTOOLCHAIN=local keeps builds on it.
RUN dnf install -y golang \
    && GOTOOLCHAIN=go1.27.4 GOPROXY=https://proxy.golang.org GOSUMDB=sum.golang.org GOPATH=/tmp/gopath \
        go env GOROOT > /tmp/goroot \
    && cp -a "$(cat /tmp/goroot)" /usr/local/go \
    && chmod -R u+w /tmp/gopath && rm -rf /tmp/gopath /tmp/goroot \
    && dnf remove -y golang && dnf clean all \
    && /usr/local/go/bin/go version | grep -q ' go1\.27\.4 '
ENV PATH=/usr/local/go/bin:$PATH GOTOOLCHAIN=local
RUN cargo install --locked cargo-about --version 0.8.4 \
    && cargo install --locked cargo-zigbuild --version 0.24.0 \
    && rm -rf /opt/cargo/registry /opt/cargo/git
# Refresh on every run, after the slow layers, so a security update to a
# bundled library is picked up.
ARG PACKAGE_REFRESH
RUN dnf upgrade -y && dnf clean all
# RHEL's libxcb package has no license file, but the AppImage bundles its
# libxcb-xkb and must carry the notice: take COPYING from the source package
# of exactly the installed version.
RUN dnf install -y bzip2 cpio \
    && work=$(mktemp -d) && cd "$work" \
    && dnf download --source "$(rpm -q libxcb)" \
    && rpm2cpio libxcb-*.src.rpm | cpio -id --quiet \
    && mkdir -p /usr/share/licenses/libxcb \
    && tar -xOf libxcb-*.tar.* --wildcards '*/COPYING' > /usr/share/licenses/libxcb/COPYING \
    && test -s /usr/share/licenses/libxcb/COPYING \
    && cd / && rm -rf "$work" && dnf clean all
# The build runs as the calling user, with its caches on a mounted directory.
RUN mkdir -p /work/src /work/cache && chmod -R 1777 /work
