# qc-voice-bridge — build for Raspberry Pi 64-bit (aarch64 Linux)
#
# Usage:
#   make           # cross-compile release binary for Pi (from Mac/Linux host)
#   make rpi       # same as default
#   make native    # build for the current machine
#   make test      # run unit tests (host)
#   make clean
#
# On the Pi itself, `make native` (or plain `cargo build --release`) is enough.
# From a Mac, install the cross toolchain once:
#   brew install messense/macos-cross-toolchains/aarch64-unknown-linux-gnu
#   rustup target add aarch64-unknown-linux-gnu
#
# Docker-based cross build (no local linker):
#   make rpi CROSS=1

CRATE      := qc-voice-bridge
RPI_TARGET := aarch64-unknown-linux-gnu
CROSS_GCC  := aarch64-unknown-linux-gnu-gcc

# Override if your linker has a different name (e.g. aarch64-linux-gnu-gcc on Debian).
CROSS_COMPILE ?=
ifneq ($(CROSS_COMPILE),)
  export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER := $(CROSS_COMPILE)gcc
else ifneq ($(shell command -v $(CROSS_GCC) 2>/dev/null),)
  export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER := $(CROSS_GCC)
else ifneq ($(shell command -v aarch64-linux-gnu-gcc 2>/dev/null),)
  export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER := aarch64-linux-gnu-gcc
endif

RPI_BIN    := target/$(RPI_TARGET)/release/$(CRATE)
NATIVE_BIN := target/release/$(CRATE)

.PHONY: all rpi native test lint fmt fmt-check clean check-toolchain

all: rpi

rpi: check-toolchain
ifdef CROSS
	cross build --release --target $(RPI_TARGET)
else
	cargo build --release --target $(RPI_TARGET)
endif
	@echo "Built $(RPI_BIN)"

native:
	cargo build --release
	@echo "Built $(NATIVE_BIN)"

test:
	cargo test

lint:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings

fmt:
	cargo fmt

fmt-check:
	cargo fmt --check

clean:
	cargo clean

check-toolchain:
	@rustup target list --installed | grep -q '^$(RPI_TARGET)$$' \
		|| rustup target add $(RPI_TARGET)
ifndef CROSS
	@if [ -z "$$CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER" ]; then \
		echo "No aarch64 Linux linker found."; \
		echo "  macOS: brew install messense/macos-cross-toolchains/aarch64-unknown-linux-gnu"; \
		echo "  Debian/Ubuntu: apt install gcc-aarch64-linux-gnu"; \
		echo "  Or: make rpi CROSS=1  (requires: cargo install cross)"; \
		exit 1; \
	fi
endif
