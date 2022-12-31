## Use Rust to build
FROM rustlang/rust:nightly as builder

## Install build dependencies.
RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y gcc cmake make zlib1g-dev libffi-dev lsb-release wget software-properties-common gnupg && \
    wget https://apt.llvm.org/llvm.sh && chmod +x llvm.sh && ./llvm.sh 13 && \
    cargo install cargo-fuzz

## Add source code to the build stage.
ADD . /wasmer
WORKDIR /wasmer/fuzz

RUN export LLVM_SYS_120_PREFIX=$(llvm-config-13 --prefix) && \
    cargo +nightly fuzz build deterministic --features "universal cranelift llvm singlepass" && \
    cargo +nightly fuzz build equivalence_universal --features "universal" && \
    cargo +nightly fuzz build metering --features "universal cranelift" && \
    cargo +nightly fuzz build universal_cranelift --features "universal cranelift" && \
    cargo +nightly fuzz build universal_llvm --features "universal llvm" && \
    cargo +nightly fuzz build universal_singlepass --features "universal singlepass"

# cargo build  --release --manifest-path lib/cli/Cargo.toml --features cranelift,llvm,singlepass,wasmer-artifact-create,static-artifact-create,wasmer-artifact-load,static-artifact-load --features="webc_runner" --bin wasmer

# Package Stage
FROM --platform=linux/amd64 ubuntu:20.04

RUN apt-get update && \
    DEBIAN_FRONTEND=noninteractive apt-get install -y libffi

## Copy the binary from the build stage to an Ubuntu docker image
COPY --from=builder \
    /wasmer/target/x86_64-unknown-linux-gnu/release/deterministic \
    /wasmer/target/x86_64-unknown-linux-gnu/release/equivalence_universal \
    /wasmer/target/x86_64-unknown-linux-gnu/release/metering \
    /wasmer/target/x86_64-unknown-linux-gnu/release/universal_cranelift \
    /wasmer/target/x86_64-unknown-linux-gnu/release/universal_llvm \
    /wasmer/target/x86_64-unknown-linux-gnu/release/universal_singlepass \
    /
