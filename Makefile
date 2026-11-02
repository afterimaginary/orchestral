ready:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test --lib --bins --test cli

verify: ready
	cargo test
	cargo build --release
	if command -v claude >/dev/null; then claude plugin validate .; fi

.PHONY: ready verify
