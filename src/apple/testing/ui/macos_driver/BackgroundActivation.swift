import AppKit
import ApplicationServices
import CoreGraphics
import Darwin
import Foundation

struct BackgroundActivationWindow {
    let id: CGWindowID
    let frame: CGRect
}

func backgroundActivationWindow(pid: pid_t, point: CGPoint? = nil) -> BackgroundActivationWindow? {
    guard let raw = CGWindowListCopyWindowInfo(
        [.optionOnScreenOnly, .excludeDesktopElements],
        kCGNullWindowID
    ) as? [[String: Any]] else {
        return nil
    }
    let candidates = raw.compactMap { entry -> (window: BackgroundActivationWindow, area: Double)? in
        guard (entry[kCGWindowOwnerPID as String] as? Int32) == pid,
              (entry[kCGWindowLayer as String] as? Int ?? 0) == 0,
              let windowNumber = entry[kCGWindowNumber as String] as? Int,
              let bounds = entry[kCGWindowBounds as String] as? [String: Double]
        else {
            return nil
        }
        let width = bounds["Width"] ?? 0
        let height = bounds["Height"] ?? 0
        guard width > 1, height > 1 else {
            return nil
        }
        let frame = CGRect(
            x: bounds["X"] ?? 0,
            y: bounds["Y"] ?? 0,
            width: width,
            height: height
        )
        return (BackgroundActivationWindow(id: CGWindowID(windowNumber), frame: frame), width * height)
    }
    if let point,
       let containingWindow = candidates.first(where: { $0.window.frame.contains(point) }) {
        return containingWindow.window
    }
    return candidates.max(by: { $0.area < $1.area })?.window
}

func backgroundQuartzWindowPoint(screenPoint: CGPoint, in window: BackgroundActivationWindow) -> CGPoint {
    CGPoint(
        x: screenPoint.x - window.frame.minX,
        y: screenPoint.y - window.frame.minY
    )
}

/// Background activation shim for semantic AX actions that need the target window
/// to behave as active without making it the user's frontmost application.
///
/// macOS does not expose a public API for this case, and closed SwiftUI `Menu`
/// controls do not publish their items in the AX tree. This follows the kwwk
/// recipe: route appKitDefined/window-local mouse events directly to the target
/// pid, and suppress focus messages sent to the previously frontmost app.
/// The unchecked Sendable conformance is scoped to this bridge because CGEvent
/// tap callbacks run on CFRunLoop threads; mutable phase state is guarded by
/// `lock`, and tap storage changes only during setup or finish.
final class BackgroundActivationSession: @unchecked Sendable {
    enum TapKind {
        case previous
        case target
    }

    private enum Phase {
        case deliveringToTarget
        case finished
    }

    final class TapContext {
        let session: BackgroundActivationSession
        let kind: TapKind

        init(session: BackgroundActivationSession, kind: TapKind) {
            self.session = session
            self.kind = kind
        }
    }

    // Focus messages do not have stable public CGEventType values across
    // AppKit/macOS releases. Match kwwk: subscribe broadly and filter narrowly
    // in shouldDrop(kind:type:).
    private static let focusEventMask = CGEventMask.max

    private let targetPID: pid_t
    private let previousPID: pid_t?
    private let lock = NSLock()
    private var phase = Phase.deliveringToTarget
    private var tapHolders = [TapHolder]()

    private init(targetPID: pid_t, previousPID: pid_t?) {
        self.targetPID = targetPID
        self.previousPID = previousPID
    }

    static func start(targetPID: pid_t) throws -> BackgroundActivationSession {
        let previousPID = NSWorkspace.shared.frontmostApplication?.processIdentifier
        let session = BackgroundActivationSession(
            targetPID: targetPID,
            previousPID: previousPID == targetPID ? nil : previousPID
        )
        do {
            try session.installTaps()
            return session
        } catch {
            session.finish()
            throw error
        }
    }

    func activateWindowWithoutRaise(window: BackgroundActivationWindow?) {
        guard let window, window.id != 0 else {
            return
        }
        let event = NSEvent.otherEvent(
            with: .appKitDefined,
            location: .zero,
            modifierFlags: [],
            timestamp: 0,
            windowNumber: Int(window.id),
            context: nil,
            subtype: Int16(1),
            data1: 0,
            data2: 0
        )?.cgEvent
        guard let event else {
            return
        }
        event.setWindowAddressingFields(windowID: window.id)
        event.postToPid(targetPID)
        usleep(20_000)
        postWindowCenterPrimer(window: window)
    }

    func restoreBackgroundActivationIfNeeded(window: BackgroundActivationWindow?) {
        guard let window, window.id != 0 else {
            return
        }
        guard NSWorkspace.shared.frontmostApplication?.processIdentifier != targetPID else {
            return
        }
        guard let event = NSEvent.otherEvent(
            with: .appKitDefined,
            location: .zero,
            modifierFlags: [],
            timestamp: 0,
            windowNumber: Int(window.id),
            context: nil,
            subtype: NSEvent.EventSubtype.applicationDeactivated.rawValue,
            data1: 0,
            data2: 0
        )?.cgEvent else {
            return
        }
        event.setWindowAddressingFields(windowID: window.id)
        event.postToPid(targetPID)
        usleep(20_000)
    }

    func finish() {
        let holders = lock.withLock { () -> [TapHolder] in
            guard phase != .finished else {
                return []
            }
            phase = .finished
            let holders = tapHolders
            tapHolders.removeAll()
            return holders
        }
        for holder in holders {
            holder.invalidate()
        }
    }

    deinit {
        finish()
    }

    func shouldDrop(kind: TapKind, type: CGEventType) -> Bool {
        guard isFocusMessage(type: type) else {
            return false
        }
        let active = lock.withLock { phase == .deliveringToTarget }
        guard active else {
            return false
        }
        return kind == .previous
    }

    private func installTaps() throws {
        guard let previousPID else {
            return
        }
        try installTap(kind: .previous, pid: previousPID)
        try installTap(kind: .target, pid: targetPID)
    }

    private func installTap(kind: TapKind, pid: pid_t) throws {
        let context = TapContext(session: self, kind: kind)
        let retainedContext = Unmanaged.passRetained(context)
        guard let tap = CGEvent.tapCreateForPid(
            pid: pid,
            place: .headInsertEventTap,
            options: .defaultTap,
            eventsOfInterest: Self.focusEventMask,
            callback: backgroundActivationTapCallback,
            userInfo: retainedContext.toOpaque()
        ) else {
            retainedContext.release()
            throw HelperFailure("failed to install background activation event tap for pid \(pid)")
        }
        let holder: TapHolder
        holder = try TapHolder(tap: tap, retainedContext: retainedContext)
        tapHolders.append(holder)
    }

    private func isFocusMessage(type: CGEventType) -> Bool {
        type.rawValue == 13 || type.rawValue == 19 || type.rawValue == 20
    }

    private func postWindowCenterPrimer(window: BackgroundActivationWindow) {
        guard window.frame.width > 0, window.frame.height > 0 else {
            return
        }
        let point = CGPoint(x: window.frame.midX, y: window.frame.midY)
        postMouse(
            .leftMouseDown,
            window: window,
            point: point,
            clickState: 1,
            pressure: 1
        )
        usleep(30_000)
        postMouse(
            .leftMouseUp,
            window: window,
            point: point,
            clickState: 1,
            pressure: 0
        )
        usleep(20_000)
    }

    private func postMouse(
        _ type: CGEventType,
        window: BackgroundActivationWindow,
        point: CGPoint,
        clickState: Int64,
        pressure: Double
    ) {
        guard let event = CGEvent(
            mouseEventSource: nil,
            mouseType: type,
            mouseCursorPosition: point,
            mouseButton: .left
        ) else {
            return
        }
        event.setIntegerValueField(.mouseEventClickState, value: clickState)
        event.setDoubleValueField(.mouseEventPressure, value: pressure)
        event.setIntegerValueField(.eventTargetUnixProcessID, value: Int64(targetPID))
        event.setIntegerValueField(.mouseEventWindowUnderMousePointer, value: Int64(window.id))
        event.setIntegerValueField(.mouseEventWindowUnderMousePointerThatCanHandleThisEvent, value: Int64(window.id))
        event.setWindowAddressingFields(windowID: window.id)
        _ = BackgroundWindowLocalEvent.setPoint(backgroundQuartzWindowPoint(screenPoint: point, in: window), on: event)
        event.postToPid(targetPID)
    }
}

private let backgroundActivationTapCallback: CGEventTapCallBack = { _, type, event, rawContext in
    guard let rawContext else {
        return Unmanaged.passUnretained(event)
    }
    let context = Unmanaged<BackgroundActivationSession.TapContext>
        .fromOpaque(rawContext)
        .takeUnretainedValue()
    if context.session.shouldDrop(kind: context.kind, type: type) {
        return nil
    }
    return Unmanaged.passUnretained(event)
}

private final class TapHolder {
    private let tap: CFMachPort
    private let source: CFRunLoopSource
    private var retainedContext: Unmanaged<BackgroundActivationSession.TapContext>?
    private var runLoop: CFRunLoop?
    private let ready = NSCondition()
    private var didStart = false
    private var didFinish = false
    private var invalidated = false
    private var thread: Thread?

    init(tap: CFMachPort, retainedContext: Unmanaged<BackgroundActivationSession.TapContext>) throws {
        self.tap = tap
        self.retainedContext = retainedContext
        guard let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0) else {
            CFMachPortInvalidate(tap)
            retainedContext.release()
            throw HelperFailure("failed to create background activation event tap run loop source")
        }
        self.source = source

        let thread = Thread { [weak self] in
            guard let self else {
                return
            }
            let currentRunLoop = CFRunLoopGetCurrent()
            ready.lock()
            runLoop = currentRunLoop
            didStart = true
            ready.signal()
            ready.unlock()

            CFRunLoopAddSource(currentRunLoop, source, .commonModes)
            CGEvent.tapEnable(tap: tap, enable: true)
            CFRunLoopRun()

            ready.lock()
            didFinish = true
            ready.broadcast()
            ready.unlock()
        }
        thread.name = "orbi-background-activation"
        self.thread = thread
        thread.start()

        ready.lock()
        let deadline = Date().addingTimeInterval(1.0)
        while !didStart && Date() < deadline {
            ready.wait(until: deadline)
        }
        let started = didStart
        ready.unlock()
        if !started {
            invalidate()
            self.retainedContext?.release()
            self.retainedContext = nil
            throw HelperFailure("timed out starting background activation event tap")
        }
    }

    func invalidate() {
        ready.lock()
        if invalidated {
            ready.unlock()
            return
        }
        invalidated = true
        let tapThread = thread
        ready.unlock()

        CGEvent.tapEnable(tap: tap, enable: false)
        CFMachPortInvalidate(tap)
        if let runLoop {
            CFRunLoopRemoveSource(runLoop, source, .commonModes)
            CFRunLoopStop(runLoop)
        }

        var finished = false
        if tapThread !== Thread.current {
            ready.lock()
            let deadline = Date().addingTimeInterval(1.0)
            while !didFinish && Date() < deadline {
                ready.wait(until: deadline)
            }
            finished = didFinish
            ready.unlock()
        }

        ready.lock()
        thread = nil
        ready.unlock()

        // If the tap run loop did not confirm exit, keep the context alive to avoid a late callback
        // dereferencing freed memory.
        if finished || tapThread === Thread.current {
            retainedContext?.release()
            retainedContext = nil
        }
    }

    deinit {
        invalidate()
    }
}

extension CGEvent {
    private static let targetWindowNumberField = CGEventField(rawValue: 51)
    private static let privateWindowRoutingField = CGEventField(rawValue: 58)

    func setWindowAddressingFields(windowID: CGWindowID) {
        if let targetWindowNumberField = Self.targetWindowNumberField {
            setIntegerValueField(targetWindowNumberField, value: Int64(windowID))
        }
        if let privateWindowRoutingField = Self.privateWindowRoutingField {
            setIntegerValueField(privateWindowRoutingField, value: 1)
        }
    }
}

enum BackgroundWindowLocalEvent {
    private typealias SetWindowLocationFn = @convention(c) (CGEvent, CGPoint) -> Void

    private static let setWindowLocation: SetWindowLocationFn? = {
        _ = dlopen("/System/Library/PrivateFrameworks/SkyLight.framework/SkyLight", RTLD_LAZY)
        guard let symbol = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "CGEventSetWindowLocation") else {
            return nil
        }
        return unsafeBitCast(symbol, to: SetWindowLocationFn.self)
    }()

    @discardableResult
    static func setPoint(_ point: CGPoint, on event: CGEvent) -> Bool {
        guard let setWindowLocation else {
            return false
        }
        setWindowLocation(event, point)
        return true
    }
}

private extension NSLock {
    func withLock<T>(_ body: () throws -> T) rethrows -> T {
        lock()
        defer { unlock() }
        return try body()
    }
}
