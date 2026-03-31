mod support;

use support::orbit_bin;

#[test]
fn init_requires_interactive_terminal() {
    let temp = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(orbit_bin())
        .current_dir(temp.path())
        .args(["--non-interactive", "init"])
        .output()
        .unwrap();

    assert!(!output.status.success());
    assert!(!temp.path().join("orbit.json").exists());

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("`orbit init` requires an interactive terminal"));
}
