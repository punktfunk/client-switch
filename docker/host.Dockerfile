# The dev box's image: CI's host lane (ubuntu-24.04 with its packages) for a box that is not
# Linux, so the tests and pf-nx-host run on lavapipe there too. scripts/docker.sh runs it when
# PF_NX_IMAGE names it (task docker:host-test).
FROM ubuntu:24.04

RUN apt-get update && apt-get install -y --no-install-recommends \
        build-essential ca-certificates clang cmake curl git libclang-dev libfontconfig1-dev \
        libfreetype-dev libvulkan1 mesa-vulkan-drivers pkg-config python3 \
    && rm -rf /var/lib/apt/lists/*

ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    CMAKE_POLICY_VERSION_MINIMUM=3.5 \
    PATH=/usr/local/cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

ARG RUST_NIGHTLY=nightly-2026-10-04
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal \
        --default-toolchain "$RUST_NIGHTLY" --component rust-src,llvm-tools-preview,clippy,rustfmt \
    && rustc -vV

WORKDIR /w
