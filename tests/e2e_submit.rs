mod support;

use std::fs;

use support::{
    base_command, create_api_key, create_build_xcrun_mock, create_home, create_signing_workspace,
    latest_receipt_path, read_log, run_and_capture, spawn_asc_mock,
};

#[test]
fn submit_uses_existing_receipt_without_rebuilding() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_signing_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();
    create_build_xcrun_mock(&mock_bin, &sdk_root);

    let api_key_path = temp.path().join("AuthKey_TEST.p8");
    create_api_key(&api_key_path);
    let artifact_path = workspace.join("ExampleApp.ipa");
    fs::write(&artifact_path, b"ipa").unwrap();
    let receipt_dir = workspace.join(".orbit/receipts");
    fs::create_dir_all(&receipt_dir).unwrap();
    let receipt_path = receipt_dir.join("receipt.json");
    fs::write(
        &receipt_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "id": "receipt-1",
            "target": "ExampleApp",
            "platform": "ios",
            "configuration": "release",
            "distribution": "app-store",
            "destination": "device",
            "bundle_id": "dev.orbit.fixture",
            "bundle_path": workspace.join("ExampleApp.app"),
            "artifact_path": artifact_path,
            "created_at_unix": 1,
            "submit_eligible": true
        }))
        .unwrap(),
    )
    .unwrap();

    let server = spawn_asc_mock(
        temp.path(),
        "TEAM123456",
        "dev.orbit.fixture",
        "ExampleApp",
        true,
    );
    let mut submit = base_command(&workspace, &home, &mock_bin, &log_path);
    submit.env("ORBIT_ASC_BASE_URL", &server.base_url);
    submit.env("ORBIT_ASC_API_KEY_PATH", &api_key_path);
    submit.env("ORBIT_ASC_KEY_ID", "KEY1234567");
    submit.env(
        "ORBIT_ASC_ISSUER_ID",
        "00000000-0000-0000-0000-000000000000",
    );
    submit.env("ORBIT_APPLE_TEAM_ID", "TEAM123456");
    submit.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "submit",
        "--receipt",
        latest_receipt_path(&workspace).to_str().unwrap(),
    ]);
    let submit_output = run_and_capture(&mut submit);
    let requests = server.requests();
    server.shutdown();

    assert!(
        submit_output.status.success(),
        "{}",
        String::from_utf8_lossy(&submit_output.stderr)
    );
    let log = read_log(&log_path);
    assert!(log.contains("xcrun altool --validate-app"));
    assert!(log.contains("xcrun altool --upload-package"));
    assert!(!log.contains("swiftc"));
    assert!(
        requests
            .iter()
            .any(|request| request.starts_with("GET /v1/bundleIds"))
    );
    assert!(
        requests
            .iter()
            .any(|request| request.starts_with("GET /v1/apps"))
    );
    assert!(
        requests
            .iter()
            .any(|request| request.starts_with("POST /v1/apps"))
    );
}
