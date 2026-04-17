use std::path::Path;
use std::process::Command;

pub fn create_api_key(path: &Path) {
    assert!(
        Command::new("openssl")
            .args([
                "genpkey",
                "-algorithm",
                "EC",
                "-pkeyopt",
                "ec_paramgen_curve:prime256v1",
                "-out",
                path.to_str().unwrap(),
            ])
            .status()
            .unwrap()
            .success()
    );
}
