.PHONY: all build frontend check test release run dev clean smoke
all: release

frontend:
	cd frontend && npm ci && npm run build

build: frontend
	cargo build --locked

release: frontend
	cargo build --release --locked

check:
	cd frontend && npm ci && npm run check && npm run lint && npm test && npm run build
	cargo fmt --all -- --check
	cargo clippy --locked --all-targets -- -D warnings
	cargo test --locked
	cargo test --locked -p webrtc-ice --lib turn_stream
	python3 -B distribution/test_release.py
	python3 -B distribution/test_relay_launcher.py
	python3 -B distribution/test_main_channel.py
	python3 -B scripts/test_main_ci.py
	python3 -B distribution/test_relay_catalog.py
	python3 -B scripts/relay-e2e/test_static.py

test:
	cd frontend && npm test
	cargo test --locked

# One process, embedded assets. No Vite server is required.
run: release
	./target/release/herdr-webapp

# Rebuild frontend first; --core-socket can point to an isolated development core.
dev: frontend
	cargo run --locked -- --port 9001

smoke:
	uv run --script scripts/smoke.py --binary "$(CURDIR)/target/release/herdr-webapp"

clean:
	cargo clean
	rm -rf frontend/dist
