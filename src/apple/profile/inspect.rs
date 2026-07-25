use std::collections::{BTreeSet, HashMap};
use std::fmt::Write as _;
use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use super::symbolicate::{
    SymbolicationImage, SymbolicationRequest, TraceSymbolication, parse_address, render_stack,
    render_stack_detail, resolve_dsym_dir,
};
use crate::cli::InspectTraceArgs;
use crate::context::AppContext;
use crate::util::resolve_path;

const DIAGNOSIS_MAX_ITEMS: usize = 10;

#[derive(Debug, Deserialize)]
struct OrbiTraceDocument {
    format: String,
    mode: String,
    arch: Option<String>,
    #[serde(default, rename = "loadedLibraries")]
    loaded_libraries: Vec<OrbiLoadedLibrary>,
    #[serde(rename = "startedAtUnixNanos")]
    started_at_unix_nanos: Option<u64>,
    cpu: Option<OrbiCpuTrace>,
    memory: Option<OrbiMemoryTrace>,
}

#[derive(Debug, Deserialize)]
struct OrbiLoadedLibrary {
    path: String,
    uuid: Option<String>,
    #[serde(rename = "loadAddress")]
    load_address: String,
    size: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct OrbiCpuTrace {
    #[serde(rename = "sampleIntervalMicros")]
    sample_interval_micros: Option<u64>,
    #[serde(rename = "droppedSamples")]
    dropped_samples: Option<u64>,
    #[serde(rename = "failedUnwinds")]
    failed_unwinds: Option<u64>,
    threads: Vec<OrbiCpuThread>,
}

#[derive(Debug, Deserialize)]
struct OrbiCpuThread {
    name: String,
    samples: Vec<OrbiCpuSample>,
}

#[derive(Debug, Deserialize)]
struct OrbiCpuSample {
    stack: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct OrbiMemoryTrace {
    summary: OrbiMemorySummary,
    dropped: Option<OrbiMemoryDropped>,
    stacks: Vec<OrbiAllocationStack>,
}

#[derive(Debug, Deserialize)]
struct OrbiMemorySummary {
    #[serde(rename = "totalAllocatedBytes")]
    total_allocated_bytes: u64,
    #[serde(rename = "allocationEvents")]
    allocation_events: u64,
    #[serde(rename = "liveBytes")]
    live_bytes: u64,
    #[serde(rename = "liveAllocations")]
    live_allocations: u64,
    #[serde(rename = "peakLiveBytes")]
    peak_live_bytes: u64,
}

#[derive(Debug, Deserialize)]
struct OrbiMemoryDropped {
    #[serde(rename = "allocationRecords")]
    allocation_records: u64,
    #[serde(rename = "allocationStacks")]
    allocation_stacks: u64,
    #[serde(rename = "processSamples")]
    process_samples: u64,
    #[serde(rename = "unknownFrees")]
    unknown_frees: u64,
}

#[derive(Debug, Deserialize)]
struct OrbiAllocationStack {
    stack: Vec<String>,
    #[serde(rename = "totalAllocatedBytes")]
    total_allocated_bytes: u64,
    #[serde(rename = "allocationCount")]
    allocation_count: u64,
    #[serde(rename = "liveBytes")]
    live_bytes: u64,
    #[serde(rename = "liveAllocationCount")]
    live_allocation_count: u64,
    #[serde(rename = "peakLiveBytes")]
    peak_live_bytes: u64,
}

fn inspect_orbi_trace_file(
    trace_path: &std::path::Path,
    dsym_dirs: &[PathBuf],
    symbolication_enabled: bool,
) -> Result<String> {
    let contents = fs::read_to_string(trace_path)
        .with_context(|| format!("failed to read {}", trace_path.display()))?;
    let document: OrbiTraceDocument =
        serde_json::from_str(&contents).context("failed to parse Orbi trace JSON")?;
    if document.format != "orbi.trace.v1" {
        bail!("unsupported trace format `{}`", document.format);
    }
    let symbols = symbolication_enabled
        .then(|| build_symbolication(trace_path, dsym_dirs, &document))
        .flatten();
    Ok(render_orbi_trace_diagnosis(&document, symbols.as_ref()))
}

fn render_orbi_trace_diagnosis(
    document: &OrbiTraceDocument,
    symbols: Option<&TraceSymbolication>,
) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "Orbi Trace");
    let _ = writeln!(output, "Mode: {}", document.mode);
    if let Some(symbols) = symbols
        && symbols.total_unique_addresses > 0
    {
        let _ = writeln!(
            output,
            "Symbolication: {}/{} unique addresses",
            symbols.symbolicated_addresses, symbols.total_unique_addresses
        );
    }
    if let Some(started) = document.started_at_unix_nanos {
        let _ = writeln!(output, "Started: {started} ns since Unix epoch");
    }
    match document.mode.as_str() {
        "cpu" => {
            if let Some(cpu) = document.cpu.as_ref() {
                render_orbi_cpu_trace(&mut output, cpu, symbols);
            }
        }
        "memory" => {
            if let Some(memory) = document.memory.as_ref() {
                render_orbi_memory_trace(&mut output, memory, symbols);
            }
        }
        _ => {
            let _ = writeln!(output, "Unsupported Orbi trace mode.");
        }
    }
    output
}

fn build_symbolication(
    trace_path: &std::path::Path,
    dsym_dirs: &[PathBuf],
    document: &OrbiTraceDocument,
) -> Option<TraceSymbolication> {
    let addresses = collect_trace_addresses(document);
    if addresses.is_empty() || document.loaded_libraries.is_empty() {
        return None;
    }
    let images = document
        .loaded_libraries
        .iter()
        .filter_map(|library| {
            parse_address(&library.load_address).map(|load_address| SymbolicationImage {
                path: library.path.clone(),
                uuid: library.uuid.clone(),
                load_address,
                size: library.size,
            })
        })
        .collect::<Vec<_>>();
    if images.is_empty() {
        return None;
    }
    Some(TraceSymbolication::symbolicate(SymbolicationRequest {
        trace_path,
        arch: document.arch.as_deref(),
        dsym_dirs,
        images: &images,
        addresses,
    }))
}

fn collect_trace_addresses(document: &OrbiTraceDocument) -> BTreeSet<u64> {
    let mut addresses = BTreeSet::new();
    if let Some(cpu) = document.cpu.as_ref() {
        for thread in &cpu.threads {
            for sample in &thread.samples {
                addresses.extend(
                    sample
                        .stack
                        .iter()
                        .filter_map(|address| parse_address(address)),
                );
            }
        }
    }
    if let Some(memory) = document.memory.as_ref() {
        for stack in &memory.stacks {
            addresses.extend(
                stack
                    .stack
                    .iter()
                    .filter_map(|address| parse_address(address)),
            );
        }
    }
    addresses
}

fn render_orbi_cpu_trace(
    output: &mut String,
    cpu: &OrbiCpuTrace,
    symbols: Option<&TraceSymbolication>,
) {
    let sample_count = cpu
        .threads
        .iter()
        .map(|thread| thread.samples.len())
        .sum::<usize>();
    let _ = writeln!(
        output,
        "Sample interval: {} us",
        cpu.sample_interval_micros.unwrap_or_default()
    );
    let _ = writeln!(output, "Threads: {}", cpu.threads.len());
    let _ = writeln!(output, "Samples: {sample_count}");
    let _ = writeln!(
        output,
        "Dropped samples: {}  Failed unwinds: {}",
        cpu.dropped_samples.unwrap_or_default(),
        cpu.failed_unwinds.unwrap_or_default()
    );

    if let Some(symbols) = symbols {
        let mut user_stacks: HashMap<String, u64> = HashMap::new();
        let mut system_stacks: HashMap<String, u64> = HashMap::new();
        for thread in &cpu.threads {
            for sample in &thread.samples {
                if sample.stack.is_empty() {
                    *system_stacks.entry("<empty stack>".to_owned()).or_default() += 1;
                    continue;
                }
                let rendered = render_stack_detail(&sample.stack, Some(symbols));
                let stacks = if rendered.has_user_code {
                    &mut user_stacks
                } else {
                    &mut system_stacks
                };
                *stacks.entry(rendered.display).or_default() += 1;
            }
        }

        let _ = writeln!(output, "\nTop CPU stacks:");
        if user_stacks.is_empty() {
            write_cpu_stack_group(output, system_stacks, sample_count, DIAGNOSIS_MAX_ITEMS);
        } else {
            write_cpu_stack_group(output, user_stacks, sample_count, DIAGNOSIS_MAX_ITEMS);
            if !system_stacks.is_empty() {
                let _ = writeln!(output, "\nTop system-only CPU stacks:");
                write_cpu_stack_group(output, system_stacks, sample_count, 3);
            }
        }
    } else {
        let mut stacks: HashMap<String, u64> = HashMap::new();
        for thread in &cpu.threads {
            for sample in &thread.samples {
                let key = if sample.stack.is_empty() {
                    "<empty stack>".to_owned()
                } else {
                    render_stack(&sample.stack, None)
                };
                *stacks.entry(key).or_default() += 1;
            }
        }
        let _ = writeln!(output, "\nTop CPU stacks:");
        write_cpu_stack_group(output, stacks, sample_count, DIAGNOSIS_MAX_ITEMS);
    }

    let _ = writeln!(output, "\nThread samples:");
    for thread in &cpu.threads {
        let _ = writeln!(output, "  {}: {}", thread.name, thread.samples.len());
    }
}

fn write_cpu_stack_group(
    output: &mut String,
    stacks: HashMap<String, u64>,
    sample_count: usize,
    max_items: usize,
) {
    let mut top = stacks.into_iter().collect::<Vec<_>>();
    top.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    for (stack, count) in top.into_iter().take(max_items) {
        let percent = if sample_count == 0 {
            0.0
        } else {
            (count as f64 / sample_count as f64) * 100.0
        };
        let _ = writeln!(output, "  {percent:>5.1}%  {count:>6} samples  {stack}");
    }
}

fn render_orbi_memory_trace(
    output: &mut String,
    memory: &OrbiMemoryTrace,
    symbols: Option<&TraceSymbolication>,
) {
    let summary = &memory.summary;
    let _ = writeln!(
        output,
        "Total allocated: {} ({})",
        summary.total_allocated_bytes,
        crate::util::human_bytes(summary.total_allocated_bytes)
    );
    let _ = writeln!(
        output,
        "Live bytes: {} ({})  Live allocations: {}",
        summary.live_bytes,
        crate::util::human_bytes(summary.live_bytes),
        summary.live_allocations
    );
    let _ = writeln!(
        output,
        "Peak live bytes: {} ({})  Allocation events: {}",
        summary.peak_live_bytes,
        crate::util::human_bytes(summary.peak_live_bytes),
        summary.allocation_events
    );
    if let Some(dropped) = memory.dropped.as_ref() {
        let _ = writeln!(
            output,
            "Dropped allocation records: {}  stack records: {}  process samples: {}  unknown frees: {}",
            dropped.allocation_records,
            dropped.allocation_stacks,
            dropped.process_samples,
            dropped.unknown_frees
        );
    }

    let mut by_live = memory.stacks.iter().collect::<Vec<_>>();
    by_live.sort_by(|left, right| {
        right
            .live_bytes
            .cmp(&left.live_bytes)
            .then_with(|| right.total_allocated_bytes.cmp(&left.total_allocated_bytes))
    });
    let _ = writeln!(output, "\nTop live allocation stacks:");
    for stack in by_live
        .into_iter()
        .filter(|stack| stack.live_bytes > 0)
        .take(DIAGNOSIS_MAX_ITEMS)
    {
        let _ = writeln!(
            output,
            "  {} live in {} allocation(s), peak {}, total {} in {} event(s)",
            crate::util::human_bytes(stack.live_bytes),
            stack.live_allocation_count,
            crate::util::human_bytes(stack.peak_live_bytes),
            crate::util::human_bytes(stack.total_allocated_bytes),
            stack.allocation_count
        );
        let _ = writeln!(output, "    {}", render_stack(&stack.stack, symbols));
    }

    let mut by_total = memory.stacks.iter().collect::<Vec<_>>();
    by_total.sort_by_key(|stack| std::cmp::Reverse(stack.total_allocated_bytes));
    let _ = writeln!(output, "\nTop allocation churn stacks:");
    for stack in by_total.into_iter().take(DIAGNOSIS_MAX_ITEMS) {
        let _ = writeln!(
            output,
            "  {} total in {} event(s), live {}",
            crate::util::human_bytes(stack.total_allocated_bytes),
            stack.allocation_count,
            crate::util::human_bytes(stack.live_bytes)
        );
        let _ = writeln!(output, "    {}", render_stack(&stack.stack, symbols));
    }
}

pub fn inspect_trace_command(app: &AppContext, args: &InspectTraceArgs) -> Result<()> {
    let trace_path = resolve_path(&app.cwd, &args.trace);
    let dsym_dirs = args
        .dsym_dirs
        .iter()
        .map(|dir| resolve_dsym_dir(&app.cwd, dir))
        .collect::<Vec<_>>();
    let summary = inspect_orbi_trace_file(&trace_path, &dsym_dirs, !args.no_symbolication)?;
    print!("{summary}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{OrbiTraceDocument, render_orbi_trace_diagnosis};
    use crate::apple::profile::symbolicate::TraceSymbolication;

    #[test]
    fn summarizes_orbi_cpu_trace_json() {
        let document: OrbiTraceDocument = serde_json::from_str(
            r#"{"format":"orbi.trace.v1","formatVersion":1,"mode":"cpu","startedAtUnixNanos":1,"cpu":{"sampleIntervalMicros":4500,"droppedSamples":0,"failedUnwinds":1,"threads":[{"id":1,"name":"main","samples":[{"timeNanos":1,"stack":["0x1000","0x2000"]},{"timeNanos":2,"stack":["0x1000","0x2000"]}]},{"id":2,"name":"worker","samples":[{"timeNanos":3,"stack":["0x3000"]}]}]}}"#,
        )
        .unwrap();

        let summary = render_orbi_trace_diagnosis(&document, None);

        assert!(summary.contains("Orbi Trace"));
        assert!(summary.contains("Mode: cpu"));
        assert!(summary.contains("Sample interval: 4500 us"));
        assert!(summary.contains("Samples: 3"));
        assert!(summary.contains("66.7%       2 samples  0x1000 <- 0x2000"));
        assert!(summary.contains("worker: 1"));
    }

    #[test]
    fn summarizes_orbi_memory_trace_json() {
        let document: OrbiTraceDocument = serde_json::from_str(
            r#"{"format":"orbi.trace.v1","formatVersion":1,"mode":"memory","startedAtUnixNanos":1,"memory":{"summary":{"totalAllocatedBytes":4096,"allocationEvents":4,"liveBytes":1024,"liveAllocations":1,"peakLiveBytes":2048},"dropped":{"allocationRecords":1,"allocationStacks":2,"processSamples":3,"unknownFrees":4},"processMemorySamples":[],"stacks":[{"stack":["0x1000","0x2000"],"totalAllocatedBytes":4096,"allocationCount":4,"liveBytes":1024,"liveAllocationCount":1,"peakLiveBytes":2048}]}}"#,
        )
        .unwrap();

        let summary = render_orbi_trace_diagnosis(&document, None);

        assert!(summary.contains("Mode: memory"));
        assert!(summary.contains("Total allocated: 4096 (4.0 KiB)"));
        assert!(summary.contains("Live bytes: 1024 (1.0 KiB)"));
        assert!(summary.contains("Dropped allocation records: 1"));
        assert!(summary.contains("0x1000 <- 0x2000"));
    }

    #[test]
    fn summarizes_symbolicated_orbi_cpu_trace_json() {
        let document: OrbiTraceDocument = serde_json::from_str(
            r#"{"format":"orbi.trace.v1","formatVersion":1,"mode":"cpu","arch":"arm64","loadedLibraries":[{"path":"/tmp/App","uuid":"AAAAAAAA-BBBB-CCCC-DDDD-EEEEEEEEEEEE","loadAddress":"0x1000","size":8192}],"startedAtUnixNanos":1,"cpu":{"sampleIntervalMicros":4500,"droppedSamples":0,"failedUnwinds":0,"threads":[{"id":1,"name":"main","samples":[{"timeNanos":1,"stack":["0x1100","0x1200"]}]}]}}"#,
        )
        .unwrap();
        let symbols = TraceSymbolication::for_test([
            (0x1100, "CPUHotLoop.run(iterations:)"),
            (0x1200, "CPUStressWorker.start()"),
        ]);

        let summary = render_orbi_trace_diagnosis(&document, Some(&symbols));

        assert!(summary.contains("Symbolication: 2/2 unique addresses"));
        assert!(summary.contains("CPUHotLoop.run(iterations:) <- CPUStressWorker.start()"));
    }
}
