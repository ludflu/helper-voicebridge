# qc-voice-bridge — build for Raspberry Pi 64-bit (aarch64 Linux)
#
# Usage:
#   make              # cross-compile on Mac (fast; needs sysroot once)
#   make rpi          # same
#   make deploy       # build + scp + restart service on lilhelper
#   make rpi-docker   # Docker buildx fallback (no local sysroot)
#   make native       # build for the current machine
#   make test         # run unit tests (host)
#   make clean
#
# Cross-compile from macOS (recommended):
#   brew install messense/macos-cross-toolchains/aarch64-unknown-linux-gnu
#   rustup target add aarch64-unknown-linux-gnu
#   make cross-sysroot   # once: download arm64 libpulse into cross-sysroot/
#   make rpi
#
# Docker fallback (if cross-sysroot fails):
#   make rpi-docker
#
# Pi native build:
#   sudo apt install libpulse-dev && cargo build --release

CRATE      := qc-voice-bridge
RPI_TARGET := aarch64-unknown-linux-gnu
RPI_BIN    := target/$(RPI_TARGET)/release/$(CRATE)
NATIVE_BIN := target/release/$(CRATE)
CROSS_SYSROOT ?= $(CURDIR)/cross-sysroot
DOCKER_PLATFORM := linux/arm64
DOCKER_BUILDER  := qc-voice-bridge-builder
PI_HOST    ?= jsnavely@lilhelper
PI_DIR     ?= ~/helper-agent
UNAME_M    := $(shell uname -m)

CROSS_GCC  := aarch64-unknown-linux-gnu-gcc
CROSS_COMPILE ?=
ifneq ($(CROSS_COMPILE),)
  export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER := $(CROSS_COMPILE)gcc
else ifneq ($(shell command -v $(CROSS_GCC) 2>/dev/null),)
  export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER := $(CROSS_GCC)
else ifneq ($(shell command -v aarch64-linux-gnu-gcc 2>/dev/null),)
  export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER := aarch64-linux-gnu-gcc
endif

# Cross-link env (rpi-host only — do not export globally; breaks host cargo test).
CROSS_ENV = \
	PKG_CONFIG_ALLOW_CROSS=1 \
	PKG_CONFIG_SYSROOT_DIR=$(CROSS_SYSROOT) \
	PKG_CONFIG_PATH=$(CROSS_SYSROOT)/usr/lib/aarch64-linux-gnu/pkgconfig:$(CROSS_SYSROOT)/usr/share/pkgconfig \
	RUSTFLAGS='-C link-arg=--sysroot=$(CROSS_SYSROOT) -C link-arg=-B$(CROSS_SYSROOT)/usr/lib/aarch64-linux-gnu -C link-arg=-L$(CROSS_SYSROOT)/lib/aarch64-linux-gnu -C link-arg=-Wl,-rpath-link,$(CROSS_SYSROOT)/lib/aarch64-linux-gnu -C link-arg=-Wl,-rpath-link,$(CROSS_SYSROOT)/usr/lib/aarch64-linux-gnu -C link-arg=-Wl,-rpath-link,$(CROSS_SYSROOT)/usr/lib/aarch64-linux-gnu/pulseaudio -C link-arg=-Wl,--allow-shlib-undefined'

.PHONY: all rpi rpi-docker rpi-host cross-sysroot docker-setup native test lint fmt fmt-check clean check-toolchain deploy

all: rpi

rpi: check-toolchain cross-sysroot rpi-host

cross-sysroot:
	@test -f $(CROSS_SYSROOT)/usr/lib/aarch64-linux-gnu/pkgconfig/libpulse.pc \
		|| $(CURDIR)/scripts/setup-cross-sysroot.sh

docker-setup:
	@command -v docker >/dev/null || (echo "Install Docker Desktop first." >&2; exit 1)
	@docker buildx inspect $(DOCKER_BUILDER) >/dev/null 2>&1 \
		|| docker buildx create --name $(DOCKER_BUILDER) --driver docker-container --bootstrap
	@docker buildx use $(DOCKER_BUILDER)
ifeq ($(UNAME_M),arm64)
	@echo "Apple Silicon ($(UNAME_M)): linux/arm64 builds run natively — no QEMU"
else
	@echo "Installing QEMU binfmt handlers for linux/arm64 on $(UNAME_M)..."
	@docker run --privileged --rm tonistiigi/binfmt --install all
endif
	@docker buildx inspect --bootstrap
	@echo "Docker buildx ready for $(DOCKER_PLATFORM)"

rpi-docker: docker-setup
	@mkdir -p target/$(RPI_TARGET)/release
	docker buildx build \
		--platform $(DOCKER_PLATFORM) \
		-f docker/Dockerfile.build \
		--output type=local,dest=target/$(RPI_TARGET)/release \
		.
	@test -f $(RPI_BIN)
	@file $(RPI_BIN)
	@echo "Built $(RPI_BIN)"

rpi-host:
	$(CROSS_ENV) cargo build --release --target $(RPI_TARGET)
	@test -f $(RPI_BIN)
	@file $(RPI_BIN)
	@echo "Built $(RPI_BIN)"

deploy: rpi
	scp $(RPI_BIN) $(PI_HOST):$(PI_DIR)/
	scp scripts/run.sh scripts/install-service.sh scripts/qc-voice-bridge.service $(PI_HOST):$(PI_DIR)/
	ssh $(PI_HOST) 'systemctl --user restart qc-voice-bridge && sleep 1 && systemctl --user status qc-voice-bridge --no-pager | head -20'

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
	rm -rf target/$(RPI_TARGET)/release/qc-voice-bridge

check-toolchain:
	@rustup target list --installed | grep -q '^$(RPI_TARGET)$$' \
		|| rustup target add $(RPI_TARGET)
	@if [ -z "$$CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER" ]; then \
		echo "No aarch64 Linux linker found."; \
		echo "  macOS: brew install messense/macos-cross-toolchains/aarch64-unknown-linux-gnu"; \
		echo "  Or: make rpi-docker"; \
		exit 1; \
	fi
