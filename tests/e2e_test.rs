mod support;

use std::fs;

use support::{
    base_command, create_home, create_testing_swift_mock, create_testing_workspace, read_log,
    run_and_capture,
};
use tempfile::tempdir;

#[test]
fn orbit_test_runs_swift_testing_for_manifest_unit_tests() {
    let temp = tempdir().unwrap();
    let home = create_home(temp.path());
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("mock.log");
    fs::create_dir_all(&mock_bin).unwrap();
    create_testing_swift_mock(&mock_bin);
    let workspace = create_testing_workspace(temp.path());

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.arg("test");
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let log = read_log(&log_path);
    assert!(log.contains("swift test --disable-keychain --package-path"));
    assert!(log.contains("--enable-swift-testing"));
    assert!(log.contains("--disable-xctest"));

    let package_root = workspace.join(".orbit/tests/swift-testing/package");
    let package_manifest = fs::read_to_string(package_root.join("Package.swift")).unwrap();
    assert!(package_manifest.contains(".executableTarget("));
    assert!(package_manifest.contains("name: \"ExampleApp\""));
    assert!(package_manifest.contains(".testTarget("));
    assert!(package_manifest.contains("name: \"ExampleAppUnitTests\""));
    assert!(
        package_root
            .join("Targets/ExampleApp/Sources/input-0")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(
        package_root
            .join("Targets/ExampleAppUnitTests/Sources/input-0")
            .symlink_metadata()
            .unwrap()
            .file_type()
            .is_symlink()
    );
}
