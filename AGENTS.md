# AGENTS.md

## Rust checks

- After making Rust code changes, always run:
  - `cargo fmt --all --check`
  - `cargo check`
  - `cargo clippy --all-targets --all-features -- -D warnings`
  - `cargo test`
