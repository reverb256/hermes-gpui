## Summary

<!-- What changes, and why. Link the issue this closes. -->

Closes #

## Testing

<!-- Commands run and what they proved. -->

- [ ] `cargo fmt --all --check`
- [ ] `cargo clippy --workspace --all-targets -- -D warnings`
- [ ] `cargo test --workspace --all-targets`

## Checklist

- [ ] No credentials, tokens or host secrets in code, logs or fixtures.
- [ ] Wire changes match the host endpoints documented in `README.md`.
- [ ] UI changes follow `STYLE.md` and include a test where the rule is checkable.
- [ ] No new stubs; unavailable surfaces say so instead of pretending.
