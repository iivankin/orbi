use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use roxmltree::{Document, Node};
use signal_hook::consts::signal::SIGINT;
use signal_hook::iterator::{Handle as SignalHandle, Signals};

use crate::apple::xcode::{SelectedXcode, xcrun_command};
use crate::cli::{InspectTraceArgs, ProfileKind};
use crate::context::AppContext;
use crate::util::{
    command_output_allow_failure, debug_command, ensure_dir, ensure_parent_dir, resolve_path,
    timestamp_slug,
};

pub(crate) const SIMULATOR_PROFILING_UNAVAILABLE_MESSAGE: &str = "simulator profiling is currently unavailable because Apple's xctrace/InstrumentsCLI simulator path is unstable and can hang or emit broken traces. Use a physical device or macOS target instead.";
const XPATH_TIME_PROFILE: &str =
    r#"/trace-toc/run[@number="1"]/data/table[@schema="time-profile"]"#;
const XPATH_ALLOCATIONS_STATISTICS: &str =
    r#"/trace-toc/run/tracks/track[@name="Allocations"]/details/detail[@name="Statistics"]"#;
const XPATH_ALLOCATIONS_LIST: &str =
    r#"/trace-toc/run/tracks/track[@name="Allocations"]/details/detail[@name="Allocations List"]"#;
const DIAGNOSIS_THRESHOLD_PERCENT: f64 = 1.0;
const DIAGNOSIS_STACK_DEPTH: usize = 5;
const DIAGNOSIS_MAX_ITEMS: usize = 10;

pub(crate) struct TraceRecording {
    output_path: PathBuf,
    selected_xcode: Option<SelectedXcode>,
    child: Child,
    debug: String,
    backend: TraceRecordingBackend,
}

#[derive(Debug, Clone, Copy)]
enum TraceRecordingBackend {
    Xctrace,
    #[cfg(test)]
    PlainFile,
}

#[derive(Debug, Clone, Copy)]
enum TraceLaunchStdio {
    Inherit,
    Null,
}

struct SignalForwarder {
    handle: SignalHandle,
    thread: Option<JoinHandle<()>>,
}

#[derive(Debug, Clone)]
struct TraceMetadata {
    process_name: String,
    process_path: Option<String>,
    duration_s: f64,
    template_name: String,
    device_platform: Option<String>,
    device_name: Option<String>,
    has_time_profile_table: bool,
    has_allocations_track: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct FrameKey {
    binary_name: String,
    name: String,
}

#[derive(Debug, Clone)]
struct TraceFrame {
    key: FrameKey,
    is_user: bool,
    is_symbolicated: bool,
}

#[derive(Debug, Clone)]
struct TraceSample {
    weight_ns: u64,
    frames: Vec<TraceFrame>,
}

#[derive(Debug, Default)]
struct TimeProfileSummary {
    total_weight_ns: u64,
    sample_count: usize,
    unsymbolicated_user_weight_ns: u64,
    self_time: HashMap<FrameKey, u64>,
    total_time: HashMap<FrameKey, u64>,
    stack_time: HashMap<Vec<FrameKey>, u64>,
}

#[derive(Debug, Clone)]
struct AllocationStatRow {
    category: String,
    persistent_bytes: u64,
    transient_bytes: u64,
    total_bytes: u64,
    count_persistent: u64,
    count_transient: u64,
    count_events: u64,
}

#[derive(Debug, Clone)]
struct AllocationListRow {
    size_bytes: u64,
    live: bool,
    responsible_caller: Option<String>,
    responsible_library: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AllocationCallerKey {
    library: String,
    caller: String,
}

pub(crate) fn start_optional_launched_process_trace(
    root: &Path,
    selected_xcode: Option<&SelectedXcode>,
    interactive: bool,
    kind: Option<ProfileKind>,
    launch_target: &str,
    device: Option<&str>,
) -> Result<Option<(ProfileKind, TraceRecording)>> {
    kind.map(|kind| {
        start_launched_process_trace(
            root,
            selected_xcode,
            interactive,
            kind,
            launch_target,
            device,
        )
        .map(|recording| (kind, recording))
    })
    .transpose()
}

pub(crate) fn start_optional_launched_command_trace(
    root: &Path,
    selected_xcode: Option<&SelectedXcode>,
    interactive: bool,
    kind: Option<ProfileKind>,
    launch_command: &[String],
    device: Option<&str>,
) -> Result<Option<(ProfileKind, TraceRecording)>> {
    kind.map(|kind| {
        start_launched_trace(
            root,
            selected_xcode,
            interactive,
            kind,
            launch_command,
            device,
            TraceLaunchStdio::Inherit,
        )
        .map(|recording| (kind, recording))
    })
    .transpose()
}

pub(crate) fn trace_recording_process_id(recording: &TraceRecording) -> u32 {
    recording.child.id()
}

pub(crate) fn ensure_simulator_profiling_supported(kind: Option<ProfileKind>) -> Result<()> {
    if kind.is_some() {
        bail!("{SIMULATOR_PROFILING_UNAVAILABLE_MESSAGE}");
    }
    Ok(())
}

fn start_launched_process_trace(
    root: &Path,
    selected_xcode: Option<&SelectedXcode>,
    interactive: bool,
    kind: ProfileKind,
    launch_target: &str,
    device: Option<&str>,
) -> Result<TraceRecording> {
    start_launched_trace(
        root,
        selected_xcode,
        interactive,
        kind,
        &[launch_target.to_owned()],
        device,
        TraceLaunchStdio::Null,
    )
}

fn start_launched_trace(
    root: &Path,
    selected_xcode: Option<&SelectedXcode>,
    interactive: bool,
    kind: ProfileKind,
    launch_command: &[String],
    device: Option<&str>,
    stdio: TraceLaunchStdio,
) -> Result<TraceRecording> {
    if launch_command.is_empty() {
        bail!("xctrace launched trace requires at least one launch argument");
    }

    let output_path = default_trace_output(root, kind)?;
    let mut command = xcrun_command(selected_xcode);
    command.arg("xctrace");
    command.arg("record");
    command.arg("--template");
    command.arg(profile_kind_template(kind));
    command.arg("--output");
    command.arg(&output_path);
    if let Some(device) = device {
        command.arg("--device").arg(device);
    }
    command.arg("--env");
    command.arg("OS_ACTIVITY_DT_MODE=1");
    command.arg("--env");
    command.arg("IDEPreferLogStreaming=YES");
    command.arg("--launch");
    command.arg("--");
    command.args(launch_command);
    if !interactive {
        command.arg("--no-prompt");
    }
    if matches!(stdio, TraceLaunchStdio::Null) {
        command.stdout(Stdio::null());
        command.stderr(Stdio::null());
    }

    let debug = debug_command(&command);
    let child = command
        .spawn()
        .with_context(|| format!("failed to execute `{debug}`"))?;
    Ok(TraceRecording {
        output_path,
        selected_xcode: selected_xcode.cloned(),
        child,
        debug,
        backend: TraceRecordingBackend::Xctrace,
    })
}

pub(crate) fn wait_for_launched_trace_exit(
    kind: ProfileKind,
    recording: TraceRecording,
) -> Result<()> {
    // `orbit run --trace` tells the user to press Ctrl-C to stop the recording. We
    // intercept that signal here, forward it to `xctrace`, and stay alive long
    // enough for the trace bundle to become exportable.
    let (interrupt_tx, interrupt_rx) = mpsc::channel();
    let signal_forwarder = SignalForwarder::install(interrupt_tx)?;
    let path = wait_for_trace_recording_exit(kind, recording, Some(&interrupt_rx))?;
    drop(signal_forwarder);
    println!("trace: {}", path.display());
    Ok(())
}

pub(crate) fn finish_started_trace(
    kind: ProfileKind,
    recording: TraceRecording,
) -> Result<PathBuf> {
    finish_trace_recording(recording)
        .with_context(|| format!("failed to finalize {} trace", profile_kind_label(kind)))
}

fn wait_for_trace_recording_exit(
    kind: ProfileKind,
    mut recording: TraceRecording,
    interrupt_rx: Option<&Receiver<()>>,
) -> Result<PathBuf> {
    let mut interrupted = false;

    loop {
        if let Some(status) = recording.child.try_wait()? {
            if status.success() {
                return finish_trace_recording(recording).with_context(|| {
                    format!("failed to finalize {} trace", profile_kind_label(kind))
                });
            }

            if interrupted {
                verify_recording_output(&recording).with_context(|| {
                    format!(
                        "failed to finalize {} trace after interruption",
                        profile_kind_label(kind)
                    )
                })?;
                return Ok(recording.output_path);
            }

            bail!("`{}` failed with {status}", recording.debug);
        }

        if received_interrupt(interrupt_rx)? {
            interrupted = true;
            send_interrupt_to_child(&recording.child)?;
        }

        thread::sleep(Duration::from_millis(100));
    }
}

fn finish_trace_recording(recording: TraceRecording) -> Result<PathBuf> {
    match recording.backend {
        TraceRecordingBackend::Xctrace => finish_xctrace_recording(recording),
        #[cfg(test)]
        TraceRecordingBackend::PlainFile => finish_plain_recording(recording),
    }
}

fn verify_recording_output(recording: &TraceRecording) -> Result<()> {
    match recording.backend {
        TraceRecordingBackend::Xctrace => {
            wait_for_recording_output_path(recording, Duration::from_secs(5))?;
            wait_for_exportable_trace(
                &recording.output_path,
                recording.selected_xcode.as_ref(),
                &recording.debug,
            )
        }
        #[cfg(test)]
        TraceRecordingBackend::PlainFile => {
            wait_for_recording_output_path(recording, Duration::from_secs(2))
        }
    }
}

fn wait_for_recording_output_path(recording: &TraceRecording, timeout: Duration) -> Result<()> {
    let started = Instant::now();
    while started.elapsed() < timeout {
        if recording.output_path.exists() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(50));
    }

    bail!(
        "`{}` exited before writing {}",
        recording.debug,
        recording.output_path.display()
    )
}

fn finish_xctrace_recording(mut recording: TraceRecording) -> Result<PathBuf> {
    let graceful_wait_started = Instant::now();
    while graceful_wait_started.elapsed() < Duration::from_millis(250) {
        if let Some(status) = recording.child.try_wait()? {
            if status.success() && recording.output_path.exists() {
                wait_for_exportable_trace(
                    &recording.output_path,
                    recording.selected_xcode.as_ref(),
                    &recording.debug,
                )?;
                return Ok(recording.output_path);
            }
            bail!(
                "`{}` exited with {status} before writing {}",
                recording.debug,
                recording.output_path.display()
            );
        }
        thread::sleep(Duration::from_millis(25));
    }

    if recording.child.try_wait()?.is_none() {
        let _ = send_interrupt_to_child(&recording.child);
    }

    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(30) {
        if let Some(status) = recording.child.try_wait()? {
            if status.success() && recording.output_path.exists() {
                wait_for_exportable_trace(
                    &recording.output_path,
                    recording.selected_xcode.as_ref(),
                    &recording.debug,
                )?;
                return Ok(recording.output_path);
            }
            bail!(
                "`{}` exited with {status} before writing {}",
                recording.debug,
                recording.output_path.display()
            );
        }
        thread::sleep(Duration::from_millis(100));
    }

    let _ = recording.child.kill();
    let _ = recording.child.wait();
    if recording.output_path.exists()
        && wait_for_exportable_trace(
            &recording.output_path,
            recording.selected_xcode.as_ref(),
            &recording.debug,
        )
        .is_ok()
    {
        return Ok(recording.output_path);
    }

    bail!(
        "timed out waiting for `{}` to finish writing an exportable trace at {}",
        recording.debug,
        recording.output_path.display()
    )
}

#[cfg(test)]
fn finish_plain_recording(mut recording: TraceRecording) -> Result<PathBuf> {
    let graceful_wait_started = Instant::now();
    while graceful_wait_started.elapsed() < Duration::from_millis(250) {
        if let Some(status) = recording.child.try_wait()? {
            if status.success() && recording.output_path.exists() {
                return Ok(recording.output_path);
            }
            bail!(
                "`{}` exited with {status} before writing {}",
                recording.debug,
                recording.output_path.display()
            );
        }
        thread::sleep(Duration::from_millis(25));
    }

    if recording.child.try_wait()?.is_none() {
        let mut interrupt = std::process::Command::new("kill");
        interrupt.args(["-INT", &recording.child.id().to_string()]);
        let _ = command_output_allow_failure(&mut interrupt)?;
    }

    let started = Instant::now();
    while started.elapsed() < Duration::from_secs(10) {
        if let Some(status) = recording.child.try_wait()? {
            if status.success() && recording.output_path.exists() {
                return Ok(recording.output_path);
            }
            bail!(
                "`{}` exited with {status} before writing {}",
                recording.debug,
                recording.output_path.display()
            );
        }
        thread::sleep(Duration::from_millis(100));
    }

    let _ = recording.child.kill();
    let _ = recording.child.wait();
    if recording.output_path.exists() {
        return Ok(recording.output_path);
    }

    bail!(
        "timed out waiting for `{}` to finish writing {}",
        recording.debug,
        recording.output_path.display()
    )
}

fn wait_for_exportable_trace(
    output_path: &Path,
    selected_xcode: Option<&SelectedXcode>,
    debug: &str,
) -> Result<()> {
    let started = Instant::now();
    let mut last_error = None;

    while started.elapsed() < Duration::from_secs(10) {
        let mut command = xctrace_export_command_with_xcode(output_path, selected_xcode);
        command.arg("--toc");
        let (success, _stdout, stderr) = command_output_allow_failure(&mut command)?;
        if success {
            return Ok(());
        }

        let stderr = stderr.trim();
        if !stderr.is_empty() {
            last_error = Some(stderr.to_owned());
        }
        thread::sleep(Duration::from_millis(100));
    }

    if let Some(error) = last_error {
        bail!(
            "timed out waiting for `{debug}` to finalize an exportable trace at {}; last export error: {error}",
            output_path.display()
        );
    }

    bail!(
        "timed out waiting for `{debug}` to finalize an exportable trace at {}",
        output_path.display()
    )
}

fn received_interrupt(interrupt_rx: Option<&Receiver<()>>) -> Result<bool> {
    let Some(interrupt_rx) = interrupt_rx else {
        return Ok(false);
    };

    let mut received = false;
    loop {
        match interrupt_rx.try_recv() {
            Ok(()) => received = true,
            Err(TryRecvError::Empty) => return Ok(received),
            Err(TryRecvError::Disconnected) => return Ok(received),
        }
    }
}

fn send_interrupt_to_child(child: &Child) -> Result<()> {
    let mut interrupt = std::process::Command::new("kill");
    interrupt.args(["-INT", &child.id().to_string()]);
    let _ = command_output_allow_failure(&mut interrupt)?;
    Ok(())
}

impl SignalForwarder {
    fn install(interrupt_tx: mpsc::Sender<()>) -> Result<Self> {
        let mut signals = Signals::new([SIGINT])
            .context("failed to install Ctrl-C handler for trace recording")?;
        let handle = signals.handle();
        let thread = thread::spawn(move || {
            for _signal in &mut signals {
                let _ = interrupt_tx.send(());
            }
        });
        Ok(Self {
            handle,
            thread: Some(thread),
        })
    }
}

impl Drop for SignalForwarder {
    fn drop(&mut self) {
        self.handle.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub fn inspect_trace_command(app: &AppContext, args: &InspectTraceArgs) -> Result<()> {
    let trace_path = resolve_path(&app.cwd, &args.trace);
    let toc_debug = debug_export_command(&trace_path, None, TraceExportMode::Toc);
    let toc_xml = capture_xctrace_export(&trace_path, None, TraceExportMode::Toc, &toc_debug)?;
    let metadata = parse_trace_metadata(&toc_xml)?;

    if metadata.has_time_profile_table {
        let profile_debug = debug_export_command(
            &trace_path,
            None,
            TraceExportMode::XPath(XPATH_TIME_PROFILE),
        );
        let profile_xml = capture_xctrace_export(
            &trace_path,
            None,
            TraceExportMode::XPath(XPATH_TIME_PROFILE),
            &profile_debug,
        )?;
        let samples = parse_time_profile_samples(&profile_xml, &metadata)?;
        print!("{}", render_time_profile_diagnosis(&metadata, &samples));
        return Ok(());
    }

    if metadata.has_allocations_track {
        let statistics_debug = debug_export_command(
            &trace_path,
            None,
            TraceExportMode::XPath(XPATH_ALLOCATIONS_STATISTICS),
        );
        let statistics_xml = capture_xctrace_export(
            &trace_path,
            None,
            TraceExportMode::XPath(XPATH_ALLOCATIONS_STATISTICS),
            &statistics_debug,
        )?;
        let allocations_debug = debug_export_command(
            &trace_path,
            None,
            TraceExportMode::XPath(XPATH_ALLOCATIONS_LIST),
        );
        let allocations_xml = capture_xctrace_export(
            &trace_path,
            None,
            TraceExportMode::XPath(XPATH_ALLOCATIONS_LIST),
            &allocations_debug,
        )?;
        let stats = parse_allocations_statistics(&statistics_xml)?;
        let rows = parse_allocations_list(&allocations_xml)?;
        print!("{}", render_allocations_diagnosis(&metadata, &stats, &rows));
        return Ok(());
    }

    let template = if metadata.template_name.is_empty() {
        "<unknown>"
    } else {
        &metadata.template_name
    };
    bail!(
        "inspect-trace currently supports Time Profiler and Allocations traces only; trace template: {template}"
    );
}

fn xctrace_export_command_with_xcode(
    trace_path: &Path,
    selected_xcode: Option<&SelectedXcode>,
) -> std::process::Command {
    let mut command = xcrun_command(selected_xcode);
    command.arg("xctrace");
    command.arg("export");
    command.arg("--input");
    command.arg(trace_path);
    command
}

#[derive(Clone, Copy)]
enum TraceExportMode<'a> {
    Toc,
    XPath(&'a str),
}

fn debug_export_command(
    trace_path: &Path,
    selected_xcode: Option<&SelectedXcode>,
    mode: TraceExportMode<'_>,
) -> String {
    let mut command = xctrace_export_command_with_xcode(trace_path, selected_xcode);
    apply_trace_export_mode(&mut command, mode);
    debug_command(&command)
}

fn apply_trace_export_mode(command: &mut std::process::Command, mode: TraceExportMode<'_>) {
    match mode {
        TraceExportMode::Toc => {
            command.arg("--toc");
        }
        TraceExportMode::XPath(xpath) => {
            command.arg("--xpath");
            command.arg(xpath);
        }
    }
}

fn capture_xctrace_export(
    trace_path: &Path,
    selected_xcode: Option<&SelectedXcode>,
    mode: TraceExportMode<'_>,
    debug: &str,
) -> Result<String> {
    let started = Instant::now();
    let mut last_error = None;

    while started.elapsed() < Duration::from_secs(10) {
        let mut command = xctrace_export_command_with_xcode(trace_path, selected_xcode);
        apply_trace_export_mode(&mut command, mode);
        let (success, stdout, stderr) = command_output_allow_failure(&mut command)?;
        if success {
            return Ok(stdout);
        }

        let stderr = stderr.trim();
        if !stderr.is_empty() {
            last_error = Some(stderr.to_owned());
        }
        thread::sleep(Duration::from_millis(100));
    }

    if let Some(error) = last_error {
        bail!(
            "timed out waiting for `{debug}` to succeed for {}; last export error: {error}",
            trace_path.display()
        );
    }

    bail!(
        "timed out waiting for `{debug}` to succeed for {}",
        trace_path.display()
    )
}

fn parse_trace_metadata(toc_xml: &str) -> Result<TraceMetadata> {
    let document = Document::parse(toc_xml).context("failed to parse xctrace TOC XML")?;
    let root = document.root_element();
    if !root.has_tag_name("trace-toc") {
        bail!(
            "unexpected xctrace TOC XML root: {}",
            root.tag_name().name()
        );
    }

    let run = root
        .children()
        .find(|node| node.has_tag_name("run"))
        .context("trace TOC did not contain a run")?;
    let info = child_element(run, "info").context("trace TOC run did not contain info")?;
    let target = child_element(info, "target").context("trace TOC run did not contain target")?;
    let summary =
        child_element(info, "summary").context("trace TOC run did not contain summary")?;

    let process = child_element(target, "process");
    let process_name = process
        .and_then(|node| node.attribute("name"))
        .unwrap_or_default()
        .to_owned();
    let duration_s = child_text(summary, "duration")
        .and_then(|text| text.parse::<f64>().ok())
        .unwrap_or_default();
    let template_name = child_text(summary, "template-name")
        .unwrap_or_default()
        .to_owned();
    let device = child_element(target, "device");
    let device_platform = device
        .and_then(|node| node.attribute("platform"))
        .map(str::to_owned);
    let device_name = device
        .and_then(|node| node.attribute("name"))
        .map(str::to_owned);

    let processes = child_element(run, "processes");
    let process_path = processes.and_then(|processes_node| {
        processes_node
            .children()
            .find(|node| {
                node.has_tag_name("process")
                    && node.attribute("name") == Some(process_name.as_str())
                    && node.attribute("path").is_some()
            })
            .and_then(|node| node.attribute("path"))
            .map(str::to_owned)
            .or_else(|| {
                processes_node
                    .children()
                    .find(|node| {
                        node.has_tag_name("process")
                            && node.attribute("name") != Some("kernel")
                            && node.attribute("path").is_some()
                    })
                    .and_then(|node| node.attribute("path"))
                    .map(str::to_owned)
            })
    });

    let has_time_profile_table = run
        .descendants()
        .any(|node| node.has_tag_name("table") && node.attribute("schema") == Some("time-profile"));
    let has_allocations_track = run
        .descendants()
        .any(|node| node.has_tag_name("track") && node.attribute("name") == Some("Allocations"));

    Ok(TraceMetadata {
        process_name,
        process_path,
        duration_s,
        template_name,
        device_platform,
        device_name,
        has_time_profile_table,
        has_allocations_track,
    })
}

fn parse_time_profile_samples(xml: &str, metadata: &TraceMetadata) -> Result<Vec<TraceSample>> {
    let document = Document::parse(xml).context("failed to parse xctrace time-profile XML")?;
    let mut registry = HashMap::new();
    for node in document.descendants().filter(|node| node.is_element()) {
        if let Some(id) = node.attribute("id") {
            registry.insert(id.to_owned(), node);
        }
    }

    let mut samples = Vec::new();
    for row in document
        .descendants()
        .filter(|node| node.has_tag_name("row"))
    {
        let Some(backtrace) = resolve_row_backtrace(row, &registry) else {
            continue;
        };
        let weight_ns = resolve_row_weight(row, &registry);
        let frames = extract_frames(backtrace, &registry, metadata);
        samples.push(TraceSample { weight_ns, frames });
    }
    Ok(samples)
}

fn render_time_profile_diagnosis(metadata: &TraceMetadata, samples: &[TraceSample]) -> String {
    let summary = summarize_time_profile(samples);
    let mut output = String::new();

    let process_name = if metadata.process_name.is_empty() {
        "<unknown>"
    } else {
        metadata.process_name.as_str()
    };
    let template_name = if metadata.template_name.is_empty() {
        "<unknown>"
    } else {
        metadata.template_name.as_str()
    };
    let _ = writeln!(
        output,
        "Process: {process_name}  Duration: {:.1}s  Template: {template_name}",
        metadata.duration_s
    );
    if let (Some(platform), Some(device_name)) = (
        metadata.device_platform.as_deref(),
        metadata.device_name.as_deref(),
    ) {
        let _ = writeln!(output, "Target: {platform}  {device_name}");
    }
    let _ = writeln!(
        output,
        "Samples: {}  Total CPU: {:.0}ms",
        summary.sample_count,
        summary.total_weight_ns as f64 / 1_000_000.0
    );

    if summary.total_weight_ns > 0 && summary.unsymbolicated_user_weight_ns > 0 {
        let unsymbolicated_pct =
            100.0 * summary.unsymbolicated_user_weight_ns as f64 / summary.total_weight_ns as f64;
        let _ = writeln!(
            output,
            "Note: {:.0}% of user samples are unsymbolicated or runtime-only",
            unsymbolicated_pct
        );
    }

    output.push('\n');

    if summary.self_time.is_empty() {
        output.push_str("No symbolicated user frames found.\n");
        return output;
    }

    output.push_str("SELF TIME\n");
    for (frame, weight_ns) in top_frame_weights(&summary.self_time) {
        let pct = 100.0 * weight_ns as f64 / summary.total_weight_ns as f64;
        if pct < DIAGNOSIS_THRESHOLD_PERCENT {
            break;
        }
        let _ = writeln!(
            output,
            "  {:5.1}%  {:6.0}ms  {}  {}",
            pct,
            weight_ns as f64 / 1_000_000.0,
            frame.binary_name,
            frame.name
        );
    }

    let mut callers = top_frame_weights(&summary.total_time)
        .into_iter()
        .filter(|(frame, total_weight)| {
            let self_weight = summary.self_time.get(frame).copied().unwrap_or_default();
            *total_weight > self_weight.saturating_mul(11) / 10
        })
        .collect::<Vec<_>>();
    if !callers.is_empty() {
        output.push('\n');
        output.push_str("TOTAL TIME (callers with significant overhead)\n");
        callers.truncate(DIAGNOSIS_MAX_ITEMS);
        for (frame, weight_ns) in callers {
            let pct = 100.0 * weight_ns as f64 / summary.total_weight_ns as f64;
            if pct < DIAGNOSIS_THRESHOLD_PERCENT {
                break;
            }
            let _ = writeln!(
                output,
                "  {:5.1}%  {:6.0}ms  {}  {}",
                pct,
                weight_ns as f64 / 1_000_000.0,
                frame.binary_name,
                frame.name
            );
        }
    }

    output.push('\n');
    output.push_str("CALL STACKS\n");
    let mut stacks = summary
        .stack_time
        .iter()
        .map(|(stack, weight)| (stack, *weight))
        .collect::<Vec<_>>();
    stacks.sort_by(|left, right| right.1.cmp(&left.1));
    for (stack, weight_ns) in stacks.into_iter().take(DIAGNOSIS_MAX_ITEMS) {
        let pct = 100.0 * weight_ns as f64 / summary.total_weight_ns as f64;
        if pct < DIAGNOSIS_THRESHOLD_PERCENT {
            break;
        }
        let chain = stack
            .iter()
            .rev()
            .map(|frame| frame.name.as_str())
            .collect::<Vec<_>>()
            .join(" > ");
        let _ = writeln!(
            output,
            "  {:5.1}%  {:6.0}ms  {}",
            pct,
            weight_ns as f64 / 1_000_000.0,
            chain
        );
    }

    output
}

fn parse_allocations_statistics(xml: &str) -> Result<Vec<AllocationStatRow>> {
    let document =
        Document::parse(xml).context("failed to parse xctrace allocations statistics XML")?;
    let mut rows = Vec::new();
    for row in document
        .descendants()
        .filter(|node| node.has_tag_name("row"))
    {
        rows.push(AllocationStatRow {
            category: row.attribute("category").unwrap_or("<unknown>").to_owned(),
            persistent_bytes: parse_u64_attribute(row, "persistent-bytes"),
            transient_bytes: parse_u64_attribute(row, "transient-bytes"),
            total_bytes: parse_u64_attribute(row, "total-bytes"),
            count_persistent: parse_u64_attribute(row, "count-persistent"),
            count_transient: parse_u64_attribute(row, "count-transient"),
            count_events: parse_u64_attribute(row, "count-events"),
        });
    }
    Ok(rows)
}

fn parse_allocations_list(xml: &str) -> Result<Vec<AllocationListRow>> {
    let document = Document::parse(xml).context("failed to parse xctrace allocations list XML")?;
    let mut rows = Vec::new();
    for row in document
        .descendants()
        .filter(|node| node.has_tag_name("row"))
    {
        rows.push(AllocationListRow {
            size_bytes: parse_u64_attribute(row, "size"),
            live: row.attribute("live") == Some("true"),
            responsible_caller: row
                .attribute("responsible-caller")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
            responsible_library: row
                .attribute("responsible-library")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned),
        });
    }
    Ok(rows)
}

fn render_allocations_diagnosis(
    metadata: &TraceMetadata,
    stats: &[AllocationStatRow],
    rows: &[AllocationListRow],
) -> String {
    let mut output = String::new();
    let process_name = if metadata.process_name.is_empty() {
        "<unknown>"
    } else {
        metadata.process_name.as_str()
    };
    let template_name = if metadata.template_name.is_empty() {
        "<unknown>"
    } else {
        metadata.template_name.as_str()
    };
    let _ = writeln!(
        output,
        "Process: {process_name}  Duration: {:.1}s  Template: {template_name}",
        metadata.duration_s
    );
    if let (Some(platform), Some(device_name)) = (
        metadata.device_platform.as_deref(),
        metadata.device_name.as_deref(),
    ) {
        let _ = writeln!(output, "Target: {platform}  {device_name}");
    }

    let overall = stats
        .iter()
        .find(|row| row.category == "All Heap & Anonymous VM")
        .or_else(|| {
            stats
                .iter()
                .find(|row| row.category == "All Heap Allocations")
        })
        .or_else(|| stats.iter().find(|row| row.category == "All VM Regions"));
    if let Some(overall) = overall {
        let _ = writeln!(
            output,
            "Live bytes: {}  Live allocations: {}",
            format_bytes(overall.persistent_bytes),
            overall.count_persistent
        );
        let _ = writeln!(
            output,
            "Transient bytes: {}  Transient allocations: {}",
            format_bytes(overall.transient_bytes),
            overall.count_transient
        );
        let _ = writeln!(
            output,
            "Total bytes: {}  Allocation events: {}",
            format_bytes(overall.total_bytes),
            overall.count_events
        );
    }

    let live_rows = stats
        .iter()
        .filter(|row| !is_aggregate_allocation_category(&row.category) && row.persistent_bytes > 0)
        .collect::<Vec<_>>();
    let transient_rows = stats
        .iter()
        .filter(|row| !is_aggregate_allocation_category(&row.category) && row.transient_bytes > 0)
        .collect::<Vec<_>>();

    output.push('\n');
    output.push_str("LIVE BY CATEGORY\n");
    if live_rows.is_empty() {
        output.push_str("  No live allocation categories found.\n");
    } else {
        let mut live_rows = live_rows;
        live_rows.sort_by(|left, right| {
            right
                .persistent_bytes
                .cmp(&left.persistent_bytes)
                .then_with(|| left.category.cmp(&right.category))
        });
        for row in live_rows.into_iter().take(DIAGNOSIS_MAX_ITEMS) {
            let _ = writeln!(
                output,
                "  {:>10}  {:>6}  {}",
                format_bytes(row.persistent_bytes),
                row.count_persistent,
                row.category
            );
        }
    }

    output.push('\n');
    output.push_str("TRANSIENT BY CATEGORY\n");
    if transient_rows.is_empty() {
        output.push_str("  No transient allocation categories found.\n");
    } else {
        let mut transient_rows = transient_rows;
        transient_rows.sort_by(|left, right| {
            right
                .transient_bytes
                .cmp(&left.transient_bytes)
                .then_with(|| left.category.cmp(&right.category))
        });
        for row in transient_rows.into_iter().take(DIAGNOSIS_MAX_ITEMS) {
            let _ = writeln!(
                output,
                "  {:>10}  {:>6}  {}",
                format_bytes(row.transient_bytes),
                row.count_transient,
                row.category
            );
        }
    }

    let mut caller_totals = HashMap::<AllocationCallerKey, (u64, u64)>::new();
    for row in rows.iter().filter(|row| row.live) {
        let Some(caller) = row
            .responsible_caller
            .as_deref()
            .filter(|caller| allocation_caller_is_useful(caller))
        else {
            continue;
        };
        let key = AllocationCallerKey {
            library: row
                .responsible_library
                .clone()
                .filter(|library| !library.is_empty())
                .unwrap_or_else(|| metadata.process_name.clone()),
            caller: caller.to_owned(),
        };
        let entry = caller_totals.entry(key).or_insert((0, 0));
        entry.0 += row.size_bytes;
        entry.1 += 1;
    }

    if !caller_totals.is_empty() {
        output.push('\n');
        output.push_str("LIVE BY RESPONSIBLE CALLER\n");
        let mut caller_totals = caller_totals.into_iter().collect::<Vec<_>>();
        caller_totals.sort_by(|left, right| {
            right
                .1
                .0
                .cmp(&left.1.0)
                .then_with(|| left.0.library.cmp(&right.0.library))
                .then_with(|| left.0.caller.cmp(&right.0.caller))
        });
        for (key, (bytes, count)) in caller_totals.into_iter().take(DIAGNOSIS_MAX_ITEMS) {
            let _ = writeln!(
                output,
                "  {:>10}  {:>6}  {}  {}",
                format_bytes(bytes),
                count,
                key.library,
                key.caller
            );
        }
    }

    output
}

fn summarize_time_profile(samples: &[TraceSample]) -> TimeProfileSummary {
    let mut summary = TimeProfileSummary {
        sample_count: samples.len(),
        total_weight_ns: samples.iter().map(|sample| sample.weight_ns).sum(),
        ..TimeProfileSummary::default()
    };

    for sample in samples {
        let user_frames = sample
            .frames
            .iter()
            .filter(|frame| frame.is_user)
            .collect::<Vec<_>>();
        if user_frames.is_empty() {
            continue;
        }

        let useful_frames = user_frames
            .iter()
            .copied()
            .filter(|frame| frame.is_symbolicated && !looks_like_runtime_internal(&frame.key.name))
            .collect::<Vec<_>>();
        if useful_frames.is_empty() {
            summary.unsymbolicated_user_weight_ns += sample.weight_ns;
            continue;
        }

        *summary
            .self_time
            .entry(useful_frames[0].key.clone())
            .or_default() += sample.weight_ns;

        let mut seen = HashSet::new();
        for frame in &useful_frames {
            if seen.insert(frame.key.clone()) {
                *summary.total_time.entry(frame.key.clone()).or_default() += sample.weight_ns;
            }
        }

        let stack_key = useful_frames
            .iter()
            .take(DIAGNOSIS_STACK_DEPTH)
            .map(|frame| frame.key.clone())
            .collect::<Vec<_>>();
        *summary.stack_time.entry(stack_key).or_default() += sample.weight_ns;
    }

    summary
}

fn top_frame_weights(weights: &HashMap<FrameKey, u64>) -> Vec<(FrameKey, u64)> {
    let mut items = weights
        .iter()
        .map(|(frame, weight)| (frame.clone(), *weight))
        .collect::<Vec<_>>();
    items.sort_by(|left, right| {
        right
            .1
            .cmp(&left.1)
            .then_with(|| left.0.binary_name.cmp(&right.0.binary_name))
            .then_with(|| left.0.name.cmp(&right.0.name))
    });
    items
}

fn parse_u64_attribute<'a, 'input>(node: Node<'a, 'input>, attribute: &str) -> u64 {
    node.attribute(attribute)
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or_default()
}

fn is_aggregate_allocation_category(category: &str) -> bool {
    category == "destroyed event" || category.starts_with("All ")
}

fn allocation_caller_is_useful(caller: &str) -> bool {
    let caller = caller.trim();
    !caller.is_empty()
        && caller != "<Call stack limit reached>"
        && caller != "<unknown>"
        && !looks_like_runtime_internal(caller)
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit_index = 0usize;
    while value >= 1024.0 && unit_index < UNITS.len() - 1 {
        value /= 1024.0;
        unit_index += 1;
    }

    if unit_index == 0 {
        format!("{bytes} {}", UNITS[unit_index])
    } else {
        format!("{value:.1} {}", UNITS[unit_index])
    }
}

fn resolve_row_weight<'a, 'input>(
    row: Node<'a, 'input>,
    registry: &HashMap<String, Node<'a, 'input>>,
) -> u64 {
    child_element(row, "weight")
        .map(|weight| resolve_ref(weight, registry))
        .and_then(|weight| weight.text())
        .and_then(|weight| weight.parse::<u64>().ok())
        .unwrap_or(1_000_000)
}

fn resolve_row_backtrace<'a, 'input>(
    row: Node<'a, 'input>,
    registry: &HashMap<String, Node<'a, 'input>>,
) -> Option<Node<'a, 'input>> {
    let tagged_backtrace = child_element(row, "tagged-backtrace")
        .or_else(|| child_element(row, "backtrace"))
        .map(|node| resolve_ref(node, registry))?;
    if tagged_backtrace.has_tag_name("backtrace") {
        return Some(tagged_backtrace);
    }
    child_element(tagged_backtrace, "backtrace").map(|node| resolve_ref(node, registry))
}

fn extract_frames<'a, 'input>(
    backtrace: Node<'a, 'input>,
    registry: &HashMap<String, Node<'a, 'input>>,
    metadata: &TraceMetadata,
) -> Vec<TraceFrame> {
    backtrace
        .children()
        .filter(|node| node.has_tag_name("frame"))
        .map(|frame| resolve_ref(frame, registry))
        .map(|frame| build_frame(frame, registry, metadata))
        .collect()
}

fn build_frame<'a, 'input>(
    frame: Node<'a, 'input>,
    registry: &HashMap<String, Node<'a, 'input>>,
    metadata: &TraceMetadata,
) -> TraceFrame {
    let frame_name = frame.attribute("name").unwrap_or("<unknown>").to_owned();
    let address = frame.attribute("addr").map(str::to_owned);
    let binary = child_element(frame, "binary").map(|binary| resolve_ref(binary, registry));
    let binary_path = binary
        .and_then(|binary| binary.attribute("path"))
        .map(str::to_owned);
    let binary_name = binary
        .and_then(|binary| binary.attribute("name"))
        .map(str::to_owned)
        .or_else(|| {
            classify_user_frame(
                &frame_name,
                address.as_deref(),
                binary_path.as_deref(),
                metadata,
            )
            .then(|| metadata.process_name.clone())
        })
        .unwrap_or_else(|| "<unknown>".to_owned());
    let is_symbolicated = !frame_name.starts_with("0x")
        && frame_name != "<deduplicated_symbol>"
        && frame_name != "<unknown>";
    let is_user = classify_user_frame(
        &frame_name,
        address.as_deref(),
        binary_path.as_deref(),
        metadata,
    );

    TraceFrame {
        key: FrameKey {
            binary_name,
            name: frame_name,
        },
        is_user,
        is_symbolicated,
    }
}

fn classify_user_frame(
    frame_name: &str,
    address: Option<&str>,
    binary_path: Option<&str>,
    metadata: &TraceMetadata,
) -> bool {
    if let Some(binary_path) = binary_path {
        if is_system_binary_path(binary_path) {
            return false;
        }
        if let Some(process_path) = metadata.process_path.as_deref() {
            if binary_path == process_path {
                return true;
            }
            if let Some(bundle_root) = bundle_root(process_path)
                && binary_path.starts_with(bundle_root)
            {
                return true;
            }
        }
        if binary_path.contains(".app/") || binary_path.ends_with(".app") {
            return true;
        }
        if binary_path.contains("/DerivedData/") || binary_path.contains("/Build/Products/") {
            return true;
        }
        return !binary_path.is_empty();
    }

    let address_like = address.unwrap_or(frame_name);
    looks_like_user_address(address_like)
}

fn bundle_root(path: &str) -> Option<&str> {
    path.find(".app").map(|index| &path[..index + 4])
}

fn is_system_binary_path(path: &str) -> bool {
    path.starts_with("/System/")
        || path.starts_with("/usr/lib/")
        || path.contains("/System/Library/")
        || path.contains("/usr/lib/system/")
        || path.contains("/Symbols/System/")
        || path.contains("/Symbols/usr/lib/")
}

fn looks_like_user_address(value: &str) -> bool {
    value.starts_with("0x10") || value.starts_with("0x11") || value.starts_with("0x12")
}

fn looks_like_runtime_internal(name: &str) -> bool {
    name.starts_with("__swift_")
        || name.starts_with("_swift_")
        || name.starts_with("swift_")
        || name.starts_with("__objc_")
        || name.starts_with("DYLD-STUB$$")
}

fn resolve_ref<'a, 'input>(
    node: Node<'a, 'input>,
    registry: &HashMap<String, Node<'a, 'input>>,
) -> Node<'a, 'input> {
    node.attribute("ref")
        .and_then(|reference| registry.get(reference).copied())
        .unwrap_or(node)
}

fn child_element<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    node.children().find(|child| child.has_tag_name(name))
}

fn child_text<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<String> {
    child_element(node, name)
        .and_then(|child| child.text())
        .map(str::to_owned)
}

fn default_trace_output(root: &Path, kind: ProfileKind) -> Result<PathBuf> {
    let output_path = root
        .join(".orbit")
        .join("artifacts")
        .join("profiles")
        .join(format!(
            "{}-{}.trace",
            timestamp_slug(),
            profile_kind_slug(kind)
        ));
    validate_trace_output_path(&output_path)?;
    Ok(output_path)
}

fn validate_trace_output_path(output_path: &Path) -> Result<()> {
    if output_path.exists() && output_path.is_dir() {
        bail!(
            "trace output must be a `.trace` path, not a directory: {}",
            output_path.display()
        );
    }
    if output_path.extension().and_then(|value| value.to_str()) != Some("trace") {
        bail!(
            "trace output must end with `.trace`: {}",
            output_path.display()
        );
    }
    if output_path.exists() {
        bail!(
            "trace output already exists; choose a new path: {}",
            output_path.display()
        );
    }

    ensure_parent_dir(output_path)?;
    if let Some(parent) = output_path.parent() {
        ensure_dir(parent)?;
    }
    Ok(())
}

fn profile_kind_template(kind: ProfileKind) -> &'static str {
    match kind {
        ProfileKind::Cpu => "Time Profiler",
        ProfileKind::Memory => "Allocations",
    }
}

fn profile_kind_label(kind: ProfileKind) -> &'static str {
    match kind {
        ProfileKind::Cpu => "CPU",
        ProfileKind::Memory => "memory",
    }
}

fn profile_kind_slug(kind: ProfileKind) -> &'static str {
    match kind {
        ProfileKind::Cpu => "cpu",
        ProfileKind::Memory => "memory",
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;
    use std::thread;
    use std::time::Duration;

    use tempfile::tempdir;

    use super::{
        TraceRecording, TraceRecordingBackend, parse_allocations_list,
        parse_allocations_statistics, parse_time_profile_samples, parse_trace_metadata,
        render_allocations_diagnosis, render_time_profile_diagnosis, wait_for_trace_recording_exit,
    };
    use crate::cli::ProfileKind;

    const SAMPLE_TOC_XML: &str = r#"<?xml version="1.0"?>
<trace-toc>
  <run number="1">
    <info>
      <target>
        <device platform="macOS" name="Example Mac"/>
        <process name="Orbit"/>
      </target>
      <summary>
        <duration>6.0</duration>
        <template-name>Time Profiler</template-name>
      </summary>
    </info>
    <processes>
      <process name="Orbit" path="/Applications/Orbit.app/Contents/MacOS/Orbit"/>
    </processes>
    <data>
      <table schema="time-profile"/>
    </data>
  </run>
</trace-toc>"#;

    const SAMPLE_TIME_PROFILE_XML: &str = r#"<?xml version="1.0"?>
<trace-query-result>
  <node xpath='//trace-toc[1]/run[1]/data[1]/table[1]'>
    <schema name="time-profile"/>
    <row>
      <weight id="1">3000000</weight>
      <tagged-backtrace id="2">
        <backtrace id="3">
          <frame id="4" name="heavyWork()" addr="0x102000100">
            <binary id="5" name="Orbit" path="/Applications/Orbit.app/Contents/MacOS/Orbit"/>
          </frame>
          <frame id="6" name="main" addr="0x102000050">
            <binary ref="5"/>
          </frame>
        </backtrace>
      </tagged-backtrace>
    </row>
    <row>
      <weight ref="1"/>
      <tagged-backtrace id="7">
        <backtrace id="8">
          <frame id="9" name="sin" addr="0x180000100">
            <binary id="10" name="libsystem_m.dylib" path="/usr/lib/system/libsystem_m.dylib"/>
          </frame>
          <frame ref="4"/>
          <frame ref="6"/>
        </backtrace>
      </tagged-backtrace>
    </row>
    <row>
      <weight id="11">1000000</weight>
      <tagged-backtrace id="12">
        <backtrace id="13">
          <frame id="14" name="0x102000200" addr="0x102000200"/>
          <frame ref="6"/>
        </backtrace>
      </tagged-backtrace>
    </row>
  </node>
</trace-query-result>"#;

    const SAMPLE_ALLOCATIONS_TOC_XML: &str = r#"<?xml version="1.0"?>
<trace-toc>
  <run number="1">
    <info>
      <target>
        <device platform="macOS" name="Example Mac"/>
        <process name="Orbit"/>
      </target>
      <summary>
        <duration>5.0</duration>
        <template-name>Allocations</template-name>
      </summary>
    </info>
    <tracks>
      <track name="Allocations">
        <details>
          <detail name="Statistics" kind="table"/>
          <detail name="Allocations List" kind="table"/>
        </details>
      </track>
    </tracks>
  </run>
</trace-toc>"#;

    const SAMPLE_ALLOCATIONS_STATISTICS_XML: &str = r#"<?xml version="1.0"?>
<trace-query-result>
  <node xpath='//trace-toc[1]/run[1]/tracks[1]/track[1]/details[1]/detail[1]'>
    <row category="All Heap &amp; Anonymous VM" persistent-bytes="33782272" count-persistent="1161" total-bytes="34183680" transient-bytes="401408" count-events="1183" count-transient="6" count-total="1167"/>
    <row category="All Heap Allocations" persistent-bytes="33782272" count-persistent="1161" total-bytes="33790464" transient-bytes="8192" count-events="1175" count-transient="2" count-total="1163"/>
    <row category="All Anonymous VM" persistent-bytes="0" count-persistent="0" total-bytes="393216" transient-bytes="393216" count-events="8" count-transient="4" count-total="4"/>
    <row category="Malloc 256.0 KiB" persistent-bytes="33554432" count-persistent="128" total-bytes="33554432" transient-bytes="0" count-events="128" count-transient="0" count-total="128"/>
    <row category="Malloc 48 Bytes" persistent-bytes="8208" count-persistent="171" total-bytes="8208" transient-bytes="0" count-events="171" count-transient="0" count-total="171"/>
    <row category="VM: Anonymous VM" persistent-bytes="0" count-persistent="0" total-bytes="393216" transient-bytes="393216" count-events="8" count-transient="4" count-total="4"/>
  </node>
</trace-query-result>"#;

    const SAMPLE_ALLOCATIONS_LIST_XML: &str = r#"<?xml version="1.0"?>
<trace-query-result>
  <node xpath='//trace-toc[1]/run[1]/tracks[1]/track[1]/details[1]/detail[2]'>
    <row address="0x10133c000" category="Malloc 256.0 KiB" live="true" responsible-caller="allocateChunk()" responsible-library="Orbit" size="262144"/>
    <row address="0x10137c000" category="Malloc 256.0 KiB" live="true" responsible-caller="allocateChunk()" responsible-library="Orbit" size="262144"/>
    <row address="0x10139c000" category="Malloc 48 Bytes" live="true" responsible-caller="bootstrap()" responsible-library="Orbit" size="48"/>
    <row address="0x10139c100" category="VM: Anonymous VM" live="false" responsible-caller="&lt;Call stack limit reached&gt;" responsible-library="" size="393216"/>
  </node>
</trace-query-result>"#;

    #[test]
    fn interrupted_trace_wait_returns_written_output_even_if_child_exits_non_zero() {
        let temp = tempdir().unwrap();
        let output_path = temp.path().join("capture.sample.txt");
        let script_path = temp.path().join("writer.py");
        fs::write(
            &script_path,
            format!(
                r#"import pathlib, signal, time

def handler(signum, frame):
    return None

signal.signal(signal.SIGINT, handler)
signal.signal(signal.SIGTERM, handler)
end = time.time() + 0.4
while time.time() < end:
    try:
        time.sleep(0.05)
    except InterruptedError:
        pass
pathlib.Path(r"{}").write_text("sample")
raise SystemExit(130)
"#,
                output_path.display()
            ),
        )
        .unwrap();

        let child = Command::new("python3").arg(&script_path).spawn().unwrap();
        let recording = TraceRecording {
            output_path: output_path.clone(),
            selected_xcode: None,
            child,
            debug: "writer".to_owned(),
            backend: TraceRecordingBackend::PlainFile,
        };

        let (interrupt_tx, interrupt_rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(150));
            let _ = interrupt_tx.send(());
        });

        let path = wait_for_trace_recording_exit(ProfileKind::Cpu, recording, Some(&interrupt_rx))
            .unwrap();

        assert_eq!(path, output_path);
        assert_eq!(fs::read_to_string(&output_path).unwrap(), "sample");
    }

    #[test]
    fn summarizes_time_profile_xml_into_hotspots() {
        let metadata = parse_trace_metadata(SAMPLE_TOC_XML).unwrap();
        let samples = parse_time_profile_samples(SAMPLE_TIME_PROFILE_XML, &metadata).unwrap();
        let summary = render_time_profile_diagnosis(&metadata, &samples);

        assert!(summary.contains("Process: Orbit"));
        assert!(summary.contains("Template: Time Profiler"));
        assert!(summary.contains("Samples: 3"));
        assert!(summary.contains("Orbit  heavyWork()"));
        assert!(summary.contains("Orbit  main"));
        assert!(summary.contains("main > heavyWork()"));
    }

    #[test]
    fn summarizes_allocations_xml_into_memory_diagnosis() {
        let metadata = parse_trace_metadata(SAMPLE_ALLOCATIONS_TOC_XML).unwrap();
        let stats = parse_allocations_statistics(SAMPLE_ALLOCATIONS_STATISTICS_XML).unwrap();
        let rows = parse_allocations_list(SAMPLE_ALLOCATIONS_LIST_XML).unwrap();
        let summary = render_allocations_diagnosis(&metadata, &stats, &rows);

        assert!(summary.contains("Template: Allocations"));
        assert!(summary.contains("Live bytes: 32.2 MiB"));
        assert!(summary.contains("LIVE BY CATEGORY"));
        assert!(summary.contains("Malloc 256.0 KiB"));
        assert!(summary.contains("TRANSIENT BY CATEGORY"));
        assert!(summary.contains("VM: Anonymous VM"));
        assert!(summary.contains("LIVE BY RESPONSIBLE CALLER"));
        assert!(summary.contains("allocateChunk()"));
    }
}
