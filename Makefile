ready:
	cargo fmt --check
	cargo clippy --all-targets -- -D warnings
	cargo test --lib --bins --test cli

verify: ready
	cargo test
	cargo build --release
	if command -v claude >/dev/null; then claude plugin validate .; fi

# Real runs against the installed claude. Never part of verify or CI.
e2e:
	cargo test --test e2e -- --ignored --nocapture --test-threads=1 compatibility local_runner

sandbox:
	cargo test --test e2e -- --ignored --nocapture sandbox_runner

.PHONY: ready verify e2e sandbox
