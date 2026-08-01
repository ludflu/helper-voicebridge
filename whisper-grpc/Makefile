PROTO_DIR := proto
GEN_DIR := src/whisper_grpc
PROTO_FILE := $(PROTO_DIR)/transcribe.proto

# Keep .venv on APFS — exFAT (external drives) sprinkles AppleDouble `._*` files
# that break transformers' import scanner (UnicodeDecodeError on `make run`).
VENV_DIR ?= $(HOME)/.local/venvs/whisper-grpc
export UV_PROJECT_ENVIRONMENT := $(VENV_DIR)

.PHONY: all proto install run test lint fmt clean venv-clean

all: proto

proto:
	uv run python -m grpc_tools.protoc \
		-I$(PROTO_DIR) \
		--python_out=$(GEN_DIR) \
		--grpc_python_out=$(GEN_DIR) \
		$(PROTO_FILE)
	@# protoc generates absolute imports; use package-relative imports.
	sed -i '' 's/^import transcribe_pb2/from whisper_grpc import transcribe_pb2/' \
		$(GEN_DIR)/transcribe_pb2_grpc.py 2>/dev/null || \
	sed -i 's/^import transcribe_pb2/from whisper_grpc import transcribe_pb2/' \
		$(GEN_DIR)/transcribe_pb2_grpc.py

install: proto
	@rm -rf .venv
	uv sync

run: install
	PYTHONUNBUFFERED=1 GRPC_VERBOSITY=ERROR WHISPER_DEVICE=$${WHISPER_DEVICE:-auto} uv run whisper-grpc-server

check-import: install
	uv run python -c "from whisper_grpc.server import main; print('import ok')"

test: proto
	uv run pytest

lint: proto
	uv run ruff check src tests

fmt:
	uv run black src tests

clean:
	rm -f $(GEN_DIR)/transcribe_pb2.py $(GEN_DIR)/transcribe_pb2_grpc.py

venv-clean:
	rm -rf .venv $(VENV_DIR)
