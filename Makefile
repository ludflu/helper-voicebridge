# qc-voice-bridge — build for Raspberry Pi 64-bit (aarch64 Linux)
#
# Usage:
#   make              # Docker buildx → Pi binary (recommended on macOS)
#   make rpi-docker   # same
#   make deploy       # build + scp + restart service on lilhelper
#   make rpi-host     # cross-compile on host (needs aarch64 linker + libpulse sysroot)
#   make native       # build for the current machine
#   make test         # run unit tests (host)
#   make clean
#
# Docker build (macOS → Pi, no Rust/libpulse on the Pi):
#   make docker-setup   # once: buildx builder + QEMU binfmt (Intel Mac)
#   make rpi-docker
#
# Pi native build (alternative):
#   sudo apt install libpulse-dev && cargo build --release
#
# Host cross-compile (legacy, libpulse cross-link is fragile):
#   brew install messense/macos-cross-toolchains/aarch64-unknown-linux-gnu
#   rustup target add aarch64-unknown-linux-gnu
#   make rpi-host

CRATE      := qc-voice-bridge
RPI_TARGET := aarch64-unknown-linux-gnu
RPI_BIN    := target/$(RPI_TARGET)/release/$(CRATE)
NATIVE_BIN := target/release/$(CRATE)
DOCKER_PLATFORM := linux/arm64
DOCKER_BUILDER  := qc-voice-bridge-builder
PI_HOST    ?= jsnavely@lilhelper
PI_DIR     ?= ~/helper-agent

CROSS_GCC  := aarch64-unknown-linux-gnu-gcc
CROSS_COMPILE ?=
ifneq ($(CROSS_COMPILE),)
  export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER := $(CROSS_COMPILE)gcc
else ifneq ($(shell command -v $(CROSS_GCC) 2>/dev/null),)
  export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER := $(CROSS_GCC)
else ifneq ($(shell command -v aarch64-linux-gnu-gcc 2>/dev/null),)
  export CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER := aarch64-linux-gnu-gcc
endif

.PHONY: all rpi rpi-docker rpi-host docker-setup native test lint fmt fmt-check clean check-toolchain deploy

all: rpi-docker

rpi: rpi-docker

docker-setup:
	@command -v docker >/dev/null || (echo "Install Docker Desktop first." >&2; exit 1)
	@docker buildx inspect $(DOCKER_BUILDER) >/dev/null 2>&1 \
		|| docker buildx create --name $(DOCKER_BUILDER) --driver docker-container --bootstrap
	@docker buildx use $(DOCKER_BUILDER)
	@echo "Installing QEMU binfmt handlers (needed on Intel Mac; harmless on Apple Silicon)..."
	@docker run --privileged --rm tonistiigi/binfmt --install all
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

rpi-host: check-toolchain
	cargo build --release --target $(RPI_TARGET)
	@echo "Built $(RPI_BIN)"

deploy: rpi-docker
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
ifndef CROSS
	@if [ -z "$$CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER" ]; then \
		echo "No aarch64 Linux linker found."; \
		echo "  Prefer: make rpi-docker"; \
		echo "  macOS: brew install messense/macos-cross-toolchains/aarch64-unknown-linux-gnu"; \
		echo "  Debian/Ubuntu: apt install gcc-aarch64-linux-gnu"; \
		exit 1; \
	fi
endif
