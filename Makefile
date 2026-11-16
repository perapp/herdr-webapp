.PHONY: all build frontend frontend-deps setup check check-fast ui-test ui-watch test release run dev dev-build dev-embedded clean smoke ui-e2e audit help
all: release

help:
	@printf '%s\n' 'make dev                 Vite HMR + disposable Core (no live user terminals)' 'make check-fast          Frontend types, lint and tests; no Rust/release build' 'make ui-test TEST=fontSize.test.ts   Focused frontend tests' 'make ui-watch TEST=fontSize.test.ts  Focused watch loop' 'make dev-build           Rebuild debug backend after Rust changes' 'make setup               Force a clean locked dependency install' 'make check && make release && make smoke  Full completion gates'

frontend-deps:
	python3 -B scripts/frontend-deps.py

setup:
	python3 -B scripts/frontend-deps.py --force

frontend: frontend-deps
	cd frontend && npm run build

# Fast inner loop: cached dependencies, no embedded-assets or Rust build.
check-fast: frontend-deps
	cd frontend && npm run check && npm run lint && npm test

ui-test: frontend-deps
	cd frontend && npm test -- $(TEST)

ui-watch: frontend-deps
	cd frontend && npx --no-install vitest --watch $(TEST)

build: frontend
	cargo build --locked

release: frontend
	cargo build --release --locked

# Full gate intentionally starts from a clean locked install.
check: setup
	cd frontend && npm run check && npm run lint && npm test && npm run build
	cargo fmt --all -- --check
	cargo clippy --locked --all-targets -- -D warnings
	cargo test --locked
	cargo test --locked -p webrtc-ice --lib turn_stream
	python3 -B distribution/test_core_bootstrap.py
	python3 -B distribution/test_release.py
	python3 -B scripts/test_release_ci.py
	python3 -B distribution/test_relay_launcher.py
	python3 -B distribution/test_main_channel.py
	python3 -B scripts/test_main_ci.py
	python3 -B distribution/test_relay_catalog.py
	python3 -B scripts/relay-e2e/test_static.py
	python3 -B scripts/test_frontend_deps.py
	python3 -B scripts/test_dev.py

test:
	cd frontend && npm test
	cargo test --locked

# One process, embedded assets. No Vite server is required.
run: release
	./target/release/herdr-webapp

# Vite reads source directly; frontend edits never restart/recompile Rust.
dev: frontend-deps
	python3 -B scripts/dev.py $(DEV_ARGS)

# Bootstrap embedded assets once; repeat explicitly after native source changes.
dev-build: frontend-deps
	@test -f frontend/dist/index.html || (cd frontend && npm run build)
	cargo build --locked

# Explicit legacy embedded debug path; caller must select an isolated Core.
dev-embedded: frontend
	cargo run --locked -- --port 9001 $(ARGS)

smoke:
	python3 -B scripts/test_terminal_provenance.py
	python3 -B scripts/test_access_privacy.py --binary "$(CURDIR)/target/release/herdr-webapp"
	uv run --script scripts/smoke.py --binary "$(CURDIR)/target/release/herdr-webapp"
	uv run --script scripts/ui-e2e.py --binary "$(CURDIR)/target/release/herdr-webapp"

# Phone and desktop browser behaviour against a disposable Core: keyboard,
# scrolling, selection, attach, key row, row menus and Settings. Needs Chrome.
ui-e2e: release
	uv run --script scripts/ui-e2e.py --binary "$(CURDIR)/target/release/herdr-webapp"

clean:
	cargo clean
	rm -rf frontend/dist

# Needs network: fetches the RustSec advisory database and the npm registry.
# Accepted advisories, with reasons, live in .cargo/audit.toml.
audit: frontend-deps
	@command -v cargo-audit >/dev/null || cargo install --locked cargo-audit
	cargo audit
	cd frontend && npm audit --omit=dev
