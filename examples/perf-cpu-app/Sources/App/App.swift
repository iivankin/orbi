import Foundation
import OSLog
import SwiftUI

private let cpuLogger = Logger(subsystem: "dev.orbi.examples.perfcpu", category: "fixture")
private let cpuStressWorker = CPUStressWorker()

@main
struct PerfCPUApp: App {
    init() {
        cpuLogger.notice("PerfCPUApp launched")
        cpuStressWorker.start()
    }

    var body: some Scene {
        WindowGroup { CPUTraceFixtureView() }
    }
}

private struct CPUTraceFixtureView: View {
    @State private var hasStarted = false
    @State private var lastDigest: UInt64 = 0
    @State private var runCount = 0
    @State private var status = "Ready"
    @State private var backgroundWorkerRunning = false

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("CPU trace fixture")
                .font(.title2.bold())

            Text(status)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("cpu-status")

            Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 8) {
                GridRow {
                    Text("Runs")
                    Text("\(runCount)")
                        .monospacedDigit()
                        .accessibilityIdentifier("cpu-run-count")
                }

                GridRow {
                    Text("Digest")
                    Text(String(lastDigest, radix: 16))
                        .font(.system(.body, design: .monospaced))
                        .lineLimit(1)
                        .accessibilityIdentifier("cpu-digest")
                }
            }

            HStack(spacing: 10) {
                Button("Run foreground spike") {
                    runForegroundSpike()
                }
                .accessibilityIdentifier("run-foreground-cpu-spike")

                Button(backgroundWorkerRunning ? "Stop background worker" : "Start background worker") {
                    toggleBackgroundWorker()
                }
                .accessibilityIdentifier("toggle-background-cpu-worker")
            }
        }
        .padding(24)
        .frame(minWidth: 460, minHeight: 260)
        .onAppear {
            guard !hasStarted else { return }
            hasStarted = true
            backgroundWorkerRunning = true
            status = "Background CPU worker running"
        }
    }

    private func runForegroundSpike() {
        status = "Running foreground CPU spike on the main actor"
        let start = Date()
        let digest = CPUHotLoop().run(iterations: 12_000_000)
        let elapsedMS = Int(Date().timeIntervalSince(start) * 1_000)

        lastDigest = digest
        runCount += 1
        status = "Completed foreground CPU spike in \(elapsedMS) ms"
        cpuLogger.notice("Foreground CPU spike completed in \(elapsedMS, privacy: .public) ms digest \(digest, privacy: .public)")
    }

    private func toggleBackgroundWorker() {
        if backgroundWorkerRunning {
            cpuStressWorker.stop()
            backgroundWorkerRunning = false
            status = "Background CPU worker stopped"
        } else {
            cpuStressWorker.start()
            backgroundWorkerRunning = true
            status = "Background CPU worker running"
        }
    }
}

private final class CPUStressWorker {
    private var thread: Thread?

    func start() {
        guard thread == nil else { return }

        let workerThread = Thread {
            Thread.current.name = "Orbi CPU Hot Loop"
            var digest: UInt64 = 0

            // This fixture intentionally burns CPU for the app lifetime so
            // short `orbi run --trace cpu` sessions always contain hot samples.
            while !Thread.current.isCancelled {
                digest &+= CPUHotLoop().run(iterations: 2_000_000)
            }

            cpuLogger.notice("Background CPU worker stopped with digest \(digest, privacy: .public)")
        }
        workerThread.qualityOfService = .userInitiated
        workerThread.start()
        thread = workerThread
        cpuLogger.notice("Background CPU worker started")
    }

    func stop() {
        thread?.cancel()
        thread = nil
        cpuLogger.notice("Background CPU worker cancellation requested")
    }
}

private struct CPUHotLoop {
    @inline(never)
    func run(iterations: Int) -> UInt64 {
        var state: UInt64 = 0x9E37_79B9_7F4A_7C15

        // This fixture intentionally burns CPU synchronously so profiler samples
        // have a stable, obvious hot path without requiring UI automation.
        for index in 0..<iterations {
            state = mix(state &+ UInt64(index))

            if index.isMultiple(of: 2_048) {
                state ^= UInt64(primeCount(upTo: 97 + (index & 127)))
            }
        }

        return state
    }

    @inline(never)
    private func mix(_ value: UInt64) -> UInt64 {
        var mixed = value
        mixed ^= mixed >> 30
        mixed &*= 0xBF58_476D_1CE4_E5B9
        mixed ^= mixed >> 27
        mixed &*= 0x94D0_49BB_1331_11EB
        mixed ^= mixed >> 31
        return mixed
    }

    @inline(never)
    private func primeCount(upTo limit: Int) -> Int {
        var count = 0

        for candidate in 2...limit {
            if isPrime(candidate) {
                count += 1
            }
        }

        return count
    }

    @inline(never)
    private func isPrime(_ value: Int) -> Bool {
        guard value > 1 else { return false }
        guard value > 3 else { return true }

        let maxDivisor = Int(Double(value).squareRoot())
        for divisor in 2...maxDivisor {
            if value.isMultiple(of: divisor) {
                return false
            }
        }

        return true
    }
}
