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
	python3 distribution/test_release.py

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
