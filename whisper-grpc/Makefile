PROTO_DIR := proto
GEN_DIR := src/whisper_grpc
PROTO_FILE := $(PROTO_DIR)/transcribe.proto

.PHONY: all proto install run test lint fmt clean

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
	uv sync

run: install
	WHISPER_DEVICE=$${WHISPER_DEVICE:-auto} uv run whisper-grpc-server

test: proto
	uv run pytest

lint: proto
	uv run ruff check src tests

fmt:
	uv run black src tests

clean:
	rm -f $(GEN_DIR)/transcribe_pb2.py $(GEN_DIR)/transcribe_pb2_grpc.py
