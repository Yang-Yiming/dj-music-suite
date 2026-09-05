# dj-music-suite build tasks
default:
    @just --list

# install frontend deps + build the web bundle (crates/server/frontend/dist)
web:
    cd crates/server/frontend && bun install && bun run build

# build the web bundle and both release binaries
build: web
    cargo build --release

# frontend dev server (proxies /api to a locally running dj-music-suite-web)
dev-web:
    cd crates/server/frontend && bun run dev

# run everything the CI would
check: web
    cargo test --workspace
    cargo clippy --workspace 2>/dev/null || true
