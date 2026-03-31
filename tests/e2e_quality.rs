mod support;

use std::fs;

use serde_json::json;

use support::{
    base_command, create_build_xcrun_mock, create_home, create_mixed_language_workspace,
    create_quality_swift_mock, create_signing_workspace, create_watch_workspace, read_log,
    run_and_capture,
};

#[test]
fn lint_runs_swiftlint_and_semantic_analysis_by_default() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_signing_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_quality_swift_mock(&mock_bin);

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "lint",
    ]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log = read_log(&log_path);
    assert!(log.contains("swift build --disable-keychain --package-path"));
    assert!(log.contains("--product orbit-swiftlint"));
    assert!(log.contains("orbit-swiftlint "));
    assert!(log.contains("xcrun --sdk iphonesimulator --show-sdk-path"));
    assert!(log.contains("\"compiler_invocations\""));
    assert!(log.contains("\"arguments\""));
    assert!(log.contains("\"swiftc\""));
    assert!(log.contains("\"-sdk\""));
    assert!(log.contains("\"ExampleApp\""));
    assert!(log.contains("Sources/App/App.swift"));
}

#[test]
fn lint_platform_flag_limits_semantic_analysis_scope() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_watch_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_quality_swift_mock(&mock_bin);

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "lint",
        "--platform",
        "ios",
    ]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log = read_log(&log_path);
    assert!(log.contains("swift build --disable-keychain --package-path"));
    assert!(log.contains("--product orbit-swiftlint"));
    assert!(log.contains("orbit-swiftlint "));
    assert!(log.contains("xcrun --sdk iphonesimulator --show-sdk-path"));
    assert!(log.contains("\"swiftc\""));
    assert!(log.contains("\"-sdk\""));
    assert!(log.contains("\"WatchFixture\""));
    assert!(!log.contains("xcrun --sdk watchsimulator --show-sdk-path"));
    assert!(!log.contains("\"WatchApp\""));
    assert!(log.contains("Sources/App/App.swift"));
}

#[test]
fn lint_runs_compiler_backed_c_family_diagnostics_for_mixed_targets() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_mixed_language_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_quality_swift_mock(&mock_bin);

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "lint",
    ]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log = read_log(&log_path);
    assert!(log.contains("orbit-swiftlint "));
    assert!(log.contains("xcrun --sdk iphonesimulator clang"));
    assert!(log.contains("-fsyntax-only"));
    assert!(log.contains("Sources/App/Bridge.m"));
    assert!(!log.contains("Bridge.m.o"));
}

#[test]
fn format_defaults_to_read_only_check() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_signing_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    fs::create_dir_all(&mock_bin).unwrap();

    create_quality_swift_mock(&mock_bin);

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "format",
    ]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log = read_log(&log_path);
    assert!(log.contains("swift build --disable-keychain --package-path"));
    assert!(log.contains("--product orbit-swift-format"));
    assert!(log.contains("orbit-swift-format "));
    assert!(log.contains("\"mode\": \"check\""));
    assert!(!log.contains("\"mode\": \"write\""));
    assert!(log.contains("Sources/App/App.swift"));
}

#[test]
fn format_write_runs_swift_format_in_place() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_signing_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    fs::create_dir_all(&mock_bin).unwrap();

    create_quality_swift_mock(&mock_bin);

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "--manifest",
        workspace.join("orbit.json").to_str().unwrap(),
        "format",
        "--write",
    ]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log = read_log(&log_path);
    assert!(log.contains("swift build --disable-keychain --package-path"));
    assert!(log.contains("--product orbit-swift-format"));
    assert!(log.contains("orbit-swift-format "));
    assert!(log.contains("\"mode\": \"write\""));
    assert!(log.contains("Sources/App/App.swift"));
}

#[test]
fn lint_reads_orbit_json_rules_and_ignore_globs() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_signing_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    let sdk_root = temp.path().join("sdk");
    fs::create_dir_all(&mock_bin).unwrap();
    fs::create_dir_all(workspace.join("Sources/App/Generated")).unwrap();
    fs::write(
        workspace.join("Sources/App/Generated/Ignored.swift"),
        "import Foundation\nlet ignored = 1\n",
    )
    .unwrap();

    let manifest_path = workspace.join("orbit.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["quality"] = json!({
        "lint": {
            "ignore": ["**/Generated/**"],
            "rules": {
                "unused_import": "error",
                "trailing_whitespace": ["warn", { "ignores_empty_lines": true }]
            }
        }
    });
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    create_build_xcrun_mock(&mock_bin, &sdk_root);
    create_quality_swift_mock(&mock_bin);

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "--manifest",
        manifest_path.to_str().unwrap(),
        "lint",
    ]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log = read_log(&log_path);
    assert!(log.contains("\"configuration_json\""));
    assert!(log.contains("\\\"unused_import\\\":\\\"error\\\""));
    assert!(
        log.contains(
            "\\\"trailing_whitespace\\\":[\\\"warn\\\",{\\\"ignores_empty_lines\\\":true}]"
        )
    );
    assert!(log.contains("Sources/App/App.swift"));
    assert!(!log.contains("Sources/App/Generated/Ignored.swift"));
}

#[test]
fn format_reads_editorconfig_rules_and_ignore_globs_from_orbit_json() {
    let temp = tempfile::tempdir().unwrap();
    let workspace = create_signing_workspace(temp.path());
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("commands.log");
    fs::create_dir_all(&mock_bin).unwrap();
    fs::create_dir_all(workspace.join("Sources/App/Generated")).unwrap();
    fs::write(
        workspace.join("Sources/App/Generated/Ignored.swift"),
        "import Foundation\nlet ignored = 1\n",
    )
    .unwrap();
    fs::write(
        workspace.join(".editorconfig"),
        "root = true\n\n[*.swift]\nindent_style = space\nindent_size = 4\ntab_width = 4\nmax_line_length = 120\n",
    )
    .unwrap();

    let manifest_path = workspace.join("orbit.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&fs::read(&manifest_path).unwrap()).unwrap();
    manifest["quality"] = json!({
        "format": {
            "ignore": ["**/Generated/**"],
            "editorconfig": true,
            "rules": {
                "NoAssignmentInExpressions": "off",
                "indentSwitchCaseLabels": true
            }
        }
    });
    fs::write(
        &manifest_path,
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();

    create_quality_swift_mock(&mock_bin);

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args([
        "--non-interactive",
        "--manifest",
        manifest_path.to_str().unwrap(),
        "format",
    ]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log = read_log(&log_path);
    assert!(log.contains("\"configuration_json\""));
    assert!(log.contains("\\\"lineLength\\\":120"));
    assert!(log.contains("\\\"indentation\\\":{\\\"spaces\\\":4}"));
    assert!(log.contains("\\\"tabWidth\\\":4"));
    assert!(log.contains("\\\"rules\\\":{\\\"NoAssignmentInExpressions\\\":false}"));
    assert!(log.contains("\\\"indentSwitchCaseLabels\\\":true"));
    assert!(log.contains("Sources/App/App.swift"));
    assert!(!log.contains("Sources/App/Generated/Ignored.swift"));
}
