import CoreGraphics
import Foundation

func postClick(
    pid: pid_t,
    point: CGPoint,
    button: CGMouseButton = .left,
    clickCount: Int64 = 1,
    durationMs: Int? = nil,
    window: BackgroundActivationWindow? = nil
) throws {
    let targetWindow = window ?? backgroundActivationWindow(pid: pid, point: point)
    let downType: CGEventType = button == .right ? .rightMouseDown : .leftMouseDown
    let upType: CGEventType = button == .right ? .rightMouseUp : .leftMouseUp
    try postMove(pid: pid, point: point, window: targetWindow)
    guard let down = CGEvent(
        mouseEventSource: nil,
        mouseType: downType,
        mouseCursorPosition: point,
        mouseButton: button
    ),
          let up = CGEvent(
            mouseEventSource: nil,
            mouseType: upType,
            mouseCursorPosition: point,
            mouseButton: button
          )
    else {
        throw HelperFailure("failed to create mouse click event")
    }
    down.setIntegerValueField(.mouseEventClickState, value: clickCount)
    up.setIntegerValueField(.mouseEventClickState, value: clickCount)
    down.setIntegerValueField(.mouseEventButtonNumber, value: buttonNumber(for: button))
    up.setIntegerValueField(.mouseEventButtonNumber, value: buttonNumber(for: button))
    stampMouseEvent(down, pid: pid, point: point, window: targetWindow, pressure: 1)
    stampMouseEvent(up, pid: pid, point: point, window: targetWindow, pressure: 0)
    postPidEvent(down, to: pid)
    usleep(clickDurationMicros(durationMs))
    postPidEvent(up, to: pid)
}

func postMove(pid: pid_t, point: CGPoint, window: BackgroundActivationWindow? = nil) throws {
    let targetWindow = window ?? backgroundActivationWindow(pid: pid, point: point)
    guard let move = CGEvent(
        mouseEventSource: nil,
        mouseType: .mouseMoved,
        mouseCursorPosition: point,
        mouseButton: .left
    ) else {
        throw HelperFailure("failed to create mouse move event")
    }
    stampMouseEvent(move, pid: pid, point: point, window: targetWindow, pressure: 0)
    postPidEvent(move, to: pid)
}

func postDrag(
    pid: pid_t,
    start: CGPoint,
    end: CGPoint,
    steps: Int,
    durationMs: Int,
    window: BackgroundActivationWindow? = nil
) throws {
    let count = max(1, min(200, steps))
    let delay = useconds_t((max(0, min(10_000, durationMs)) * 1_000) / count)
    let targetWindow = window ?? backgroundActivationWindow(pid: pid, point: start)
    try postMove(pid: pid, point: start, window: targetWindow)
    guard let down = CGEvent(
        mouseEventSource: nil,
        mouseType: .leftMouseDown,
        mouseCursorPosition: start,
        mouseButton: .left
    ) else {
        throw HelperFailure("failed to create drag mouseDown event")
    }
    down.setIntegerValueField(.mouseEventClickState, value: 1)
    down.setIntegerValueField(.mouseEventButtonNumber, value: buttonNumber(for: .left))
    stampMouseEvent(down, pid: pid, point: start, window: targetWindow, pressure: 1)
    postPidEvent(down, to: pid)
    usleep(12_000)
    for index in 1...count {
        let progress = CGFloat(index) / CGFloat(count)
        let point = CGPoint(
            x: start.x + ((end.x - start.x) * progress),
            y: start.y + ((end.y - start.y) * progress)
        )
        guard let drag = CGEvent(
            mouseEventSource: nil,
            mouseType: .leftMouseDragged,
            mouseCursorPosition: point,
            mouseButton: .left
        ) else {
            throw HelperFailure("failed to create drag mouseDragged event")
        }
        drag.setIntegerValueField(.mouseEventClickState, value: 1)
        drag.setIntegerValueField(.mouseEventButtonNumber, value: buttonNumber(for: .left))
        stampMouseEvent(drag, pid: pid, point: point, window: targetWindow, pressure: 1)
        usleep(delay)
        postPidEvent(drag, to: pid)
    }
    guard let up = CGEvent(
        mouseEventSource: nil,
        mouseType: .leftMouseUp,
        mouseCursorPosition: end,
        mouseButton: .left
    ) else {
        throw HelperFailure("failed to create drag mouseUp event")
    }
    up.setIntegerValueField(.mouseEventClickState, value: 1)
    up.setIntegerValueField(.mouseEventButtonNumber, value: buttonNumber(for: .left))
    stampMouseEvent(up, pid: pid, point: end, window: targetWindow, pressure: 0)
    usleep(delay)
    postPidEvent(up, to: pid)
}

func postScroll(
    pid: pid_t,
    deltaX: Int32,
    deltaY: Int32,
    point: CGPoint? = nil,
    window: BackgroundActivationWindow? = nil
) throws {
    let targetWindow = window ?? backgroundActivationWindow(pid: pid, point: point)
    let eventPoint = point ?? targetWindow.map {
        CGPoint(x: $0.frame.midX, y: $0.frame.midY)
    }
    guard let event = CGEvent(
        scrollWheelEvent2Source: nil,
        units: .line,
        wheelCount: 2,
        wheel1: deltaY,
        wheel2: deltaX,
        wheel3: 0
    ) else {
        throw HelperFailure("failed to create scroll event")
    }
    if let eventPoint {
        event.location = eventPoint
    }
    event.setIntegerValueField(.eventTargetUnixProcessID, value: Int64(pid))
    if let targetWindow {
        event.setWindowAddressingFields(windowID: targetWindow.id)
        if let eventPoint {
            _ = BackgroundWindowLocalEvent.setPoint(
                backgroundQuartzWindowPoint(screenPoint: eventPoint, in: targetWindow),
                on: event
            )
        }
    }
    postPidEvent(event, to: pid)
}

func postText(pid: pid_t, text: String) {
    for scalar in text.unicodeScalars {
        var value = UniChar(scalar.value)
        if let down = CGEvent(keyboardEventSource: eventSource(), virtualKey: 0, keyDown: true) {
            withUnsafePointer(to: &value) { pointer in
                down.keyboardSetUnicodeString(stringLength: 1, unicodeString: pointer)
            }
            postPidEvent(down, to: pid)
        }
        if let up = CGEvent(keyboardEventSource: eventSource(), virtualKey: 0, keyDown: false) {
            withUnsafePointer(to: &value) { pointer in
                up.keyboardSetUnicodeString(stringLength: 1, unicodeString: pointer)
            }
            postPidEvent(up, to: pid)
        }
    }
}

func postKeyCode(pid: pid_t, keyCode: CGKeyCode, modifiers: [String]) throws {
    let flags = eventFlags(modifiers)
    let source = eventSource()
    guard let down = CGEvent(keyboardEventSource: source, virtualKey: keyCode, keyDown: true),
          let up = CGEvent(keyboardEventSource: source, virtualKey: keyCode, keyDown: false)
    else {
        throw HelperFailure("failed to create keyboard event")
    }
    down.flags = flags
    up.flags = flags
    postPidEvent(down, to: pid)
    postPidEvent(up, to: pid)
}

private func eventSource() -> CGEventSource? {
    let source = CGEventSource(stateID: .hidSystemState)
    source?.localEventsSuppressionInterval = 0
    return source
}

private func buttonNumber(for button: CGMouseButton) -> Int64 {
    switch button {
    case .left:
        return 0
    case .right:
        return 1
    case .center:
        return 2
    default:
        return Int64(button.rawValue)
    }
}

private func stampMouseEvent(
    _ event: CGEvent,
    pid: pid_t,
    point: CGPoint,
    window: BackgroundActivationWindow?,
    pressure: Double
) {
    event.setDoubleValueField(.mouseEventPressure, value: pressure)
    event.setIntegerValueField(.eventTargetUnixProcessID, value: Int64(pid))
    guard let window else {
        return
    }
    event.setIntegerValueField(.mouseEventWindowUnderMousePointer, value: Int64(window.id))
    event.setIntegerValueField(
        .mouseEventWindowUnderMousePointerThatCanHandleThisEvent,
        value: Int64(window.id)
    )
    event.setWindowAddressingFields(windowID: window.id)
    _ = BackgroundWindowLocalEvent.setPoint(
        backgroundQuartzWindowPoint(screenPoint: point, in: window),
        on: event
    )
}

private func clickDurationMicros(_ durationMs: Int?) -> useconds_t {
    guard let durationMs else {
        return 12_000
    }
    return useconds_t(max(0, min(10_000, durationMs)) * 1_000)
}

private func postPidEvent(_ event: CGEvent, to pid: pid_t) {
    event.postToPid(pid)
}

func keyCode(for params: [String: Any]) throws -> CGKeyCode {
    let kind = try requiredString(params, "kind")
    if kind == "Character" {
        let value = try requiredString(params, "value")
        guard let character = value.lowercased().first, let keyCode = characterKeyCodes[character] else {
            throw HelperFailure("unsupported character key `\(value)`")
        }
        return keyCode
    }
    guard let keyCode = namedKeyCodes[kind] else {
        throw HelperFailure("unsupported key `\(kind)` on macOS")
    }
    return keyCode
}

private func eventFlags(_ modifiers: [String]) -> CGEventFlags {
    var flags = CGEventFlags()
    for modifier in modifiers {
        switch modifier {
        case "Command":
            flags.insert(.maskCommand)
        case "Shift":
            flags.insert(.maskShift)
        case "Option":
            flags.insert(.maskAlternate)
        case "Control":
            flags.insert(.maskControl)
        case "Function":
            flags.insert(.maskSecondaryFn)
        default:
            break
        }
    }
    return flags
}

private let namedKeyCodes: [String: CGKeyCode] = [
    "Enter": 36,
    "Backspace": 51,
    "Escape": 53,
    "Space": 49,
    "Tab": 48,
    "LeftArrow": 123,
    "RightArrow": 124,
    "DownArrow": 125,
    "UpArrow": 126,
    "Home": 115,
]

private let characterKeyCodes: [Character: CGKeyCode] = [
    "a": 0, "s": 1, "d": 2, "f": 3, "h": 4, "g": 5, "z": 6, "x": 7,
    "c": 8, "v": 9, "b": 11, "q": 12, "w": 13, "e": 14, "r": 15,
    "y": 16, "t": 17, "1": 18, "2": 19, "3": 20, "4": 21, "6": 22,
    "5": 23, "=": 24, "9": 25, "7": 26, "-": 27, "8": 28, "0": 29,
    "]": 30, "o": 31, "u": 32, "[": 33, "i": 34, "p": 35, "l": 37,
    "j": 38, "'": 39, "k": 40, ";": 41, "\\": 42, ",": 43, "/": 44,
    "n": 45, "m": 46, ".": 47, "`": 50,
]
