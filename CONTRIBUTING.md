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

Orbit uses three test layers:

1. Unit tests inside `src/`
2. Mocked integration/e2e tests in `tests/`
3. Manual live Apple-account e2e tests in [`tests/apple/e2e_live_apple.rs`](/Users/ilyai/Developer/personal/orbit2/tests/apple/e2e_live_apple.rs)

The live Apple-account tests are intentionally `#[ignore]` and are never meant to run in CI.

## Running mocked e2e tests

Mocked e2e coverage is included in normal `cargo test`.

If you want to run only the integration suite:

```bash
cargo test --test e2e_run
cargo test --test e2e_signing
cargo test --test e2e_submit
cargo test --test e2e_lifecycle
```

## Running live Apple-account tests

These tests create real Apple Developer resources. Use a disposable bundle prefix and only run them against an account or team you control.

All live Apple scenarios live in [`tests/apple/e2e_live_apple.rs`](/Users/ilyai/Developer/personal/orbit2/tests/apple/e2e_live_apple.rs), but they compile into the `apple` integration target. To inspect the available ignored tests:

```bash
cargo test --test apple -- --ignored --list
```

To run any single live case:

```bash
cargo test --test apple e2e_live_apple::<test_name> -- --ignored --exact --nocapture
```

### Apple ID / Developer Services live suite

These tests exercise the GrandSlam/AuthKit + Developer Services path.

Required environment:

```bash
export ORBIT_RUN_LIVE_APPLE_E2E=1
export ORBIT_APPLE_ID=you@example.com
export ORBIT_APPLE_TEAM_ID=TEAMID1234
```

Optional:

```bash
export ORBIT_APPLE_PROVIDER_ID=123456789
export ORBIT_APPLE_PASSWORD='app-specific-or-account-password'
export ORBIT_LIVE_TEST_BUNDLE_PREFIX=dev.orbit.livee2e
```

Notes:

1. The helper seeds isolated `ORBIT_DATA_DIR` / `ORBIT_CACHE_DIR` paths per workspace, so live tests do not share local `.orbit` state with your normal machine state.
2. The Apple ID helpers explicitly clear `ORBIT_ASC_*` variables so API key auth cannot leak into the Developer Services path.
3. If Apple session-derived material has gone stale and a post-build helper starts failing with `developer services authkit bootstrap failed with 401 Unauthorized`, refresh it once with:

```bash
target/debug/orbit apple device list --refresh
```

4. The current-machine registration test only mutates Apple device records if `ORBIT_RUN_LIVE_APPLE_DEVICE_MUTATION_E2E=1` is set and the machine is not already registered.

Useful Apple ID live commands:

```bash
ORBIT_RUN_LIVE_APPLE_E2E=1 \
ORBIT_APPLE_TEAM_ID=<team-id> \
cargo test --test apple e2e_live_apple::live_developer_services_lists_configured_team -- --ignored --exact --nocapture

ORBIT_RUN_LIVE_APPLE_E2E=1 \
ORBIT_APPLE_TEAM_ID=<team-id> \
cargo test --test apple e2e_live_apple::live_build_sign_provision_and_clean_remote_state -- --ignored --exact --nocapture

ORBIT_RUN_LIVE_APPLE_E2E=1 \
ORBIT_APPLE_TEAM_ID=<team-id> \
cargo test --test apple e2e_live_apple::live_push_notifications_capability_syncs_to_bundle_id -- --ignored --exact --nocapture

ORBIT_RUN_LIVE_APPLE_E2E=1 \
ORBIT_APPLE_TEAM_ID=<team-id> \
cargo test --test apple e2e_live_apple::live_network_extension_capability_syncs_on_extension_bundle_id -- --ignored --exact --nocapture

ORBIT_RUN_LIVE_APPLE_E2E=1 \
ORBIT_APPLE_TEAM_ID=<team-id> \
cargo test --test apple e2e_live_apple::live_file_provider_extension_syncs_testing_mode_capability -- --ignored --exact --nocapture

ORBIT_RUN_LIVE_APPLE_E2E=1 \
ORBIT_APPLE_TEAM_ID=<team-id> \
cargo test --test apple e2e_live_apple::live_container_backed_capabilities_sync_reuse_and_remove -- --ignored --exact --nocapture
```

Coverage currently in the Apple ID suite:

- Smoke / cleanup:
  - `live_developer_services_lists_configured_team`
  - `live_build_sign_provision_and_clean_remote_state`
  - `live_clean_all_removes_remote_app_groups_and_merchants`
  - `live_clean_all_skips_forbidden_cloud_container_cleanup`
- Capability lifecycle:
  - `live_associated_domains_capability_removal_updates_remote_bundle_id`
  - `live_push_notifications_capability_syncs_to_bundle_id`
  - `live_network_extension_capability_syncs_on_extension_bundle_id`
  - `live_file_provider_extension_syncs_testing_mode_capability`
  - `live_container_backed_capabilities_sync_reuse_and_remove`
- Device registration:
  - `live_device_register_current_machine_uses_apple_id_auth`
- Multi-target signing:
  - `live_ios_app_clip_build_signs_host_and_clip_targets`
  - `live_ios_extension_build_signs_host_and_extension_targets`
  - `live_watch_companion_build_signs_host_watch_app_and_extension_targets`
- Provisioning reuse:
  - `live_ios_development_build_reuses_provisioning_profile_when_ios_devices_exist`
  - `live_ios_adhoc_build_reuses_provisioning_profile_when_ios_devices_exist`
  - `live_macos_development_build_reuses_provisioning_profile`
- Recovery:
  - `live_stale_local_certificate_state_recovers_on_next_build`
  - `live_missing_local_p12_recovers_on_next_build`
  - `live_revoked_test_owned_remote_certificate_recovers_on_next_build`
  - `live_macos_developer_id_installer_signing_recovers_missing_local_p12`
- Platform coverage:
  - `live_tvos_app_store_build_signs_target`
  - `live_visionos_app_store_build_signs_target`
- Submit / notary:
  - `live_submit_uses_real_app_store_connect_account`
  - `live_macos_developer_id_build_and_submit`

Destructive Apple ID cases:

- `live_revoked_test_owned_remote_certificate_recovers_on_next_build` deletes a remote distribution certificate only if the first build created a new test-owned certificate. If the build reuses a shared team certificate, the test safely skips the destructive branch.
- `live_macos_developer_id_build_and_submit` performs a real notarization submission. Apple may reject it with status code `7000` if notarization is not enabled for the team.

### App Store Connect API key live suite

These tests exercise the public ASC API key path instead of Developer Services.

Required environment:

```bash
export ORBIT_RUN_LIVE_ASC_E2E=1
export ORBIT_APPLE_TEAM_ID=TEAMID1234
export ORBIT_ASC_API_KEY_PATH=/absolute/path/to/AuthKey_XXXXXX.p8
export ORBIT_ASC_KEY_ID=XXXXXX1234
export ORBIT_ASC_ISSUER_ID=00000000-0000-0000-0000-000000000000
```

Optional:

```bash
export ORBIT_LIVE_TEST_BUNDLE_PREFIX=dev.orbit.livee2e
```

Notes:

1. `live_asc_command(...)` sets `ORBIT_ASC_*` and explicitly removes `ORBIT_APPLE_ID` / `ORBIT_APPLE_PROVIDER_ID`, so the subprocess cannot silently fall back to Apple ID auth.
2. The ASC tests still use isolated `ORBIT_DATA_DIR` / `ORBIT_CACHE_DIR` workspaces and normal best-effort cleanup on drop.
3. `ASC` distribution certificate rotation is destructive by design. Apple will send a certificate-revocation email when that test deletes the test-owned certificate.

Useful ASC live commands:

```bash
ORBIT_RUN_LIVE_ASC_E2E=1 \
ORBIT_APPLE_TEAM_ID=TEAMID1234 \
ORBIT_ASC_API_KEY_PATH=/absolute/path/to/AuthKey_XXXXXX.p8 \
ORBIT_ASC_KEY_ID=XXXXXX1234 \
ORBIT_ASC_ISSUER_ID=00000000-0000-0000-0000-000000000000 \
cargo test --test apple e2e_live_apple::live_asc_ios_app_store_build_signs_target -- --ignored --exact --nocapture

ORBIT_RUN_LIVE_ASC_E2E=1 \
ORBIT_APPLE_TEAM_ID=TEAMID1234 \
ORBIT_ASC_API_KEY_PATH=/absolute/path/to/AuthKey_XXXXXX.p8 \
ORBIT_ASC_KEY_ID=XXXXXX1234 \
ORBIT_ASC_ISSUER_ID=00000000-0000-0000-0000-000000000000 \
cargo test --test apple e2e_live_apple::live_asc_distribution_certificate_rotation_recovers_after_remote_delete -- --ignored --exact --nocapture
```

### Submit-only live command

This test intentionally uses a separate enable flag because it uploads a real build to Apple:

```bash
ORBIT_RUN_LIVE_APPLE_SUBMIT_E2E=1 \
cargo test --test apple e2e_live_apple::live_submit_uses_real_app_store_connect_account -- --ignored --exact --nocapture
```

## Cleanup expectations

Live tests use a best-effort cleanup guard:

- most live tests attempt `orbit clean --all` on drop
- the submit test only runs `orbit clean --local`

This split is intentional. After a real submit, Apple may keep the App Store Connect app record or explicit App ID, so full remote rollback is not always possible.

`orbit clean --all` is intentionally conservative:

- it removes Orbit-managed profiles, bundle IDs, app groups, merchant IDs, and iCloud containers
- it removes local signing material from `.orbit`
- it does not revoke remote signing certificates
- it currently treats `cloudContainers` specially when Apple rejects deletion with a `403`; the corresponding live test documents that backend limitation instead of treating it as an Orbit failure
