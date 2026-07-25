import AVFoundation
import CoreMedia
import CoreVideo
import Foundation
import ScreenCaptureKit

@available(macOS 15.0, *)
// SCRecordingOutput callbacks and the finalize timeout arrive off-main; mutable state is guarded by stateLock.
final class WindowVideoRecorder: NSObject, SCRecordingOutputDelegate, SCStreamDelegate, @unchecked Sendable {
    private let target: ScreenCaptureWindowTarget
    private let outputURL: URL
    private let stateLock = NSLock()
    private var stream: SCStream?
    private var recordingOutput: SCRecordingOutput?
    private var finishContinuation: CheckedContinuation<Void, Error>?
    private var finished = false
    private var failure: Error?

    init(target: ScreenCaptureWindowTarget, outputPath: String) throws {
        self.target = target
        self.outputURL = URL(fileURLWithPath: outputPath)
        try FileManager.default.createDirectory(
            at: outputURL.deletingLastPathComponent(),
            withIntermediateDirectories: true
        )
        if FileManager.default.fileExists(atPath: outputURL.path) {
            try FileManager.default.removeItem(at: outputURL)
        }
    }

    func start() async throws {
        let filter = SCContentFilter(display: target.display, including: [target.window])
        let streamConfiguration = SCStreamConfiguration()
        let size = Self.pixelSize(contentRect: target.captureRect, pointPixelScale: target.pointPixelScale)
        streamConfiguration.sourceRect = target.captureRect
        streamConfiguration.width = size.width
        streamConfiguration.height = size.height
        streamConfiguration.pixelFormat = kCVPixelFormatType_32BGRA
        streamConfiguration.minimumFrameInterval = CMTime(value: 1, timescale: 30)
        streamConfiguration.queueDepth = 5
        streamConfiguration.showsCursor = false
        streamConfiguration.capturesAudio = false
        streamConfiguration.captureMicrophone = false
        streamConfiguration.excludesCurrentProcessAudio = true

        let recordingConfiguration = SCRecordingOutputConfiguration()
        recordingConfiguration.outputURL = outputURL
        recordingConfiguration.outputFileType = .mp4
        recordingConfiguration.videoCodecType = .h264

        let recordingOutput = SCRecordingOutput(
            configuration: recordingConfiguration,
            delegate: self
        )
        let stream = SCStream(filter: filter, configuration: streamConfiguration, delegate: self)
        try stream.addRecordingOutput(recordingOutput)
        self.stream = stream
        self.recordingOutput = recordingOutput
        try await stream.startCapture()
    }

    func stop() async throws {
        guard let stream else {
            return
        }
        do {
            try await stream.stopCapture()
        } catch {
            completeFinish(.failure(error))
        }
        try await waitForFinish()
        self.stream = nil
        self.recordingOutput = nil
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        completeFinish(.failure(error))
    }

    func recordingOutputDidFinishRecording(_ recordingOutput: SCRecordingOutput) {
        completeFinish(.success(()))
    }

    func recordingOutput(_ recordingOutput: SCRecordingOutput, didFailWithError error: Error) {
        completeFinish(.failure(error))
    }

    private func waitForFinish() async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            stateLock.lock()
            if let failure {
                stateLock.unlock()
                continuation.resume(throwing: failure)
                return
            }
            if finished {
                stateLock.unlock()
                continuation.resume()
                return
            }
            finishContinuation = continuation
            stateLock.unlock()

            DispatchQueue.global().asyncAfter(deadline: .now() + 5.0) { [weak self] in
                self?.completeFinish(.failure(HelperFailure("timed out finalizing macOS video recording")))
            }
        }
    }

    private func completeFinish(_ result: Result<Void, Error>) {
        stateLock.lock()
        if finished || failure != nil {
            stateLock.unlock()
            return
        }
        let continuation = finishContinuation
        finishContinuation = nil
        switch result {
        case .success:
            finished = true
        case .failure(let error):
            failure = error
        }
        stateLock.unlock()

        switch result {
        case .success:
            continuation?.resume()
        case .failure(let error):
            continuation?.resume(throwing: error)
        }
    }

    private static func pixelSize(contentRect: CGRect, pointPixelScale: CGFloat) -> (width: Int, height: Int) {
        let scale = max(1.0, pointPixelScale)
        return (
            width: evenDimension(max(2, Int((contentRect.width * scale).rounded()))),
            height: evenDimension(max(2, Int((contentRect.height * scale).rounded())))
        )
    }

    private static func evenDimension(_ value: Int) -> Int {
        value.isMultiple(of: 2) ? value : value + 1
    }
}
