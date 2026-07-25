import AppKit
import ApplicationServices
import Foundation

@MainActor
final class MacosAutomation {
    private var videoRecorder: Any?

    func handle(_ request: HelperRequest) async throws -> Any? {
        switch request.command {
        case "checkPermissions":
            return [
                "backendAvailable": true,
                "accessibilityTrusted": AXIsProcessTrusted(),
                "screenCaptureAccess": await screenRecordingAvailable(),
            ]
        case "launchApp":
            try await launchApp(request.params)
            return [:]
        case "waitForApp":
            try await waitForApp(request.params)
            return [:]
        case "stopApp":
            try await stopApp(
                bundleID: requiredString(request.params, "bundleId"),
                force: optionalBool(request.params, "force") ?? false
            )
            return [:]
        case "clearAppState":
            try await clearAppState(request.params)
            return [:]
        case "focus":
            try focus(bundleID: requiredString(request.params, "bundleId"))
            return [:]
        case "describeAll":
            return try describeAll(bundleID: requiredString(request.params, "bundleId"))
        case "describePoint":
            return try describePoint(request.params)
        case "activateSelector":
            return try activateSelector(request.params)
        case "tapPoint":
            try await tapPoint(request.params)
            return [:]
        case "hoverPoint":
            try await hoverPoint(request.params)
            return [:]
        case "rightClickPoint":
            try await rightClickPoint(request.params)
            return [:]
        case "swipe":
            try await swipe(request.params)
            return [:]
        case "drag":
            try await drag(request.params)
            return [:]
        case "inputText":
            try inputText(request.params)
            return [:]
        case "pressKey":
            try pressKey(request.params)
            return [:]
        case "pressKeyCode":
            try pressKeyCode(request.params)
            return [:]
        case "pressKeySequence":
            try pressKeySequence(request.params)
            return [:]
        case "selectMenuItem":
            try selectMenuItem(request.params)
            return [:]
        case "scroll":
            try await scroll(request.params)
            return [:]
        case "takeScreenshot":
            let pid = try runningPid(bundleID: requiredString(request.params, "bundleId"))
            try await captureWindowScreenshot(pid: pid, outputPath: requiredString(request.params, "path"))
            return [:]
        case "startVideoRecording":
            try await startVideoRecording(request.params)
            return [:]
        case "stopVideoRecording":
            try await stopVideoRecording()
            return [:]
        default:
            throw HelperFailure("unsupported helper command `\(request.command)`")
        }
    }

    private func launchApp(_ params: [String: Any]) async throws {
        let bundleID = try requiredString(params, "bundleId")
        let bundlePath = try requiredString(params, "bundlePath")
        if try requiredBool(params, "stopApp") {
            try await stopApp(bundleID: bundleID)
        }

        let configuration = NSWorkspace.OpenConfiguration()
        configuration.activates = false
        configuration.addsToRecentItems = false
        configuration.arguments = launchArguments(params)
        configuration.environment = launchEnvironment(params)

        let app = try await withCheckedThrowingContinuation { continuation in
            NSWorkspace.shared.openApplication(at: URL(fileURLWithPath: bundlePath), configuration: configuration) { app, error in
                if let app {
                    continuation.resume(returning: app)
                } else {
                    continuation.resume(throwing: error ?? HelperFailure("failed to launch `\(bundleID)`"))
                }
            }
        }
        _ = try await waitForRunningApp(bundleID: bundleID, expectedPid: app.processIdentifier)
        try await waitForAccessibleWindow(bundleID: bundleID, pid: app.processIdentifier)
    }

    private func waitForApp(_ params: [String: Any]) async throws {
        let bundleID = try requiredString(params, "bundleId")
        let app = try await waitForRunningApp(bundleID: bundleID, expectedPid: 0)
        try await waitForAccessibleWindow(bundleID: bundleID, pid: app.processIdentifier)
    }

    private func stopApp(bundleID: String, force: Bool = false) async throws {
        let apps = runningApps(bundleID: bundleID)
        if force {
            for app in apps {
                app.forceTerminate()
            }
            try await waitForAppToStop(bundleID: bundleID, attempts: 10)
            return
        }
        for app in apps {
            app.terminate()
        }
        try await waitForAppToStop(bundleID: bundleID, attempts: 40)
        for app in runningApps(bundleID: bundleID) {
            app.forceTerminate()
        }
    }

    private func waitForAppToStop(bundleID: String, attempts: Int) async throws {
        for _ in 0..<attempts {
            if runningApps(bundleID: bundleID).isEmpty {
                return
            }
            try await Task.sleep(nanoseconds: 100_000_000)
        }
    }

    private func startVideoRecording(_ params: [String: Any]) async throws {
        guard videoRecorder == nil else {
            throw HelperFailure("video recording is already active")
        }
        guard #available(macOS 15.0, *) else {
            throw HelperFailure("macOS video recording requires macOS 15.0 or newer")
        }
        let pid = try runningPid(bundleID: requiredString(params, "bundleId"))
        let target = try await targetScreenCaptureWindow(pid: pid)
        let recorder = try WindowVideoRecorder(target: target, outputPath: requiredString(params, "path"))
        videoRecorder = recorder
        do {
            try await recorder.start()
        } catch {
            videoRecorder = nil
            throw error
        }
    }

    private func stopVideoRecording() async throws {
        guard let recorder = videoRecorder else {
            return
        }
        videoRecorder = nil
        if #available(macOS 15.0, *), let recorder = recorder as? WindowVideoRecorder {
            try await recorder.stop()
        }
    }

    private func clearAppState(_ params: [String: Any]) async throws {
        let bundleID = try requiredString(params, "bundleId")
        try await stopApp(bundleID: bundleID)
        let home = FileManager.default.homeDirectoryForCurrentUser
        clearPreferences(bundleID: bundleID)
        let paths = [
            home.appendingPathComponent("Library/Containers/\(bundleID)"),
            home.appendingPathComponent("Library/Application Support/\(bundleID)"),
            home.appendingPathComponent("Library/Caches/\(bundleID)"),
            home.appendingPathComponent("Library/Saved Application State/\(bundleID).savedState"),
            home.appendingPathComponent("Library/Preferences/\(bundleID).plist"),
        ]
        for path in paths {
            // These per-bundle state paths are optional; a fresh app often has only a subset.
            try? FileManager.default.removeItem(at: path)
        }
        removeByHostPreferences(bundleID: bundleID, home: home)
        clearPreferences(bundleID: bundleID)
    }

    private func focus(bundleID: String) throws {
        guard let app = runningApps(bundleID: bundleID).first else {
            throw HelperFailure("application `\(bundleID)` is not running")
        }
        app.activate()
    }

    private func describeAll(bundleID: String) throws -> Any {
        let pid = try runningPid(bundleID: bundleID)
        let app = axApplication(pid: pid)
        var visited = Set<CFHashCode>()
        var budget = 8_000
        return snapshot(app, visited: &visited, budget: &budget)
    }

    private func describePoint(_ params: [String: Any]) throws -> Any {
        let pid = try runningPid(bundleID: requiredString(params, "bundleId"))
        let app = axApplication(pid: pid)
        guard let element = elementAtPoint(
            app: app,
            x: try requiredDouble(params, "x"),
            y: try requiredDouble(params, "y")
        ) else {
            throw HelperFailure("could not resolve an accessibility element at the requested point")
        }
        var visited = Set<CFHashCode>()
        var budget = 500
        return snapshot(element, visited: &visited, budget: &budget)
    }

    private func activateSelector(_ params: [String: Any]) throws -> Bool {
        let pid = try runningPid(bundleID: requiredString(params, "bundleId"))
        let selector = UiSelector(optionalDictionary(params, "selector") ?? [:])
        guard let candidate = bestElement(in: roots(pid: pid), matching: selector, targetPid: pid) else {
            return false
        }
        if boolAttribute(candidate.element, kAXEnabledAttribute as String) == false {
            throw HelperFailure("matched `\(candidate.label)`, but it is disabled")
        }
        if isTextInput(candidate.element) {
            return focusTextInput(candidate.element, targetPid: pid)
        }
        if try performPreferredAction(candidate.element, preferred: [
            kAXPressAction as String,
            kAXPickAction as String,
            kAXConfirmAction as String,
        ], targetPid: pid) {
            return true
        }
        return try selectContainingTableRow(candidate.element, targetPid: pid)
    }

    private func isTextInput(_ element: AXUIElement) -> Bool {
        guard let role = stringAttribute(element, kAXRoleAttribute as String) else {
            return false
        }
        return role == kAXTextFieldRole as String ||
            role == kAXTextAreaRole as String ||
            role == kAXComboBoxRole as String
    }

    private func focusTextInput(_ element: AXUIElement, targetPid: pid_t) -> Bool {
        guard pid(of: element) == targetPid else {
            return false
        }
        guard isAttributeSettable(element, kAXFocusedAttribute as String) else {
            return false
        }
        return AXUIElementSetAttributeValue(element, kAXFocusedAttribute as CFString, kCFBooleanTrue) == .success
    }

    private func tapPoint(_ params: [String: Any]) async throws {
        let pid = try runningPid(bundleID: requiredString(params, "bundleId"))
        let point = CGPoint(x: try requiredDouble(params, "x"), y: try requiredDouble(params, "y"))
        let window = backgroundActivationWindow(pid: pid, point: point)
        try performWithBackgroundInput(pid: pid, window: window) {
            try postClick(
                pid: pid,
                point: point,
                durationMs: optionalInt(params, "durationMs"),
                window: window
            )
        }
    }

    private func hoverPoint(_ params: [String: Any]) async throws {
        let pid = try runningPid(bundleID: requiredString(params, "bundleId"))
        let point = CGPoint(x: try requiredDouble(params, "x"), y: try requiredDouble(params, "y"))
        if let element = elementAtPoint(app: axApplication(pid: pid), x: Double(point.x), y: Double(point.y)),
           try performPreferredAction(element, preferred: [
               kAXShowDefaultUIAction as String,
               kAXShowAlternateUIAction as String,
           ], targetPid: pid) {
            return
        }
        let window = backgroundActivationWindow(pid: pid, point: point)
        let sendHover = {
            if let element = elementAtPoint(app: axApplication(pid: pid), x: Double(point.x), y: Double(point.y)),
               let rect = frame(of: element) {
                let outside = CGPoint(x: max(0, rect.minX - 6), y: rect.midY)
                try postMove(pid: pid, point: outside, window: window)
                usleep(60_000)
            }
            try postMove(pid: pid, point: point, window: window)
        }
        try performWithBackgroundInput(pid: pid, window: window) {
            try sendHover()
        }
    }

    private func rightClickPoint(_ params: [String: Any]) async throws {
        let pid = try runningPid(bundleID: requiredString(params, "bundleId"))
        let point = CGPoint(x: try requiredDouble(params, "x"), y: try requiredDouble(params, "y"))
        if let element = elementAtPoint(app: axApplication(pid: pid), x: Double(point.x), y: Double(point.y)),
           try performPreferredAction(element, preferred: [kAXShowMenuAction as String], targetPid: pid) {
            return
        }
        let window = backgroundActivationWindow(pid: pid, point: point)
        try performWithBackgroundInput(pid: pid, window: window) {
            try postClick(pid: pid, point: point, button: .right, window: window)
        }
    }

    private func swipe(_ params: [String: Any]) async throws {
        let pid = try runningPid(bundleID: requiredString(params, "bundleId"))
        let start = CGPoint(x: try requiredDouble(params, "startX"), y: try requiredDouble(params, "startY"))
        let end = CGPoint(x: try requiredDouble(params, "endX"), y: try requiredDouble(params, "endY"))
        let window = backgroundActivationWindow(pid: pid, point: start)
        let sendDrag = {
            try postDrag(
                pid: pid,
                start: start,
                end: end,
                steps: optionalInt(params, "delta") ?? 8,
                durationMs: optionalInt(params, "durationMs") ?? 500,
                window: window
            )
        }
        try performWithBackgroundInput(pid: pid, window: window) {
            try sendDrag()
        }
    }

    private func drag(_ params: [String: Any]) async throws {
        let pid = try runningPid(bundleID: requiredString(params, "bundleId"))
        let start = CGPoint(x: try requiredDouble(params, "startX"), y: try requiredDouble(params, "startY"))
        let end = CGPoint(x: try requiredDouble(params, "endX"), y: try requiredDouble(params, "endY"))
        let app = axApplication(pid: pid)
        if let source = elementAtPoint(app: app, x: Double(start.x), y: Double(start.y)),
           let destination = elementAtPoint(app: app, x: Double(end.x), y: Double(end.y)),
           try performPreferredAction(source, preferred: [kAXPickAction as String], targetPid: pid),
           try performPreferredAction(destination, preferred: [kAXConfirmAction as String], targetPid: pid) {
            return
        }
        try await swipe(params)
    }

    private func inputText(_ params: [String: Any]) throws {
        let pid = try runningPid(bundleID: requiredString(params, "bundleId"))
        let text = try requiredString(params, "text")
        let app = axApplication(pid: pid)
        if let focusedValue = attribute(app, kAXFocusedUIElementAttribute as String),
           let focused = axElement(from: focusedValue) {
            if isAttributeSettable(focused, kAXSelectedTextAttribute as String),
               AXUIElementSetAttributeValue(focused, kAXSelectedTextAttribute as CFString, text as CFString) == .success {
                return
            }
            if isAttributeSettable(focused, kAXValueAttribute as String),
               AXUIElementSetAttributeValue(focused, kAXValueAttribute as CFString, text as CFString) == .success {
                return
            }
        }
        postText(pid: pid, text: text)
    }

    private func pressKey(_ params: [String: Any]) throws {
        let pid = try runningPid(bundleID: requiredString(params, "bundleId"))
        let key = optionalDictionary(params, "key") ?? [:]
        let modifiers = params["modifiers"] as? [String] ?? []
        if (key["kind"] as? String) == "Character", modifiers.isEmpty {
            postText(pid: pid, text: try requiredString(key, "value"))
            return
        }
        try postKeyCode(pid: pid, keyCode: keyCode(for: key), modifiers: modifiers)
    }

    private func pressKeyCode(_ params: [String: Any]) throws {
        let pid = try runningPid(bundleID: requiredString(params, "bundleId"))
        try postKeyCode(
            pid: pid,
            keyCode: CGKeyCode(try requiredDouble(params, "keyCode")),
            modifiers: params["modifiers"] as? [String] ?? []
        )
    }

    private func pressKeySequence(_ params: [String: Any]) throws {
        let pid = try runningPid(bundleID: requiredString(params, "bundleId"))
        guard let keyCodes = params["keyCodes"] as? [Int] else {
            throw HelperFailure("missing numeric array `keyCodes`")
        }
        for keyCode in keyCodes {
            try postKeyCode(pid: pid, keyCode: CGKeyCode(keyCode), modifiers: [])
        }
    }

    private func selectMenuItem(_ params: [String: Any]) throws {
        let pid = try runningPid(bundleID: requiredString(params, "bundleId"))
        let path = try requiredStringArray(params, "path")
        guard !path.isEmpty else {
            throw HelperFailure("selectMenuItem path must not be empty")
        }
        let app = axApplication(pid: pid)
        if let source = optionalDictionary(params, "source") {
            let selector = UiSelector(source)
            guard let candidate = bestElement(in: roots(pid: pid), matching: selector, targetPid: pid) else {
                throw HelperFailure("could not find menu source matching \(selector.summary)")
            }
            if boolAttribute(candidate.element, kAXEnabledAttribute as String) == false {
                throw HelperFailure("matched menu source `\(candidate.label)`, but it is disabled")
            }
            if try performPreferredAction(candidate.element, preferred: menuActionNames(path), targetPid: pid) {
                return
            }
            let backgroundActivation = try BackgroundActivationSession.start(targetPID: pid)
            let focusWindow = backgroundActivationWindow(pid: pid)
            backgroundActivation.activateWindowWithoutRaise(window: focusWindow)
            defer {
                backgroundActivation.restoreBackgroundActivationIfNeeded(window: focusWindow)
                backgroundActivation.finish()
            }
            _ = try performPreferredAction(candidate.element, preferred: [
                kAXPressAction as String,
                kAXPickAction as String,
            ], targetPid: pid)
            Thread.sleep(forTimeInterval: 0.12)
            if try selectMenuPath(path, from: [candidate.element, app], targetPid: pid) {
                return
            }
        }
        guard let menuBarValue = attribute(app, kAXMenuBarAttribute as String),
              let menuBar = axElement(from: menuBarValue) else {
            throw HelperFailure("application does not expose an accessibility menu bar")
        }
        if try selectMenuPath(path, from: [menuBar], targetPid: pid) {
            return
        }
        throw HelperFailure("could not find menu item path `\(path.joined(separator: " > "))`")
    }

    private func scroll(_ params: [String: Any]) async throws {
        let pid = try runningPid(bundleID: requiredString(params, "bundleId"))
        let direction = try requiredString(params, "direction")
        let point: CGPoint? = if params["x"] != nil || params["y"] != nil {
            CGPoint(x: try requiredDouble(params, "x"), y: try requiredDouble(params, "y"))
        } else {
            nil
        }
        let actions = scrollActions(direction)
        if let point,
           let element = elementAtPoint(app: axApplication(pid: pid), x: Double(point.x), y: Double(point.y)),
           let container = firstScrollableAncestor(of: element, targetPid: pid, matching: actions),
           try scrollWithScrollbar(in: container, direction: direction, targetPid: pid) {
            return
        }
        if let point,
           let element = elementAtPoint(app: axApplication(pid: pid), x: Double(point.x), y: Double(point.y)),
           try performPreferredAction(element, preferred: actions, targetPid: pid) {
            return
        }
        if let container = firstScrollableElement(pid: pid, matching: actions),
           try scrollWithScrollbar(in: container, direction: direction, targetPid: pid) {
            return
        }
        if let container = firstScrollableElement(pid: pid, matching: actions),
           try performPreferredAction(container, preferred: actions, targetPid: pid) {
            return
        }
        let delta = scrollDelta(direction)
        let focusWindow = backgroundActivationWindow(pid: pid, point: point)
        try performWithBackgroundInput(pid: pid, window: focusWindow) {
            try postScroll(
                pid: pid,
                deltaX: delta.0,
                deltaY: delta.1,
                point: point,
                window: focusWindow
            )
        }
    }

    private func performWithBackgroundInput<T>(
        pid: pid_t,
        window: BackgroundActivationWindow?,
        _ body: () throws -> T
    ) throws -> T {
        // Mirror kwwk's focus-message suppression for private routed input,
        // but skip its activation primer here: the primer is a real click and
        // can hit unrelated controls in a generic fixture/application window.
        let backgroundActivation = try BackgroundActivationSession.start(targetPID: pid)
        defer {
            backgroundActivation.restoreBackgroundActivationIfNeeded(window: window)
            backgroundActivation.finish()
        }
        return try body()
    }

    private func performPreferredAction(_ element: AXUIElement, preferred: [String], targetPid: pid_t) throws -> Bool {
        var current: AXUIElement? = element
        var depth = 0
        while let candidate = current, depth < 8 {
            if pid(of: candidate) == targetPid {
                let actions = elementActions(candidate)
                for action in preferred {
                    guard let match = actions.first(where: { $0.raw == action || $0.name == action }) else {
                        continue
                    }
                    let status = AXUIElementPerformAction(candidate, match.raw as CFString)
                    if status == .success {
                        return true
                    }
                }
            }
            current = parent(of: candidate)
            depth += 1
        }
        return false
    }

    private func selectContainingTableRow(_ element: AXUIElement, targetPid: pid_t) throws -> Bool {
        var current: AXUIElement? = element
        var depth = 0
        while let candidate = current, depth < 8 {
            if pid(of: candidate) == targetPid,
               stringAttribute(candidate, kAXRoleAttribute as String) == kAXRowRole as String {
                if isAttributeSettable(candidate, kAXSelectedAttribute as String),
                   AXUIElementSetAttributeValue(candidate, kAXSelectedAttribute as CFString, kCFBooleanTrue) == .success {
                    return true
                }
                if let table = firstAncestor(of: candidate, role: kAXTableRole as String, targetPid: targetPid),
                   isAttributeSettable(table, kAXSelectedRowsAttribute as String) {
                    let rows = [candidate] as CFArray
                    if AXUIElementSetAttributeValue(table, kAXSelectedRowsAttribute as CFString, rows) == .success {
                        return true
                    }
                }
            }
            current = parent(of: candidate)
            depth += 1
        }
        return false
    }

    private func selectMenuPath(_ path: [String], from roots: [AXUIElement], targetPid: pid_t) throws -> Bool {
        for root in roots {
            var current = root
            var matchedPath = true
            for (index, title) in path.enumerated() {
                guard let next = findDescendant(in: current, title: title, targetPid: targetPid) else {
                    matchedPath = false
                    break
                }
                _ = try performPreferredAction(next, preferred: [
                    kAXPickAction as String,
                    kAXPressAction as String,
                ], targetPid: targetPid)
                current = next
                if index + 1 < path.count {
                    Thread.sleep(forTimeInterval: 0.08)
                }
            }
            if matchedPath {
                return true
            }
        }
        return false
    }

    private func findDescendant(in root: AXUIElement, title: String, targetPid: pid_t) -> AXUIElement? {
        var stack = childElements(root)
        var visited = Set<CFHashCode>()
        var budget = 2_000
        while let element = stack.popLast(), budget > 0 {
            budget -= 1
            let key = CFHash(element)
            if visited.contains(key) {
                continue
            }
            visited.insert(key)
            if pid(of: element) == targetPid, let label = label(for: element), matchMenuTitle(label, title) {
                return element
            }
            stack.append(contentsOf: childElements(element))
        }
        return nil
    }

    private func firstScrollableAncestor(of element: AXUIElement, targetPid: pid_t, matching preferredActions: [String]) -> AXUIElement? {
        var current: AXUIElement? = element
        var depth = 0
        while let candidate = current, depth < 8 {
            if pid(of: candidate) == targetPid,
               isScrollable(candidate, matching: preferredActions) {
                return candidate
            }
            current = parent(of: candidate)
            depth += 1
        }
        return nil
    }

    private func firstScrollableElement(pid: pid_t, matching preferredActions: [String]) -> AXUIElement? {
        var stack = roots(pid: pid)
        var visited = Set<CFHashCode>()
        var budget = 4_000
        while let element = stack.popLast(), budget > 0 {
            budget -= 1
            let key = CFHash(element)
            if visited.contains(key) {
                continue
            }
            visited.insert(key)
            if isScrollable(element, matching: preferredActions) {
                return element
            }
            stack.append(contentsOf: childElements(element))
        }
        return nil
    }

    private func isScrollable(_ element: AXUIElement, matching preferredActions: [String]) -> Bool {
        !Set(actionNames(element)).isDisjoint(with: Set(preferredActions))
    }

    private func scrollWithScrollbar(in container: AXUIElement, direction: String, targetPid: pid_t) throws -> Bool {
        let vertical = direction == "up" || direction == "down"
        guard let scrollbar = findScrollbar(in: container, vertical: vertical, targetPid: targetPid) else {
            return false
        }
        guard isAttributeSettable(scrollbar, kAXValueAttribute as String) else {
            return false
        }
        let minimum = numberAttribute(scrollbar, kAXMinValueAttribute as String) ?? 0
        let maximum = numberAttribute(scrollbar, kAXMaxValueAttribute as String) ?? 1
        let current = numberAttribute(scrollbar, kAXValueAttribute as String) ?? minimum
        let span = max(0.1, maximum - minimum)
        let step = max(0.1, span * 0.45)
        let next = switch direction {
        case "up", "left":
            max(minimum, current - step)
        case "down", "right":
            min(maximum, current + step)
        default:
            min(maximum, current + step)
        }
        let status = AXUIElementSetAttributeValue(scrollbar, kAXValueAttribute as CFString, NSNumber(value: next))
        return status == .success
    }

    private func findScrollbar(in root: AXUIElement, vertical: Bool, targetPid: pid_t) -> AXUIElement? {
        var stack = childElements(root)
        var visited = Set<CFHashCode>()
        var budget = 1_000
        while let element = stack.popLast(), budget > 0 {
            budget -= 1
            let key = CFHash(element)
            if visited.contains(key) {
                continue
            }
            visited.insert(key)
            if pid(of: element) == targetPid,
               stringAttribute(element, kAXRoleAttribute as String) == kAXScrollBarRole as String,
               scrollbarMatches(element, vertical: vertical) {
                return element
            }
            stack.append(contentsOf: childElements(element))
        }
        return nil
    }

    private func scrollbarMatches(_ element: AXUIElement, vertical: Bool) -> Bool {
        if let orientation = stringAttribute(element, kAXOrientationAttribute as String) {
            return vertical
                ? orientation == kAXVerticalOrientationValue as String
                : orientation == kAXHorizontalOrientationValue as String
        }
        guard let rect = frame(of: element) else {
            return true
        }
        return vertical ? rect.height >= rect.width : rect.width >= rect.height
    }

    private func firstAncestor(of element: AXUIElement, role: String, targetPid: pid_t) -> AXUIElement? {
        var current = parent(of: element)
        var depth = 0
        while let candidate = current, depth < 8 {
            if pid(of: candidate) == targetPid,
               stringAttribute(candidate, kAXRoleAttribute as String) == role {
                return candidate
            }
            current = parent(of: candidate)
            depth += 1
        }
        return nil
    }

    private func numberAttribute(_ element: AXUIElement, _ name: String) -> Double? {
        guard let value = attribute(element, name) else {
            return nil
        }
        return (value as? NSNumber)?.doubleValue
    }

    private func roots(pid: pid_t) -> [AXUIElement] {
        [axApplication(pid: pid)]
    }

    private func runningPid(bundleID: String) throws -> pid_t {
        guard let app = runningApps(bundleID: bundleID).first else {
            throw HelperFailure("application `\(bundleID)` is not running")
        }
        return app.processIdentifier
    }

    private func runningApps(bundleID: String) -> [NSRunningApplication] {
        NSRunningApplication.runningApplications(withBundleIdentifier: bundleID)
            .filter { !$0.isTerminated }
    }

    private func waitForRunningApp(bundleID: String, expectedPid: pid_t) async throws -> NSRunningApplication {
        for _ in 0..<50 {
            if let app = runningApps(bundleID: bundleID).first(where: { $0.processIdentifier == expectedPid }) ?? runningApps(bundleID: bundleID).first {
                return app
            }
            try await Task.sleep(nanoseconds: 100_000_000)
        }
        throw HelperFailure("application `\(bundleID)` did not start")
    }

    private func waitForAccessibleWindow(bundleID: String, pid: pid_t) async throws {
        let app = axApplication(pid: pid)
        for _ in 0..<80 {
            if let windows = attribute(app, kAXWindowsAttribute as String) as? [Any],
               !windows.isEmpty {
                return
            }
            try await Task.sleep(nanoseconds: 100_000_000)
        }
        throw HelperFailure("application `\(bundleID)` did not expose an accessibility window")
    }

    private func launchArguments(_ params: [String: Any]) -> [String] {
        guard let raw = params["arguments"] as? [[String: Any]] else {
            return []
        }
        return raw.flatMap { entry -> [String] in
            guard let key = entry["key"] as? String, let value = entry["value"] as? String else {
                return []
            }
            return ["-\(key)", value]
        }
    }

    private func launchEnvironment(_ params: [String: Any]) -> [String: String] {
        guard let raw = params["environment"] as? [[String: Any]] else {
            return [:]
        }
        var environment: [String: String] = [:]
        for entry in raw {
            guard let key = entry["key"] as? String, !key.isEmpty,
                  let value = entry["value"] as? String else {
                continue
            }
            environment[key] = value
        }
        return environment
    }
}

private func clearPreferences(bundleID: String) {
    let appID = bundleID as CFString
    if let keys = CFPreferencesCopyKeyList(appID, kCFPreferencesCurrentUser, kCFPreferencesAnyHost) {
        // cfprefsd can keep AppStorage/UserDefaults values alive after deleting the plist file.
        CFPreferencesSetMultiple(nil, keys, appID, kCFPreferencesCurrentUser, kCFPreferencesAnyHost)
    }
    _ = CFPreferencesSynchronize(appID, kCFPreferencesCurrentUser, kCFPreferencesAnyHost)
    _ = CFPreferencesAppSynchronize(appID)
    UserDefaults.standard.removePersistentDomain(forName: bundleID)
    _ = UserDefaults.standard.synchronize()
}

private func removeByHostPreferences(bundleID: String, home: URL) {
    let byHost = home.appendingPathComponent("Library/Preferences/ByHost")
    guard let urls = try? FileManager.default.contentsOfDirectory(at: byHost, includingPropertiesForKeys: nil) else {
        return
    }
    for url in urls where url.lastPathComponent.hasPrefix("\(bundleID).") && url.pathExtension == "plist" {
        try? FileManager.default.removeItem(at: url)
    }
}

private func matchMenuTitle(_ value: String, _ needle: String) -> Bool {
    value == needle || value.caseInsensitiveCompare(needle) == .orderedSame
}

private func menuActionNames(_ path: [String]) -> [String] {
    var names = [String]()
    let joined = path.joined(separator: " > ")
    if !joined.isEmpty {
        names.append(joined)
    }
    if let last = path.last, last != joined {
        names.append(last)
    }
    return names
}

private func scrollActions(_ direction: String) -> [String] {
    switch direction {
    case "up":
        return ["AXScrollUp", "AXScrollUpByPage"]
    case "down":
        return ["AXScrollDown", "AXScrollDownByPage"]
    case "left":
        return ["AXScrollLeft", "AXScrollLeftByPage"]
    case "right":
        return ["AXScrollRight", "AXScrollRightByPage"]
    default:
        return ["AXScrollDown", "AXScrollDownByPage"]
    }
}

private func scrollDelta(_ direction: String) -> (Int32, Int32) {
    switch direction {
    case "up":
        return (0, 5)
    case "down":
        return (0, -5)
    case "left":
        return (5, 0)
    case "right":
        return (-5, 0)
    default:
        return (0, -5)
    }
}
