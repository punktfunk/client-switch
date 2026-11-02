# The console lane's image: devkitA64, libnx and switch-tools from devkitPro's pacman (the base
# image is published for amd64 and arm64, so this runs natively on either), the pinned nightly
# with rust-src for -Z build-std, and clang for bindgen. scripts/pins.env names every version.
FROM devkitpro/devkita64@sha256:1fc388c3a0d34bd2045a6dadcb1020e069d5f876a187fd705de14b4440c00282

# devkitPro serves only its current packages, so -Syu takes today's; the check fails the build
# when one moved past scripts/pins.env, which makes a bump a deliberate change.
ARG DEVKITA64=r30-1
ARG LIBNX=4.12.0-1
ARG SWITCH_TOOLS=1.13.1-1
ARG SWITCH_FFMPEG=7.1-5
ARG SWITCH_DAV1D=1.5.0-1
RUN dkp-pacman -Syu --noconfirm --needed devkitA64 libnx switch-tools switch-libopus switch-ffmpeg switch-dav1d \
    && dkp-pacman -Scc --noconfirm \
    && dkp-pacman -Q devkitA64 libnx switch-tools switch-ffmpeg switch-dav1d | tee /opt/devkitpro/VERSIONS \
    && grep -qx "devkitA64 $DEVKITA64" /opt/devkitpro/VERSIONS \
    && grep -qx "libnx $LIBNX" /opt/devkitpro/VERSIONS \
    && grep -qx "switch-tools $SWITCH_TOOLS" /opt/devkitpro/VERSIONS \
    && grep -qx "switch-ffmpeg $SWITCH_FFMPEG" /opt/devkitpro/VERSIONS \
    && grep -qx "switch-dav1d $SWITCH_DAV1D" /opt/devkitpro/VERSIONS

# bindgen reads Skia's headers as aarch64 Linux (scripts/build-skia.sh): off an arm64 box that
# takes Debian's arm64 cross libstdc++, where clang finds it.
RUN apt-get update && apt-get install -y --no-install-recommends clang libclang-dev ninja-build pkg-config \
    && if [ "$(dpkg --print-architecture)" != arm64 ]; then \
        apt-get install -y --no-install-recommends libstdc++-12-dev-arm64-cross; fi \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    DEVKITA64=/opt/devkitpro/devkitA64 \
    PATH=/usr/local/cargo/bin:/opt/devkitpro/devkitA64/bin:/opt/devkitpro/tools/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

ARG RUST_NIGHTLY=nightly-2026-10-04
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal \
        --default-toolchain "$RUST_NIGHTLY" --component rust-src,llvm-tools-preview,clippy,rustfmt \
    && rustc -vV

WORKDIR /w
