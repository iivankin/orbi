use anyhow::Result;

use crate::apple::xcode::{
    SelectedXcode, log_redirect_dylib_path as selected_xcode_log_redirect_dylib_path,
};

pub(crate) fn macos_quit_applescript(bundle_id: &str) -> String {
    format!("tell application id \"{}\" to quit", bundle_id)
}

pub(crate) fn macos_xcode_log_redirect_env(
    selected_xcode: Option<&SelectedXcode>,
) -> Result<String> {
    Ok(macos_xcode_log_redirect_environment(selected_xcode)?
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join(" "))
}

pub(crate) fn macos_xcode_log_redirect_environment(
    selected_xcode: Option<&SelectedXcode>,
) -> Result<Vec<(String, String)>> {
    let log_redirect_dylib = selected_xcode_log_redirect_dylib_path(selected_xcode)?;
    Ok(vec![
        ("NSUnbufferedIO".to_owned(), "YES".to_owned()),
        ("OS_LOG_TRANSLATE_PRINT_MODE".to_owned(), "0x80".to_owned()),
        (
            "IDE_DISABLED_OS_ACTIVITY_DT_MODE".to_owned(),
            "1".to_owned(),
        ),
        ("OS_LOG_DT_HOOK_MODE".to_owned(), "0x07".to_owned()),
        ("CFLOG_FORCE_DISABLE_STDERR".to_owned(), "1".to_owned()),
        (
            "DYLD_INSERT_LIBRARIES".to_owned(),
            log_redirect_dylib.display().to_string(),
        ),
    ])
}

pub(crate) fn tcl_quote_arg(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('$', "\\$")
        .replace('[', "\\[")
        .replace(']', "\\]")
}

pub(crate) fn shell_quote_arg(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

pub(crate) fn lldb_quote_arg(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}
