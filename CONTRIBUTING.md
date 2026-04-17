# Contributing

## Development checks

Run the standard local checks before opening a change:

```bash
cargo fmt --all --check
cargo check
cargo clippy --all-targets --all-features -- -D warnings
cargo test
```

CI runs the same checks in [`.github/workflows/ci.yml`](/Users/ilyai/Developer/personal/orbit2/.github/workflows/ci.yml).

## Test layers

Orbit uses these test layers:

1. Unit tests inside `src/`
2. Mocked Apple integration/e2e tests in [`tests/apple/main.rs`](/Users/ilyai/Developer/personal/orbit2/tests/apple/main.rs)
3. Small top-level e2e coverage like [`tests/e2e_init.rs`](/Users/ilyai/Developer/personal/orbit2/tests/e2e_init.rs)

## Running mocked e2e tests

Mocked Apple e2e coverage is included in normal `cargo test`.

If you want to run only the Apple integration suite:

```bash
cargo test --test apple
```

## Manual ASC verification

Orbit now uses the embedded `asc` section in `orbit.json` for Apple account state. For manual verification outside the mocked suite, the relevant commands are:

```bash
orbit asc validate
orbit asc plan
orbit asc apply
orbit asc signing import
```
