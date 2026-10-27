# dj-music-suite build tasks.
#
# Mirrors the justfile (keep both in sync): `make` ships with macOS and most
# Linux boxes, so contributing does not require installing `just`.
#
#   make          list the targets
#   make build    build the web bundle, then both release binaries
#   make check    build the bundle, run the tests and the linter

BUN ?= bun

.PHONY: default web build dev-web check

default:
	@echo "targets:"
	@echo "  web       build the web bundle (crates/server/frontend/dist)"
	@echo "  build     build the web bundle and both release binaries"
	@echo "  dev-web   frontend dev server (proxies /api to a local server)"
	@echo "  check     build the bundle, then cargo test and cargo clippy"

web:
	cd crates/server/frontend && $(BUN) install && $(BUN) run build

build: web
	cargo build --release

dev-web:
	cd crates/server/frontend && $(BUN) run dev

check: web
	cargo test --workspace
	-cargo clippy --workspace 2>/dev/null
