import Foundation
import OSLog
import SwiftUI

private let stallLogger = Logger(subsystem: "dev.orbi.examples.perfmainthreadstall", category: "fixture")

@main
struct PerfMainThreadStallApp: App {
    init() {
        stallLogger.notice("PerfMainThreadStallApp launched")
        let start = Date()

        // Run during launch so short trace sessions capture a deterministic
        // main-thread stall without waiting for window rendering or UI actions.
        let digest = MainThreadStallWorkload().run(iterations: 40_000_000)

        let elapsedMS = Int(Date().timeIntervalSince(start) * 1_000)
        stallLogger.notice("Startup main thread stall completed in \(elapsedMS, privacy: .public) ms digest \(digest, privacy: .public)")
    }

    var body: some Scene {
        WindowGroup { MainThreadStallFixtureView() }
    }
}

private struct MainThreadStallFixtureView: View {
    @State private var lastDigest: UInt64 = 0
    @State private var stallCount = 0
    @State private var status = "Startup stall ran during app launch"

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Main thread stall fixture")
                .font(.title2.bold())

            Text(status)
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("main-thread-stall-status")

            Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 8) {
                GridRow {
                    Text("Stalls")
                    Text("\(stallCount)")
                        .monospacedDigit()
                        .accessibilityIdentifier("main-thread-stall-count")
                }

                GridRow {
                    Text("Digest")
                    Text(String(lastDigest, radix: 16))
                        .font(.system(.body, design: .monospaced))
                        .lineLimit(1)
                        .accessibilityIdentifier("main-thread-stall-digest")
                }
            }

            Button("Run main thread stall") {
                runMainThreadStall(label: "Manual", iterations: 40_000_000)
            }
            .accessibilityIdentifier("run-main-thread-stall")
        }
        .padding(24)
        .frame(minWidth: 500, minHeight: 260)
    }

    private func runMainThreadStall(label: String, iterations: Int) {
        status = "\(label) main thread stall running"
        let start = Date()

        // This fixture intentionally blocks the main thread so CPU traces have
        // an obvious UI-freeze stack without involving background worker noise.
        let digest = MainThreadStallWorkload().run(iterations: iterations)

        let elapsedMS = Int(Date().timeIntervalSince(start) * 1_000)
        lastDigest = digest
        stallCount += 1
        status = "\(label) main thread stall completed in \(elapsedMS) ms"
        stallLogger.notice("\(label, privacy: .public) main thread stall completed in \(elapsedMS, privacy: .public) ms digest \(digest, privacy: .public)")
    }
}

private struct MainThreadStallWorkload {
    @inline(never)
    func run(iterations: Int) -> UInt64 {
        var state: UInt64 = 0xD1B5_4A32_D192_ED03

        for index in 0..<iterations {
            state = recomputeLayoutState(state &+ UInt64(index))

            if index.isMultiple(of: 4_096) {
                state ^= measureTextRun(seed: state, glyphCount: 64 + (index & 127))
            }
        }

        return state
    }

    @inline(never)
    private func recomputeLayoutState(_ value: UInt64) -> UInt64 {
        var state = value
        state ^= state >> 33
        state &*= 0xFF51_AFD7_ED55_8CCD
        state ^= state >> 29
        state &*= 0xC4CE_B9FE_1A85_EC53
        state ^= state >> 32
        return state
    }

    @inline(never)
    private func measureTextRun(seed: UInt64, glyphCount: Int) -> UInt64 {
        var width: UInt64 = seed & 0xFF

        for glyph in 0..<glyphCount {
            let mixedGlyph = (glyph &* 31) ^ Int(truncatingIfNeeded: seed)
            let advance = UInt64(truncatingIfNeeded: mixedGlyph)
            width &+= recomputeLayoutState(advance) & 0x3FF
        }

        return width
    }
}
