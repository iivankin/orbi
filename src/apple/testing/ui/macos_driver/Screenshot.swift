import AppKit
import Foundation
import ScreenCaptureKit

struct ScreenCaptureWindowTarget {
    let window: SCWindow
    let display: SCDisplay
    let captureRect: CGRect
    let pointPixelScale: CGFloat
}

func screenRecordingAvailable() async -> Bool {
    do {
        _ = try await shareableContent()
        return true
    } catch {
        return false
    }
}

func captureWindowScreenshot(pid: pid_t, outputPath: String) async throws {
    let target = try await targetScreenCaptureWindow(pid: pid)
    try await captureWindowWithScreenCaptureKit(target: target, outputPath: outputPath)
}

func targetScreenCaptureWindow(pid: pid_t) async throws -> ScreenCaptureWindowTarget {
    let content = try await shareableContent()
    let windows = content.windows.filter { window in
        window.owningApplication?.processID == pid && window.isOnScreen && window.windowLayer == 0
    }
    guard let window = windows.max(by: { left, right in
        (left.frame.width * left.frame.height) < (right.frame.width * right.frame.height)
    }) else {
        throw HelperFailure("could not find an on-screen window for pid \(pid)")
    }
    let display = content.displays.max(by: { left, right in
        intersectionArea(left.frame, window.frame) < intersectionArea(right.frame, window.frame)
    })
    guard let display else {
        throw HelperFailure("could not find a display for window \(window.windowID)")
    }
    let captureRect = visibleCaptureRect(window: window, display: display)
    let filter = SCContentFilter(display: display, including: [window])
    return ScreenCaptureWindowTarget(
        window: window,
        display: display,
        captureRect: captureRect,
        pointPixelScale: max(1.0, CGFloat(filter.pointPixelScale))
    )
}

private func captureWindowWithScreenCaptureKit(target: ScreenCaptureWindowTarget, outputPath: String) async throws {
    let filter = SCContentFilter(display: target.display, including: [target.window])
    let config = SCStreamConfiguration()
    config.sourceRect = target.captureRect
    config.width = max(1, Int((target.captureRect.width * target.pointPixelScale).rounded()))
    config.height = max(1, Int((target.captureRect.height * target.pointPixelScale).rounded()))
    config.showsCursor = false

    let image = try await captureImage(filter: filter, configuration: config)
    let rep = NSBitmapImageRep(cgImage: image)
    guard let png = rep.representation(using: .png, properties: [:]) else {
        throw HelperFailure("failed to encode screenshot as PNG")
    }
    try png.write(to: URL(fileURLWithPath: outputPath), options: .atomic)
}

private func visibleCaptureRect(window: SCWindow, display: SCDisplay) -> CGRect {
    let rect = window.frame.intersection(display.frame)
    if !rect.isNull && !rect.isEmpty {
        return rect
    }
    return window.frame
}

private func intersectionArea(_ left: CGRect, _ right: CGRect) -> CGFloat {
    let intersection = left.intersection(right)
    guard !intersection.isNull && !intersection.isEmpty else {
        return 0
    }
    return intersection.width * intersection.height
}

private func shareableContent() async throws -> SCShareableContent {
    try await withCheckedThrowingContinuation { continuation in
        SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: true) { content, error in
            if let content {
                continuation.resume(returning: content)
            } else {
                continuation.resume(throwing: error ?? HelperFailure("failed to read ScreenCaptureKit shareable content"))
            }
        }
    }
}

private func captureImage(filter: SCContentFilter, configuration: SCStreamConfiguration) async throws -> CGImage {
    try await withCheckedThrowingContinuation { continuation in
        SCScreenshotManager.captureImage(contentFilter: filter, configuration: configuration) { image, error in
            if let image {
                continuation.resume(returning: image)
            } else {
                continuation.resume(throwing: error ?? HelperFailure("ScreenCaptureKit returned no image"))
            }
        }
    }
}
