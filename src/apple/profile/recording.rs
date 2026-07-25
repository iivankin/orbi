use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
#[cfg(unix)]
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::apple::xcode::SelectedXcode;
use crate::cli::ProfileKind;
use crate::util::{command_output_allow_failure, debug_command, ensure_parent_dir, timestamp_slug};
use anyhow::{Context, Result, bail};
#[cfg(unix)]
use signal_hook::consts::signal::SIGINT;
#[cfg(unix)]
use signal_hook::iterator::{Handle as SignalHandle, Signals};

const TRACE_RECORDING_FINALIZE_TIMEOUT: Duration = Duration::from_secs(90);
const TRACE_RECORDING_INTERRUPT_GRACE: Duration = Duration::from_millis(250);

pub(crate) struct TraceRecording {
    output_path: PathBuf,
    child: Child,
    debug: String,
    interrupt_grace: Duration,
}

struct LaunchedTraceRequest<'a> {
    root: &'a Path,
    kind: ProfileKind,
    launch_command: &'a [String],
    launch_environment: &'a [(String, String)],
}

#[cfg(unix)]
struct SignalForwarder {
    handle: SignalHandle,
    thread: Option<JoinHandle<()>>,
}

#[cfg(not(unix))]
struct SignalForwarder;

pub(crate) fn start_optional_launched_command_trace(
    root: &Path,
    _selected_xcode: Option<&SelectedXcode>,
    _interactive: bool,
    kind: Option<ProfileKind>,
    launch_command: &[String],
    launch_environment: &[(String, String)],
    _device: Option<&str>,
) -> Result<Option<(ProfileKind, TraceRecording)>> {
    kind.map(|kind| {
        start_launched_trace(LaunchedTraceRequest {
            root,
            kind,
            launch_command,
            launch_environment,
        })
        .map(|recording| (kind, recording))
    })
    .transpose()
}

pub(crate) fn ensure_simulator_profiling_supported(kind: Option<ProfileKind>) -> Result<()> {
    let _ = kind;
    Ok(())
}

pub(crate) fn trace_launch_environment(
    kind: ProfileKind,
    output_path: &Path,
) -> Vec<(String, String)> {
    vec![
        ("ORBI_TRACE_MODE".to_owned(), kind.trace_slug().to_owned()),
        (
            "ORBI_TRACE_OUTPUT".to_owned(),
            output_path.display().to_string(),
        ),
        ("OS_ACTIVITY_DT_MODE".to_owned(), "1".to_owned()),
        ("IDEPreferLogStreaming".to_owned(), "YES".to_owned()),
    ]
}

pub(crate) fn wait_for_launched_trace_exit(
    kind: ProfileKind,
    recording: TraceRecording,
) -> Result<()> {
    // `orbi run --trace` tells the user to press Ctrl-C to stop the recording.
    // Forward the interrupt to the traced process and then wait for the in-process
    // runtime to flush its JSON file.
    let (interrupt_tx, interrupt_rx) = mpsc::channel();
    let signal_forwarder = SignalForwarder::install(interrupt_tx)?;
    let path = wait_for_trace_recording_exit(kind, recording, Some(&interrupt_rx))?;
    drop(signal_forwarder);
    println!("trace: {}", path.display());
    Ok(())
}

fn start_launched_trace(request: LaunchedTraceRequest<'_>) -> Result<TraceRecording> {
    if request.launch_command.is_empty() {
        bail!("trace launch requires at least one launch argument");
    }

    let output_path = default_trace_output(request.root, request.kind)?;
    let mut command = build_orbi_trace_launch_command(&request, &output_path);
    let debug = debug_command(&command);
    let child = command
        .spawn()
        .with_context(|| format!("failed to execute `{debug}`"))?;
    Ok(TraceRecording {
        output_path,
        child,
        debug,
        interrupt_grace: TRACE_RECORDING_INTERRUPT_GRACE,
    })
}

fn build_orbi_trace_launch_command(
    request: &LaunchedTraceRequest<'_>,
    output_path: &Path,
) -> Command {
    let mut command = Command::new(&request.launch_command[0]);
    command.args(&request.launch_command[1..]);
    for (key, value) in trace_launch_environment(request.kind, output_path) {
        command.env(key, value);
    }
    for (key, value) in request.launch_environment {
        command.env(key, value);
    }
    command
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
                return finish_trace_recording(recording)
                    .with_context(|| format!("failed to finalize {} trace", kind.trace_label()));
            }

            if interrupted {
                verify_recording_output(&recording).with_context(|| {
                    format!(
                        "failed to finalize {} trace after interruption",
                        kind.trace_label()
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
    finish_orbi_runtime_recording(recording)
}

fn verify_recording_output(recording: &TraceRecording) -> Result<()> {
    wait_for_recording_output_path(recording, Duration::from_secs(5))
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

fn finish_orbi_runtime_recording(mut recording: TraceRecording) -> Result<PathBuf> {
    let graceful_wait_started = Instant::now();
    while graceful_wait_started.elapsed() < recording.interrupt_grace {
        if let Some(status) = recording.child.try_wait()? {
            if recording.output_path.exists() {
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
    while started.elapsed() < TRACE_RECORDING_FINALIZE_TIMEOUT {
        if let Some(status) = recording.child.try_wait()? {
            let output_path = recording.output_path.clone();
            return wait_for_recording_output_path(&recording, Duration::from_secs(5))
                .map(|()| output_path)
                .with_context(|| {
                    format!(
                        "`{}` exited with {status} before writing {}",
                        recording.debug,
                        recording.output_path.display()
                    )
                });
        }
        thread::sleep(Duration::from_millis(100));
    }

    let _ = recording.child.kill();
    let _ = recording.child.wait();
    if recording.output_path.exists() {
        return Ok(recording.output_path);
    }

    bail!(
        "timed out waiting for `{}` to finish writing trace file at {}",
        recording.debug,
        recording.output_path.display()
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
    #[cfg(unix)]
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

    #[cfg(not(unix))]
    fn install(_interrupt_tx: mpsc::Sender<()>) -> Result<Self> {
        Ok(Self)
    }
}

#[cfg(unix)]
impl Drop for SignalForwarder {
    fn drop(&mut self) {
        self.handle.close();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

pub(crate) fn default_trace_output(root: &Path, kind: ProfileKind) -> Result<PathBuf> {
    let output_path = root
        .join(".orbi")
        .join("artifacts")
        .join("profiles")
        .join(format!(
            "{}-{}.orbitrace.json",
            timestamp_slug(),
            kind.trace_slug()
        ));
    validate_trace_output_path(&output_path)?;
    Ok(output_path)
}

fn validate_trace_output_path(output_path: &Path) -> Result<()> {
    if output_path.exists() && output_path.is_dir() {
        bail!(
            "trace output must be a `.orbitrace.json` path, not a directory: {}",
            output_path.display()
        );
    }
    if !output_path
        .file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value.ends_with(".orbitrace.json"))
    {
        bail!(
            "trace output must end with `.orbitrace.json`: {}",
            output_path.display()
        );
    }
    if output_path.exists() {
        bail!(
            "trace output already exists; choose a new path: {}",
            output_path.display()
        );
    }

    ensure_parent_dir(output_path)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::process::Command;
    use std::thread;
    use std::time::{Duration, Instant};

    use tempfile::tempdir;

    use super::{
        LaunchedTraceRequest, TraceRecording, build_orbi_trace_launch_command,
        wait_for_trace_recording_exit,
    };
    use crate::cli::ProfileKind;

    #[test]
    fn launched_trace_command_sets_runtime_environment() {
        let root = PathBuf::from("/tmp");
        let launch_command = [
            "/tmp/App.app/Contents/MacOS/App".to_owned(),
            "--fixture".to_owned(),
        ];
        let launch_environment = [("ORBI_FIXTURE".to_owned(), "enabled".to_owned())];
        let request = LaunchedTraceRequest {
            root: root.as_path(),
            kind: ProfileKind::Cpu,
            launch_command: &launch_command,
            launch_environment: &launch_environment,
        };
        let command = build_orbi_trace_launch_command(
            &request,
            PathBuf::from("/tmp/run.orbitrace.json").as_path(),
        );
        assert_eq!(command.get_program().to_string_lossy(), launch_command[0]);
        assert_eq!(
            command
                .get_args()
                .map(|value| value.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            vec!["--fixture"]
        );
        let env = command
            .get_envs()
            .map(|(key, value)| {
                (
                    key.to_string_lossy().into_owned(),
                    value
                        .map(|value| value.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                )
            })
            .collect::<std::collections::BTreeMap<_, _>>();
        assert_eq!(env.get("ORBI_TRACE_MODE").map(String::as_str), Some("cpu"));
        assert_eq!(
            env.get("ORBI_TRACE_OUTPUT").map(String::as_str),
            Some("/tmp/run.orbitrace.json")
        );
        assert_eq!(env.get("ORBI_FIXTURE").map(String::as_str), Some("enabled"));
    }

    #[test]
    fn interrupted_trace_wait_returns_written_output_even_if_child_exits_non_zero() {
        let temp = tempdir().unwrap();
        let output_path = temp.path().join("capture.sample.txt");
        let ready_path = temp.path().join("writer.ready");
        let script_path = temp.path().join("writer.py");
        fs::write(
            &script_path,
            format!(
                r#"import pathlib, signal, time

def handler(signum, frame):
    return None

signal.signal(signal.SIGINT, handler)
signal.signal(signal.SIGTERM, handler)
pathlib.Path(r"{}").write_text("ready")
end = time.time() + 0.4
while time.time() < end:
    try:
        time.sleep(0.05)
    except InterruptedError:
        pass
pathlib.Path(r"{}").write_text("sample")
raise SystemExit(130)
"#,
                ready_path.display(),
                output_path.display()
            ),
        )
        .unwrap();

        let child = Command::new("python3")
            .arg("-S")
            .arg(&script_path)
            .spawn()
            .unwrap();
        let recording = TraceRecording {
            output_path: output_path.clone(),
            child,
            debug: "writer".to_owned(),
            interrupt_grace: super::TRACE_RECORDING_INTERRUPT_GRACE,
        };

        let (interrupt_tx, interrupt_rx) = std::sync::mpsc::channel();
        let interrupt_ready_path = ready_path.clone();
        thread::spawn(move || {
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(2) {
                if interrupt_ready_path.exists() {
                    break;
                }
                thread::sleep(Duration::from_millis(10));
            }
            thread::sleep(Duration::from_millis(50));
            let _ = interrupt_tx.send(());
        });

        let path = wait_for_trace_recording_exit(ProfileKind::Cpu, recording, Some(&interrupt_rx))
            .unwrap();

        assert_eq!(path, output_path);
        assert_eq!(fs::read_to_string(&output_path).unwrap(), "sample");
    }
}
