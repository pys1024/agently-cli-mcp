# agently-cli-mcp build and release helpers.

CARGO ?= cargo
STRIP ?= strip

# Default build target = the host triple (what `cargo build` builds by default).
HOST_TARGET := $(shell rustc -vV | sed -n 's/^host: //p')

ifeq ($(OS),Windows_NT)
  BIN_NAME := agently-cli-mcp.exe
  ARCHIVE := agently-cli-mcp-$(HOST_TARGET).zip
  BUILD_ENV := RUSTFLAGS="-C target-feature=+crt-static"
else
  BIN_NAME := agently-cli-mcp
  ARCHIVE := agently-cli-mcp-$(HOST_TARGET).tar.gz
  BUILD_ENV :=
endif

DIST_DIR := dist

.PHONY: all build test lint fmt fmt-check clippy dist clean

all: build

build: ## Build the release binary for the host target
	$(BUILD_ENV) $(CARGO) build --release

test: ## Run the test suite (test fixtures included)
	$(CARGO) test --features test-fixtures

fmt: ## Format all sources
	$(CARGO) fmt --all

fmt-check: ## Fail if any source needs formatting
	$(CARGO) fmt --all --check

clippy: ## Lint with clippy, treating warnings as errors
	$(CARGO) clippy --all-targets --features test-fixtures -- -D warnings

lint: fmt-check clippy ## Run all lint checks

dist: build ## Package the host-target release archive + checksum into dist/
	@mkdir -p "$(DIST_DIR)"
	@rm -f "$(DIST_DIR)/$(ARCHIVE)" "$(DIST_DIR)/checksums.txt"
ifeq ($(OS),Windows_NT)
	powershell -NoProfile -Command "Compress-Archive -Path 'target/release/$(BIN_NAME)' -DestinationPath '$(DIST_DIR)/$(ARCHIVE)'"
else
	$(STRIP) "target/release/$(BIN_NAME)"
	tar -czf "$(DIST_DIR)/$(ARCHIVE)" -C target/release "$(BIN_NAME)"
endif
	@cd "$(DIST_DIR)" && sha256sum "$(ARCHIVE)" > checksums.txt
	@echo "built $(DIST_DIR)/$(ARCHIVE)"

clean:
	$(CARGO) clean
	rm -rf "$(DIST_DIR)"
