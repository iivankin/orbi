use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::Mutex;

use anyhow::{Context, Result, anyhow, bail};
use serde::Deserialize;
use serde_json::{Value as JsonValue, json};
use sha2::{Digest, Sha256};

use super::super::{
    UiCrashDeleteRequest, UiCrashQuery, UiHardwareButton, UiKeyModifier, UiKeyPress,
    UiMenuSelection, UiPermissionConfig, UiPressKey, UiSelector, UiSwipeDirection, UiTravel,
};
use super::{MacosDoctorStatus, UiBackend};
use crate::apple::logs::MacosInferiorLogRelay;
use crate::apple::xcode::{SelectedXcode, xcrun_command};
use crate::context::ProjectContext;
use crate::util::{ensure_dir, run_command};

const HELPER_OVERRIDE_ENV: &str = "ORBI_INTERNAL_MACOS_UI_HELPER_PATH";
const HELPER_BINARY_NAME: &str = "orbi-macos-ui-helper";
const HELPER_SOURCES: &[&str] = &[
    "src/apple/testing/ui/macos_driver/Protocol.swift",
    "src/apple/testing/ui/macos_driver/AXSupport.swift",
    "src/apple/testing/ui/macos_driver/Input.swift",
    "src/apple/testing/ui/macos_driver/Screenshot.swift",
    "src/apple/testing/ui/macos_driver/Recording.swift",
    "src/apple/testing/ui/macos_driver/BackgroundActivation.swift",
    "src/apple/testing/ui/macos_driver/Automation.swift",
    "src/apple/testing/ui/macos_driver/Main.swift",
];

pub struct MacosBackend {
    helper: Mutex<MacosHelperProcess>,
    helper_path: PathBuf,
    bundle_id: String,
    bundle_path: PathBuf,
    log_pipe_path: PathBuf,
    log_relay: Mutex<Option<MacosUiLogRelay>>,
    _log_temp_dir: tempfile::TempDir,
    verbose: bool,
    active_video_path: Option<PathBuf>,
}

impl MacosBackend {
    pub fn prepare(
        project: &ProjectContext,
        receipt: &crate::apple::build::receipt::BuildReceipt,
    ) -> Result<Self> {
        let helper_path = ensure_macos_helper(project)?;
        let log_temp_dir = tempfile::TempDir::new()
            .context("failed to create temporary directory for macOS UI app logs")?;
        let log_pipe_path = log_temp_dir.path().join("inferior-stdio.pipe");
        Ok(Self {
            helper: Mutex::new(MacosHelperProcess::spawn(&helper_path)?),
            helper_path,
            bundle_id: receipt.bundle_id.clone(),
            bundle_path: receipt.bundle_path.clone(),
            log_pipe_path,
            log_relay: Mutex::new(None),
            _log_temp_dir: log_temp_dir,
            verbose: project.app.verbose,
            active_video_path: None,
        })
    }

    fn ensure_owned_bundle(&self, bundle_id: &str, action: &str) -> Result<()> {
        if bundle_id == self.bundle_id {
            return Ok(());
        }
        bail!(
            "{action} currently supports only Orbi's built app `{}` on macOS",
            self.bundle_id
        )
    }

    fn request(&self, command: &str, params: JsonValue) -> Result<JsonValue> {
        let mut helper = self
            .helper
            .lock()
            .map_err(|_| anyhow::anyhow!("failed to lock the macOS UI helper process"))?;
        if helper.has_exited() {
            *helper = MacosHelperProcess::spawn(&self.helper_path)?;
        }
        match helper.request(command, params) {
            Ok(value) => Ok(value),
            Err(error) => {
                let details = format!("{error:#}");
                if helper.has_exited()
                    || details.contains("Broken pipe")
                    || details.contains("exited before replying")
                {
                    let _ = MacosHelperProcess::spawn(&self.helper_path)
                        .map(|replacement| *helper = replacement);
                }
                Err(anyhow!(
                    "macOS UI helper `{}` failed while running `{command}`: {details}",
                    self.helper_path.display()
                ))
            }
        }
    }

    fn run(&self, command: &str, params: JsonValue) -> Result<()> {
        self.request(command, params).map(|_| ())
    }

    fn restart_log_relay(&self) -> Result<()> {
        let mut relay = self
            .log_relay
            .lock()
            .map_err(|_| anyhow!("failed to lock macOS UI log relay"))?;
        if relay.is_none() {
            *relay = Some(MacosUiLogRelay::start(
                &self.log_pipe_path,
                &self.bundle_id,
                self.verbose,
            )?);
        }
        Ok(())
    }

    fn stop_log_relay(&self) {
        if let Ok(mut relay) = self.log_relay.lock() {
            *relay = None;
        }
    }

    fn launch_with_open(
        &self,
        arguments: &[(String, String)],
        environment: &[(String, String)],
    ) -> Result<()> {
        self.restart_log_relay()?;

        let mut command = Command::new("open");
        command.args(["-g", "-n", "--stdout"]);
        command.arg(&self.log_pipe_path);
        command.arg("--stderr");
        command.arg(&self.log_pipe_path);
        for (key, value) in macos_ui_launch_environment(environment) {
            command.arg("--env");
            command.arg(format!("{key}={value}"));
        }
        command.arg(&self.bundle_path);
        if !arguments.is_empty() {
            command.arg("--args");
            for (key, value) in arguments {
                command.arg(format!("-{key}"));
                command.arg(value);
            }
        }

        run_command(&mut command)
            .with_context(|| format!("failed to launch `{}` with `open`", self.bundle_id))?;
        self.run("waitForApp", json!({ "bundleId": self.bundle_id }))
    }
}

impl UiBackend for MacosBackend {
    fn backend_name(&self) -> &'static str {
        "orbi-ax-macos"
    }

    fn target_name(&self) -> &str {
        "Mac"
    }

    fn target_id(&self) -> &str {
        "mac"
    }

    fn auto_record_top_level_flows(&self) -> bool {
        false
    }

    fn describe_all(&self) -> Result<JsonValue> {
        self.request("describeAll", json!({ "bundleId": self.bundle_id }))
    }

    fn describe_point(&self, x: f64, y: f64) -> Result<JsonValue> {
        self.request(
            "describePoint",
            json!({ "bundleId": self.bundle_id, "x": x, "y": y }),
        )
    }

    fn launch_app(
        &self,
        bundle_id: &str,
        stop_app: bool,
        arguments: &[(String, String)],
        environment: &[(String, String)],
    ) -> Result<()> {
        self.ensure_owned_bundle(bundle_id, "launchApp")?;
        if stop_app {
            self.stop_app(bundle_id)?;
        }
        self.launch_with_open(arguments, environment)
    }

    fn stop_app(&self, bundle_id: &str) -> Result<()> {
        let result = self.run("stopApp", json!({ "bundleId": bundle_id }));
        self.stop_log_relay();
        result
    }

    fn clear_app_state(&self, bundle_id: &str) -> Result<()> {
        self.ensure_owned_bundle(bundle_id, "clearState")?;
        self.run(
            "clearAppState",
            json!({ "bundleId": bundle_id, "bundlePath": self.bundle_path }),
        )
    }

    fn focus(&self) -> Result<()> {
        self.run("focus", json!({ "bundleId": self.bundle_id }))
    }

    fn tap_point(&self, x: f64, y: f64, duration_ms: Option<u32>) -> Result<()> {
        self.run(
            "tapPoint",
            json!({
                "bundleId": self.bundle_id,
                "x": x,
                "y": y,
                "durationMs": duration_ms,
            }),
        )
    }

    fn activate_selector(&self, selector: &UiSelector) -> Result<bool> {
        let result = self.request(
            "activateSelector",
            json!({ "bundleId": self.bundle_id, "selector": selector_json(selector) }),
        )?;
        result
            .as_bool()
            .context("macOS UI helper returned a non-bool `activateSelector` result")
    }

    fn hover_point(&self, x: f64, y: f64) -> Result<()> {
        self.run(
            "hoverPoint",
            json!({ "bundleId": self.bundle_id, "x": x, "y": y }),
        )
    }

    fn right_click_point(&self, x: f64, y: f64) -> Result<()> {
        self.run(
            "rightClickPoint",
            json!({ "bundleId": self.bundle_id, "x": x, "y": y }),
        )
    }

    fn swipe_points(
        &self,
        start: (f64, f64),
        end: (f64, f64),
        duration_ms: Option<u32>,
        delta: Option<u32>,
    ) -> Result<()> {
        self.run(
            "swipe",
            json!({
                "bundleId": self.bundle_id,
                "startX": start.0,
                "startY": start.1,
                "endX": end.0,
                "endY": end.1,
                "durationMs": duration_ms,
                "delta": delta,
            }),
        )
    }

    fn drag_points(
        &self,
        start: (f64, f64),
        end: (f64, f64),
        duration_ms: Option<u32>,
        delta: Option<u32>,
        payload_hint: Option<&str>,
    ) -> Result<()> {
        self.run(
            "drag",
            json!({
                "bundleId": self.bundle_id,
                "startX": start.0,
                "startY": start.1,
                "endX": end.0,
                "endY": end.1,
                "durationMs": duration_ms,
                "delta": delta,
                "payloadHint": payload_hint,
            }),
        )
    }

    fn input_text(&self, text: &str) -> Result<()> {
        self.run(
            "inputText",
            json!({ "bundleId": self.bundle_id, "text": text }),
        )
    }

    fn press_button(&self, button: UiHardwareButton, _duration_ms: Option<u32>) -> Result<()> {
        bail!("hardware button `{button:?}` is not supported by the macOS UI backend")
    }

    fn press_key(&self, key: &UiKeyPress) -> Result<()> {
        self.run(
            "pressKey",
            json!({
                "bundleId": self.bundle_id,
                "key": key_json(key.key),
                "modifiers": modifiers_json(&key.modifiers),
            }),
        )
    }

    fn select_menu_item(&self, selection: &UiMenuSelection) -> Result<()> {
        self.run(
            "selectMenuItem",
            json!({
                "bundleId": self.bundle_id,
                "source": selection.source.as_ref().map(selector_json),
                "path": selection.path,
            }),
        )
    }

    fn press_key_code(
        &self,
        keycode: u32,
        duration_ms: Option<u32>,
        modifiers: &[UiKeyModifier],
    ) -> Result<()> {
        self.run(
            "pressKeyCode",
            json!({
                "bundleId": self.bundle_id,
                "keyCode": keycode,
                "durationMs": duration_ms,
                "modifiers": modifiers_json(modifiers),
            }),
        )
    }

    fn press_key_sequence(&self, keycodes: &[u32]) -> Result<()> {
        self.run(
            "pressKeySequence",
            json!({ "bundleId": self.bundle_id, "keyCodes": keycodes }),
        )
    }

    fn take_screenshot(&self, path: &Path) -> Result<()> {
        self.run(
            "takeScreenshot",
            json!({ "bundleId": self.bundle_id, "path": path }),
        )
    }

    fn open_link(&self, _url: &str) -> Result<()> {
        bail!("`openLink` is not supported by the background macOS UI backend")
    }

    fn clear_keychain(&self) -> Result<()> {
        bail!("`clearKeychain` is not supported by the macOS UI backend")
    }

    fn set_location(&self, _latitude: f64, _longitude: f64) -> Result<()> {
        bail!("`setLocation` is not supported by the macOS UI backend")
    }

    fn set_permissions(&self, _bundle_id: &str, _config: &UiPermissionConfig) -> Result<()> {
        bail!("`setPermissions` is not supported by the macOS UI backend")
    }

    fn travel(&self, _command: &UiTravel) -> Result<()> {
        bail!("`travel` is not supported by the macOS UI backend")
    }

    fn add_media(&self, _paths: &[PathBuf]) -> Result<()> {
        bail!("`addMedia` is not supported by the macOS UI backend")
    }

    fn install_dylib(&self, _path: &Path) -> Result<()> {
        bail!("`installDylib` is not supported by the macOS UI backend")
    }

    fn run_instruments(&self, _template: &str, _arguments: &[String]) -> Result<()> {
        bail!("`runInstruments` is not supported by the macOS UI backend")
    }

    fn update_contacts(&self, _path: &Path) -> Result<()> {
        bail!("`updateContacts` is not supported by the macOS UI backend")
    }

    fn list_crash_logs(&self, _query: &UiCrashQuery) -> Result<()> {
        bail!("`crash list` is not supported by the macOS UI backend")
    }

    fn show_crash_log(&self, _name: &str) -> Result<()> {
        bail!("`crash show` is not supported by the macOS UI backend")
    }

    fn delete_crash_logs(&self, _request: &UiCrashDeleteRequest) -> Result<()> {
        bail!("`crash delete` is not supported by the macOS UI backend")
    }

    fn stream_logs(&self, _arguments: &[String]) -> Result<()> {
        bail!("`logs` is not supported by the macOS UI backend")
    }

    fn scroll_in_direction(&self, direction: UiSwipeDirection) -> Result<()> {
        self.run(
            "scroll",
            json!({ "bundleId": self.bundle_id, "direction": direction_json(direction) }),
        )
    }

    fn scroll_at_point(&self, direction: UiSwipeDirection, point: (f64, f64)) -> Result<()> {
        self.run(
            "scroll",
            json!({
                "bundleId": self.bundle_id,
                "direction": direction_json(direction),
                "x": point.0,
                "y": point.1,
            }),
        )
    }

    fn hide_keyboard(&self) -> Result<()> {
        Ok(())
    }

    fn start_video_recording(&mut self, path: &Path) -> Result<()> {
        if self.active_video_path.is_some() {
            bail!(
                "video recording is already active for {}",
                self.target_name()
            );
        }
        if let Some(parent) = path.parent() {
            ensure_dir(parent)?;
        }
        self.run(
            "startVideoRecording",
            json!({ "bundleId": self.bundle_id, "path": path }),
        )?;
        self.active_video_path = Some(path.to_path_buf());
        Ok(())
    }

    fn stop_video_recording(&mut self) -> Result<()> {
        let Some(path) = self.active_video_path.take() else {
            return Ok(());
        };
        self.run("stopVideoRecording", json!({}))?;
        if !path.exists() {
            bail!(
                "macOS video recording finished without writing {}",
                path.display()
            );
        }
        Ok(())
    }
}

impl Drop for MacosBackend {
    fn drop(&mut self) {
        let params = json!({ "bundleId": self.bundle_id, "force": true });
        let Ok(mut helper) = self.helper.lock() else {
            return;
        };
        if helper.has_exited() {
            if let Ok(mut replacement) = MacosHelperProcess::spawn(&self.helper_path) {
                let _ = replacement.request("stopApp", params);
            }
            return;
        }
        let _ = helper.request("stopVideoRecording", json!({}));
        let _ = helper.request("stopApp", params);
        self.stop_log_relay();
    }
}

struct MacosUiLogRelay {
    anchor: Option<fs::File>,
    relay: Option<MacosInferiorLogRelay>,
}

impl MacosUiLogRelay {
    fn start(pipe_path: &Path, bundle_id: &str, verbose: bool) -> Result<Self> {
        match fs::remove_file(pipe_path) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to remove {}", pipe_path.display()));
            }
        }
        let mut mkfifo = Command::new("mkfifo");
        mkfifo.arg(pipe_path);
        run_command(&mut mkfifo)?;
        let anchor = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(pipe_path)
            .with_context(|| format!("failed to open macOS UI log pipe {}", pipe_path.display()))?;
        let relay = MacosInferiorLogRelay::start(pipe_path, bundle_id, verbose);
        Ok(Self {
            anchor: Some(anchor),
            relay: Some(relay),
        })
    }
}

impl Drop for MacosUiLogRelay {
    fn drop(&mut self) {
        self.anchor.take();
        if let Some(mut relay) = self.relay.take() {
            relay.stop();
        }
    }
}

fn macos_ui_launch_environment(environment: &[(String, String)]) -> BTreeMap<String, String> {
    let mut merged = BTreeMap::from([
        ("IDEPreferLogStreaming".to_owned(), "YES".to_owned()),
        ("OS_ACTIVITY_DT_MODE".to_owned(), "1".to_owned()),
    ]);
    for (key, value) in environment {
        merged.insert(key.clone(), value.clone());
    }
    merged
}

struct MacosHelperProcess {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl MacosHelperProcess {
    fn spawn(path: &Path) -> Result<Self> {
        let mut child = Command::new(path)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("failed to start macOS UI helper {}", path.display()))?;
        let stdin = child
            .stdin
            .take()
            .context("macOS UI helper did not expose stdin")?;
        let stdout = child
            .stdout
            .take()
            .context("macOS UI helper did not expose stdout")?;
        Ok(Self {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        })
    }

    fn request(&mut self, command: &str, params: JsonValue) -> Result<JsonValue> {
        let id = self.next_id;
        self.next_id += 1;
        let request = json!({
            "id": id,
            "command": command,
            "params": params,
        });
        serde_json::to_writer(&mut self.stdin, &request)
            .context("failed to encode macOS UI helper request")?;
        self.stdin
            .write_all(b"\n")
            .context("failed to write macOS UI helper request newline")?;
        self.stdin
            .flush()
            .context("failed to flush macOS UI helper request")?;

        let mut line = String::new();
        let bytes = self
            .stdout
            .read_line(&mut line)
            .context("failed to read macOS UI helper response")?;
        if bytes == 0 {
            bail!("macOS UI helper exited before replying to `{command}`");
        }
        let response: HelperResponse = serde_json::from_str(&line).with_context(|| {
            format!("failed to parse macOS UI helper response `{}`", line.trim())
        })?;
        if response.id != id {
            bail!(
                "macOS UI helper response id mismatch: expected {id}, got {}",
                response.id
            );
        }
        if !response.ok {
            bail!(
                "{}",
                response
                    .error
                    .unwrap_or_else(|| format!("macOS UI helper command `{command}` failed"))
            );
        }
        Ok(response.result.unwrap_or(JsonValue::Null))
    }

    fn has_exited(&mut self) -> bool {
        match self.child.try_wait() {
            Ok(Some(_)) => true,
            Ok(None) => false,
            Err(_) => true,
        }
    }
}

impl Drop for MacosHelperProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[derive(Debug, Deserialize)]
struct HelperResponse {
    id: u64,
    ok: bool,
    result: Option<JsonValue>,
    error: Option<String>,
}

pub(crate) fn macos_doctor(project: &ProjectContext) -> Result<MacosDoctorStatus> {
    let helper_path = ensure_macos_helper(project)?;
    let mut helper = MacosHelperProcess::spawn(&helper_path)?;
    let status = helper.request("checkPermissions", json!({}))?;
    serde_json::from_value(status).context("failed to parse macOS UI helper doctor status")
}

fn ensure_macos_helper(project: &ProjectContext) -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(HELPER_OVERRIDE_ENV) {
        let path = PathBuf::from(path);
        if !path.exists() {
            bail!(
                "{HELPER_OVERRIDE_ENV} points to {}, but that file does not exist",
                path.display()
            );
        }
        return Ok(path);
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let sources = HELPER_SOURCES
        .iter()
        .map(|source| manifest_dir.join(source))
        .collect::<Vec<_>>();
    let helper_hash = helper_source_hash(&sources)?;
    let output_dir = project
        .app
        .global_paths
        .cache_dir
        .join("macos-ui-helper")
        .join(helper_hash);
    let helper_path = output_dir.join(HELPER_BINARY_NAME);
    if helper_path.exists() {
        return Ok(helper_path);
    }

    ensure_dir(&output_dir)?;
    compile_macos_helper(
        project.selected_xcode.as_ref(),
        sources.as_slice(),
        &helper_path,
    )?;
    Ok(helper_path)
}

fn helper_source_hash(sources: &[PathBuf]) -> Result<String> {
    let mut hasher = Sha256::new();
    for source in sources {
        hasher.update(source.to_string_lossy().as_bytes());
        hasher.update([0]);
        let contents = fs::read(source).with_context(|| {
            format!("failed to read macOS UI helper source {}", source.display())
        })?;
        hasher.update(contents);
        hasher.update([0]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn compile_macos_helper(
    selected_xcode: Option<&SelectedXcode>,
    sources: &[PathBuf],
    output_path: &Path,
) -> Result<()> {
    let mut command = xcrun_command(selected_xcode);
    command
        .args(["--sdk", "macosx", "swiftc", "-o"])
        .arg(output_path)
        .args(sources)
        .args([
            "-framework",
            "AppKit",
            "-framework",
            "ApplicationServices",
            "-framework",
            "ScreenCaptureKit",
            "-framework",
            "AVFoundation",
            "-framework",
            "CoreGraphics",
        ]);
    run_command(&mut command).with_context(|| {
        format!(
            "failed to compile macOS UI helper to {}",
            output_path.display()
        )
    })
}

fn selector_json(selector: &UiSelector) -> JsonValue {
    json!({
        "text": selector.text,
        "id": selector.id,
    })
}

fn key_json(key: UiPressKey) -> JsonValue {
    match key {
        UiPressKey::Character(character) => {
            json!({ "kind": "Character", "value": character.to_string() })
        }
        UiPressKey::Home => json!({ "kind": "Home" }),
        UiPressKey::Enter => json!({ "kind": "Enter" }),
        UiPressKey::Backspace => json!({ "kind": "Backspace" }),
        UiPressKey::Escape => json!({ "kind": "Escape" }),
        UiPressKey::Space => json!({ "kind": "Space" }),
        UiPressKey::Tab => json!({ "kind": "Tab" }),
        UiPressKey::LeftArrow => json!({ "kind": "LeftArrow" }),
        UiPressKey::RightArrow => json!({ "kind": "RightArrow" }),
        UiPressKey::UpArrow => json!({ "kind": "UpArrow" }),
        UiPressKey::DownArrow => json!({ "kind": "DownArrow" }),
        UiPressKey::Lock
        | UiPressKey::VolumeUp
        | UiPressKey::VolumeDown
        | UiPressKey::Back
        | UiPressKey::Power => json!({ "kind": key.summary() }),
    }
}

fn modifiers_json(modifiers: &[UiKeyModifier]) -> Vec<&'static str> {
    modifiers
        .iter()
        .map(|modifier| match modifier {
            UiKeyModifier::Command => "Command",
            UiKeyModifier::Shift => "Shift",
            UiKeyModifier::Option => "Option",
            UiKeyModifier::Control => "Control",
            UiKeyModifier::Function => "Function",
        })
        .collect()
}

fn direction_json(direction: UiSwipeDirection) -> &'static str {
    match direction {
        UiSwipeDirection::Left => "left",
        UiSwipeDirection::Right => "right",
        UiSwipeDirection::Up => "up",
        UiSwipeDirection::Down => "down",
    }
}
