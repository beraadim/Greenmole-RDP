.PHONY: help build build-cli build-ui build-ui-rust build-ui-frontend \
       dev dev-cli dev-ui clean clean-cli clean-ui \
       install run-cli run-cli-ipc run-ui check release release-fast link-cli

# Default target
help:
	@echo "Greenmole RDP Makefile"
	@echo ""
	@echo "  Development:"
	@echo "    make install          Install frontend dependencies"
	@echo "    make build-cli        Build CLI"
	@echo "    make dev-ui           Start UI development mode (Tauri)"
	@echo "    make dev-front        Start only the frontend dev server"
	@echo "    make run-cli          Run CLI (port 3389)"
	@echo "    make run-cli-ipc      Legacy CLI alias (private IPC is automatic)"
	@echo "    make check            Run all checks"
	@echo ""
	@echo "  Release:"
	@echo "    make release          Full release build (optimized CLI+UI, DMG)"
	@echo "    make release-fast     Fast release build (unoptimized, DMG)"
	@echo ""
	@echo "  Clean:"
	@echo "    make clean            Remove all build artifacts"
	@echo "    make clean-cli        Clean only CLI artifacts"
	@echo "    make clean-ui         Clean only UI artifacts"

# === CLI ===

link-cli:
	@ln -sf ../../target/debug/macrdp-server macrdp-ui/src-tauri/macrdp-server
	@mkdir -p macrdp-ui/src-tauri/target/debug
	@ln -sf ../../../../target/debug/macrdp-server macrdp-ui/src-tauri/target/debug/macrdp-server

build-cli: link-cli
	cargo build -p macrdp-server

run-cli:
	cargo run -p macrdp-server


# The supervised server now owns private IPC automatically.
run-cli-ipc: run-cli

clean-cli:
	cargo clean

# === UI ===

install:
	cd macrdp-ui && npm install

build-ui-rust:
	cargo build --manifest-path macrdp-ui/src-tauri/Cargo.toml

build-ui-front:
	cd macrdp-ui && npm run build

dev-ui: build-cli
	cd macrdp-ui && npm run tauri dev

dev-front:
	cd macrdp-ui && npm run dev

clean-ui:
	rm -rf macrdp-ui/dist macrdp-ui/src-tauri/target

# === Release ===

BUNDLE_ID := com.macrdp.app
SIGN_IDENTITY ?= -
APP_BUNDLE := macrdp-ui/src-tauri/target/release/bundle/macos/Greenmole RDP.app

# Tauri signing environment for certificate-based builds
export MACRDP_SIGNING_IDENTITY ?= $(SIGN_IDENTITY)

# Full release: optimized build, signing and DMG packaging
release:
	@echo "=== Full release build (signing: $(MACRDP_SIGNING_IDENTITY)) ==="
	cd macrdp-ui && BUNDLE_ID=$(BUNDLE_ID) MACRDP_CLI_PROFILE=release ./build.sh
	@echo ""
	@echo "=== Build complete ==="
	@ls -lh macrdp-ui/src-tauri/target/release/bundle/dmg/*.dmg

# Fast release: unoptimized build, signing and DMG packaging
release-fast:
	@echo "=== Fast release build (signing: $(MACRDP_SIGNING_IDENTITY)) ==="
	cd macrdp-ui && BUNDLE_ID=$(BUNDLE_ID) MACRDP_CLI_PROFILE=debug ./build.sh
	@echo ""
	@echo "=== Build complete ==="
	@ls -lh macrdp-ui/src-tauri/target/debug/bundle/dmg/*.dmg

# === Global ===

build: build-cli build-ui-front build-ui-rust

clean: clean-cli clean-ui

check:
	./tools/check.sh
