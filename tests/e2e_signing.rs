mod support;

use std::fs;

use support::{
    base_command, create_home, create_p12, create_security_mock, create_signing_workspace,
    read_log, run_and_capture,
};

#[test]
fn signing_import_export_and_clean_round_trip() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_signing_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let security_db = temp.path().join("security-db.txt");
    fs::create_dir_all(&mock_bin).unwrap();

    create_security_mock(&mock_bin, &security_db);

    let p12_path = create_p12(&temp.path().join("identity"), "secret");

    let mut import = base_command(&workspace, &home, &mock_bin, &log_path);
    import.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "apple",
        "signing",
        "import",
        "--platform",
        "ios",
        "--distribution",
        "development",
        "--p12",
        p12_path.to_str().unwrap(),
        "--password",
        "secret",
    ]);
    let import_output = run_and_capture(&mut import);
    assert!(
        import_output.status.success(),
        "{}",
        String::from_utf8_lossy(&import_output.stderr)
    );

    let state_path = home.join("Library/Application Support/orbit/teams/TEAM123456/signing.json");
    let mut signing_state: serde_json::Value =
        serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    let certificate_id = signing_state["certificates"][0]["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let profile_path = home.join(
        "Library/Application Support/orbit/teams/TEAM123456/profiles/fixture.mobileprovision",
    );
    fs::create_dir_all(profile_path.parent().unwrap()).unwrap();
    fs::write(&profile_path, b"fixture-profile").unwrap();
    signing_state["profiles"] = serde_json::json!([{
        "id": "PROFILE-1",
        "profile_type": "limited",
        "bundle_id": "dev.orbit.fixture",
        "path": profile_path,
        "uuid": "UUID-1",
        "certificate_ids": [certificate_id],
        "device_ids": []
    }]);
    fs::write(
        &state_path,
        serde_json::to_vec_pretty(&signing_state).unwrap(),
    )
    .unwrap();

    let export_dir = temp.path().join("exported-signing");
    let mut export = base_command(&workspace, &home, &mock_bin, &log_path);
    export.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "apple",
        "signing",
        "export",
        "--platform",
        "ios",
        "--distribution",
        "development",
        "--output-dir",
        export_dir.to_str().unwrap(),
    ]);
    let export_output = run_and_capture(&mut export);
    assert!(
        export_output.status.success(),
        "{}",
        String::from_utf8_lossy(&export_output.stderr)
    );
    let stdout = String::from_utf8_lossy(&export_output.stdout);
    assert!(stdout.contains("p12_password: secret"));
    assert!(
        export_dir
            .join("ExampleApp-ios-development-debug.p12")
            .exists()
    );
    assert!(
        export_dir
            .join("ExampleApp-ios-development-debug.mobileprovision")
            .exists()
    );

    fs::create_dir_all(workspace.join(".orbit/build")).unwrap();
    fs::write(workspace.join(".orbit/build/marker"), b"build").unwrap();

    let mut clean = base_command(&workspace, &home, &mock_bin, &log_path);
    clean.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "clean",
        "--local",
    ]);
    let clean_output = run_and_capture(&mut clean);
    assert!(
        clean_output.status.success(),
        "{}",
        String::from_utf8_lossy(&clean_output.stderr)
    );
    assert!(!workspace.join(".orbit").exists());

    let mut second_export = base_command(&workspace, &home, &mock_bin, &log_path);
    second_export.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "apple",
        "signing",
        "export",
        "--platform",
        "ios",
        "--distribution",
        "development",
        "--output-dir",
        export_dir.to_str().unwrap(),
    ]);
    let second_export_output = run_and_capture(&mut second_export);
    assert!(!second_export_output.status.success());
}

#[test]
fn push_auth_key_export_copies_team_scoped_p8() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_signing_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    fs::create_dir_all(&mock_bin).unwrap();

    let team_dir = home.join("Library/Application Support/orbit/teams/TEAM123456");
    let push_keys_dir = team_dir.join("push-keys");
    fs::create_dir_all(&push_keys_dir).unwrap();
    let push_key_path = push_keys_dir.join("PUSHKEY123.p8");
    fs::write(&push_key_path, b"push-auth-key").unwrap();
    fs::write(
        team_dir.join("signing.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "certificates": [],
            "profiles": [],
            "push_keys": [{
                "id": "PUSHKEY123",
                "name": "@orbit/apns",
                "path": push_key_path
            }],
            "push_certificates": []
        }))
        .unwrap(),
    )
    .unwrap();

    let export_path = temp.path().join("AuthKey_PUSHKEY123.p8");
    let mut export = base_command(&workspace, &home, &mock_bin, &log_path);
    export.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "apple",
        "signing",
        "export-push",
        "--output",
        export_path.to_str().unwrap(),
    ]);
    let output = run_and_capture(&mut export);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("team_id: TEAM123456"));
    assert!(stdout.contains("key_id: PUSHKEY123"));
    assert_eq!(fs::read(&export_path).unwrap(), b"push-auth-key");
    let _ = read_log(&log_path);
}
