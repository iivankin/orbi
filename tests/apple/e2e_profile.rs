use std::fs;

use crate::support::{base_command, create_home, read_log, run_and_capture};
use tempfile::tempdir;

#[test]
fn orbi_inspect_trace_prints_orbi_cpu_diagnosis() {
    let temp = tempdir().unwrap();
    let home = create_home(temp.path());
    let workspace = temp.path().join("workspace");
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("mock.log");
    let trace_path = workspace.join("sample-cpu.orbitrace.json");

    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&mock_bin).unwrap();
    fs::write(
        &trace_path,
        r#"{"format":"orbi.trace.v1","formatVersion":1,"mode":"cpu","startedAtUnixNanos":1,"cpu":{"sampleIntervalMicros":4500,"droppedSamples":0,"failedUnwinds":1,"threads":[{"id":1,"name":"main","samples":[{"timeNanos":1,"stack":["0x1000","0x2000"]},{"timeNanos":2,"stack":["0x1000","0x2000"]}]},{"id":2,"name":"worker","samples":[{"timeNanos":3,"stack":["0x3000"]}]}]}}"#,
    )
    .unwrap();

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args(["inspect-trace", "sample-cpu.orbitrace.json"]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Orbi Trace"));
    assert!(stdout.contains("Mode: cpu"));
    assert!(stdout.contains("Sample interval: 4500 us"));
    assert!(stdout.contains("Samples: 3"));
    assert!(stdout.contains("0x1000 <- 0x2000"));

    let log = read_log(&log_path);
    assert!(!log.contains("xctrace"));
}

#[test]
fn orbi_inspect_trace_prints_orbi_memory_diagnosis() {
    let temp = tempdir().unwrap();
    let home = create_home(temp.path());
    let workspace = temp.path().join("workspace");
    let mock_bin = temp.path().join("mock-bin");
    let log_path = temp.path().join("mock.log");
    let trace_path = workspace.join("sample-memory.orbitrace.json");

    fs::create_dir_all(&workspace).unwrap();
    fs::create_dir_all(&mock_bin).unwrap();
    fs::write(
        &trace_path,
        r#"{"format":"orbi.trace.v1","formatVersion":1,"mode":"memory","startedAtUnixNanos":1,"memory":{"summary":{"totalAllocatedBytes":4096,"allocationEvents":4,"liveBytes":1024,"liveAllocations":1,"peakLiveBytes":2048},"dropped":{"allocationRecords":1,"allocationStacks":2,"processSamples":3,"unknownFrees":4},"processMemorySamples":[],"stacks":[{"stack":["0x1000","0x2000"],"totalAllocatedBytes":4096,"allocationCount":4,"liveBytes":1024,"liveAllocationCount":1,"peakLiveBytes":2048}]}}"#,
    )
    .unwrap();

    let mut command = base_command(&workspace, &home, &mock_bin, &log_path);
    command.args(["inspect-trace", "sample-memory.orbitrace.json"]);
    let output = run_and_capture(&mut command);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Mode: memory"));
    assert!(stdout.contains("Total allocated: 4096 (4.0 KiB)"));
    assert!(stdout.contains("Live bytes: 1024 (1.0 KiB)"));
    assert!(stdout.contains("Dropped allocation records: 1"));
    assert!(stdout.contains("0x1000 <- 0x2000"));

    let log = read_log(&log_path);
    assert!(!log.contains("xctrace"));
}
