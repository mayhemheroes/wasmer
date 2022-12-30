## Use Rust to build
FROM rustlang/rust:nightly as builder

## Install build dependencies.
RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y gcc llvm cmake make clang-10

## Add source code to the build stage.
ADD . /wasmer
WORKDIR /wasmer

RUN cargo install cargo-fuzz llvmenv
RUN llvmenv init && llvmenv entries && llvmenv build-entry 12.0.0
ENV LLVM_SYS_120_PREFIX=$HOME/llvm-12.0.0

WORKDIR /wasmer/fuzz
RUN cargo +nightly fuzz build deterministic --features "universal cranelift llvm singlepass"
RUN cargo +nightly fuzz build equivalence_universal --features "universal"
RUN cargo +nightly fuzz build metering --features "universal cranelift"
RUN cargo +nightly fuzz build universal_cranelift --features "universal cranelift"
RUN cargo +nightly fuzz build universal_llvm --features "universal llvm"
RUN cargo +nightly fuzz build universal_singlepass --features "universal singlepass"

# Package Stage
FROM --platform=linux/amd64 ubuntu:20.04

## Copy the binary from the build stage to an Ubuntu docker image
COPY --from=builder \
        /wasmer/target/x86_64-unknown-linux-gnu/release/deterministic \
        /wasmer/target/x86_64-unknown-linux-gnu/release/equivalence_universal \
        /wasmer/target/x86_64-unknown-linux-gnu/release/metering \
        /wasmer/target/x86_64-unknown-linux-gnu/release/universal_cranelift \
        /wasmer/target/x86_64-unknown-linux-gnu/release/universal_llvm \
        /wasmer/target/x86_64-unknown-linux-gnu/release/universal_singlepass \
        /
