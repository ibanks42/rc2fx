# Build environment for `just package-check`: the Ubuntu baseline of the
# release packages, with the release job's Rust tools at their pinned versions.
FROM docker.io/library/ubuntu:24.04
ARG RUST_VERSION
ENV RUSTUP_HOME=/opt/rustup CARGO_HOME=/opt/cargo PATH=/opt/cargo/bin:$PATH
COPY linux-build-deps.sh /tmp/linux-build-deps.sh
RUN sh /tmp/linux-build-deps.sh \
    && apt-get install -y --no-install-recommends \
        ca-certificates curl file git python3 xz-utils \
    && rm -rf /var/lib/apt/lists/*
RUN curl --fail --location --proto '=https' -o /tmp/rustup-init \
        https://static.rust-lang.org/rustup/archive/1.29.3/x86_64-unknown-linux-gnu/rustup-init \
    && echo 'b1c8cb14e30889e7e2999e6b69e62d9e9b9ffc0f4df8180564d306d9680e6c55  /tmp/rustup-init' | sha256sum -c - \
    && chmod +x /tmp/rustup-init \
    && /tmp/rustup-init -y --no-modify-path --profile minimal --default-toolchain "$RUST_VERSION" \
        --component rustfmt --component clippy --component rust-docs \
        --target aarch64-unknown-linux-gnu \
    && rm /tmp/rustup-init
RUN cargo install --locked cargo-about --version 0.8.4 \
    && cargo install --locked cargo-zigbuild --version 0.24.0 \
    && rm -rf /opt/cargo/registry /opt/cargo/git
# The release job installs current packages. Refresh on every run, after the slow
# layers, so a new transitive library shows up here rather than on GitHub.
ARG APT_REFRESH
RUN apt-get update \
    && DEBIAN_FRONTEND=noninteractive apt-get full-upgrade -y --no-install-recommends \
    && rm -rf /var/lib/apt/lists/*
# The build runs as the calling user, with its caches on a mounted directory.
RUN mkdir -p /work/src /work/cache && chmod -R 1777 /work
