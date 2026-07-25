import OSLog
import SwiftUI

private let memoryLogger = Logger(subsystem: "dev.orbi.examples.perfmemory", category: "fixture")

@main
struct PerfMemoryApp: App {
    init() {
        memoryLogger.notice("PerfMemoryApp launched")
    }

    var body: some Scene {
        WindowGroup { MemoryTraceFixtureView() }
    }
}

private struct MemoryTraceFixtureView: View {
    @State private var hasStarted = false
    @State private var retainedBlocks: [[UInt8]] = []
    @State private var churnChecksum: UInt64 = 0
    @State private var status = "Ready"

    private var retainedMegabytes: Int {
        retainedBlocks.reduce(0) { total, block in
            total + block.count
        } / (1_024 * 1_024)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Memory trace fixture")
                .font(.title2.bold())

            Text(status)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("memory-status")

            Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 8) {
                GridRow {
                    Text("Retained")
                    Text("\(retainedMegabytes) MB")
                        .monospacedDigit()
                        .accessibilityIdentifier("retained-memory")
                }

                GridRow {
                    Text("Checksum")
                    Text(String(churnChecksum, radix: 16))
                        .font(.system(.body, design: .monospaced))
                        .lineLimit(1)
                        .accessibilityIdentifier("memory-checksum")
                }
            }

            HStack(spacing: 10) {
                Button("Retain 64 MB") {
                    retainHeap(megabytes: 64)
                }
                .accessibilityIdentifier("retain-heap")

                Button("Churn allocations") {
                    churnAllocations(rounds: 160, blockSize: 512 * 1_024)
                }
                .accessibilityIdentifier("churn-allocations")

                Button("Reset") {
                    retainedBlocks.removeAll(keepingCapacity: false)
                    status = "Released retained blocks"
                }
                .accessibilityIdentifier("reset-memory")
            }
        }
        .padding(24)
        .frame(minWidth: 520, minHeight: 280)
        .onAppear {
            guard !hasStarted else { return }
            hasStarted = true
            retainHeap(megabytes: 64)
            churnAllocations(rounds: 160, blockSize: 512 * 1_024)
        }
    }

    private func retainHeap(megabytes: Int) {
        let blocks = MemoryWorkload.makeRetainedBlocks(megabytes: megabytes)
        retainedBlocks.append(contentsOf: blocks)
        status = "Retained \(retainedMegabytes) MB"
        memoryLogger.notice("Retained \(retainedMegabytes, privacy: .public) MB")
    }

    private func churnAllocations(rounds: Int, blockSize: Int) {
        churnChecksum = MemoryWorkload().churn(rounds: rounds, blockSize: blockSize)
        status = "Churned \(rounds) transient allocation rounds"
        memoryLogger.notice("Allocation churn completed checksum \(churnChecksum, privacy: .public)")
    }
}

private struct MemoryWorkload {
    @inline(never)
    static func makeRetainedBlocks(megabytes: Int) -> [[UInt8]] {
        var blocks: [[UInt8]] = []
        blocks.reserveCapacity(megabytes)

        // These blocks are intentionally retained by SwiftUI state so memory
        // traces can distinguish live heap growth from short-lived churn.
        for index in 0..<megabytes {
            var block = [UInt8](repeating: UInt8(truncatingIfNeeded: index), count: 1_024 * 1_024)
            touch(&block, seed: index)
            blocks.append(block)
        }

        return blocks
    }

    @inline(never)
    func churn(rounds: Int, blockSize: Int) -> UInt64 {
        var checksum: UInt64 = 0

        for round in 0..<rounds {
            var block = [UInt8](repeating: UInt8(truncatingIfNeeded: round), count: blockSize)
            MemoryWorkload.touch(&block, seed: round)
            checksum &+= UInt64(block[0])
            checksum &+= UInt64(block[block.count - 1])
        }

        return checksum
    }

    @inline(never)
    private static func touch(_ block: inout [UInt8], seed: Int) {
        var offset = 0

        while offset < block.count {
            block[offset] = UInt8(truncatingIfNeeded: seed &+ offset)
            offset += 4_096
        }

        if !block.isEmpty {
            block[block.count - 1] = UInt8(truncatingIfNeeded: seed)
        }
    }
}
