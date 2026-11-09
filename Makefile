CARGO ?= cargo

.PHONY: check fmt clippy test run release clean

# Mirrors the CI workflow so a red badge can always be reproduced locally.
check: fmt clippy test

fmt:
	$(CARGO) fmt --all --check

clippy:
	$(CARGO) clippy --workspace --all-targets -- -D warnings

test:
	$(CARGO) test --workspace --all-targets

run:
	$(CARGO) run -p hermes-desktop

release:
	$(CARGO) build --release --locked -p hermes-desktop

clean:
	$(CARGO) clean
