import ApplicationServices
import CoreGraphics
import Foundation

struct UiSelector {
    let text: String?
    let identifier: String?

    init(_ params: [String: Any]) {
        self.text = params["text"] as? String
        self.identifier = params["id"] as? String
    }

    var summary: String {
        if let text, let identifier {
            return "text `\(text)` and id `\(identifier)`"
        }
        if let text {
            return "text `\(text)`"
        }
        if let identifier {
            return "id `\(identifier)`"
        }
        return "empty selector"
    }
}

struct ElementCandidate {
    let element: AXUIElement
    let score: Int
    let label: String
    let frame: CGRect?
}

struct ElementAction {
    let raw: String
    let name: String
}

private let readableAttributes = [
    kAXRoleAttribute as String,
    kAXSubroleAttribute as String,
    kAXTitleAttribute as String,
    kAXDescriptionAttribute as String,
    kAXIdentifierAttribute as String,
    kAXValueAttribute as String,
    kAXEnabledAttribute as String,
    kAXFocusedAttribute as String,
    kAXHiddenAttribute as String,
    kAXHelpAttribute as String,
]

private let childAttributes = [
    kAXWindowsAttribute as String,
    kAXMenuBarAttribute as String,
    kAXChildrenAttribute as String,
    kAXVisibleChildrenAttribute as String,
    "AXVisibleRows",
    "AXRows",
    "AXColumns",
    "AXContents",
    "AXToolbar",
    "AXTabs",
    kAXFocusedWindowAttribute as String,
]

func axApplication(pid: pid_t) -> AXUIElement {
    let app = AXUIElementCreateApplication(pid)
    AXUIElementSetMessagingTimeout(app, 1.0)
    return app
}

func pid(of element: AXUIElement) -> pid_t? {
    var pid: pid_t = 0
    guard AXUIElementGetPid(element, &pid) == .success else {
        return nil
    }
    return pid
}

func attribute(_ element: AXUIElement, _ name: String) -> Any? {
    var value: CFTypeRef?
    guard AXUIElementCopyAttributeValue(element, name as CFString, &value) == .success else {
        return nil
    }
    return value
}

func stringAttribute(_ element: AXUIElement, _ name: String) -> String? {
    guard let value = attribute(element, name) else {
        return nil
    }
    if let string = value as? String {
        return string
    }
    if let number = value as? NSNumber {
        return number.stringValue
    }
    return nil
}

func boolAttribute(_ element: AXUIElement, _ name: String) -> Bool? {
    guard let value = attribute(element, name) else {
        return nil
    }
    return (value as? NSNumber)?.boolValue
}

func actionNames(_ element: AXUIElement) -> [String] {
    elementActions(element).map(\.name)
}

func elementActions(_ element: AXUIElement) -> [ElementAction] {
    var names: CFArray?
    guard AXUIElementCopyActionNames(element, &names) == .success else {
        return []
    }
    return (names as? [String] ?? []).map { raw in
        ElementAction(raw: raw, name: cleanActionName(raw))
    }
}

func isAttributeSettable(_ element: AXUIElement, _ name: String) -> Bool {
    var settable = DarwinBoolean(false)
    guard AXUIElementIsAttributeSettable(element, name as CFString, &settable) == .success else {
        return false
    }
    return settable.boolValue
}

func frame(of element: AXUIElement) -> CGRect? {
    guard let positionValue = attribute(element, kAXPositionAttribute as String),
          let sizeValue = attribute(element, kAXSizeAttribute as String),
          let position = cgPoint(from: positionValue),
          let size = cgSize(from: sizeValue)
    else {
        return nil
    }
    return CGRect(origin: position, size: size)
}

func snapshot(_ element: AXUIElement, depth: Int = 0, visited: inout Set<CFHashCode>, budget: inout Int) -> [String: Any] {
    AXUIElementSetMessagingTimeout(element, 1.0)
    budget -= 1

    var node = [String: Any]()
    for name in readableAttributes {
        if let value = jsonValue(attribute(element, name)) {
            node[name] = value
        }
    }
    if let rect = frame(of: element) {
        node["frame"] = frameDictionary(rect)
    }
    let actions = actionNames(element)
    if !actions.isEmpty {
        node["actions"] = actions
    }
    if let label = label(for: element), !label.isEmpty {
        node["AXLabel"] = label
    }

    guard depth < 24, budget > 0 else {
        return node
    }
    let key = CFHash(element)
    if visited.contains(key) {
        return node
    }
    visited.insert(key)

    var children = [[String: Any]]()
    for child in childElements(element) {
        if budget <= 0 {
            break
        }
        children.append(snapshot(child, depth: depth + 1, visited: &visited, budget: &budget))
    }
    if !children.isEmpty {
        node["children"] = children
    }
    return node
}

func childElements(_ element: AXUIElement) -> [AXUIElement] {
    var children = [AXUIElement]()
    for name in childAttributes {
        guard let value = attribute(element, name) else {
            continue
        }
        if let child = axElement(from: value) {
            children.append(child)
        } else if let array = value as? [Any] {
            children.append(contentsOf: array.compactMap(axElement(from:)))
        }
    }
    return children
}

func label(for element: AXUIElement) -> String? {
    for name in [
        kAXTitleAttribute as String,
        kAXDescriptionAttribute as String,
        kAXValueAttribute as String,
        kAXIdentifierAttribute as String,
        kAXHelpAttribute as String,
    ] {
        if let value = stringAttribute(element, name), !value.isEmpty {
            return value
        }
    }
    return nil
}

private func cleanActionName(_ raw: String) -> String {
    if raw.hasPrefix("AX") {
        return raw
    }
    for line in raw.split(whereSeparator: \.isNewline) {
        if let range = line.range(of: "Name:") {
            let name = line[range.upperBound...].trimmingCharacters(in: .whitespaces)
            if !name.isEmpty {
                return name
            }
        }
    }
    return raw
}

func bestElement(in roots: [AXUIElement], matching selector: UiSelector, targetPid: pid_t) -> ElementCandidate? {
    var candidates = [ElementCandidate]()
    var visited = Set<CFHashCode>()
    var budget = 8_000
    for root in roots {
        collectCandidates(root, selector: selector, targetPid: targetPid, depth: 0, visited: &visited, budget: &budget, candidates: &candidates)
    }
    return candidates.sorted { left, right in
        if left.score != right.score {
            return left.score > right.score
        }
        if (left.frame != nil) != (right.frame != nil) {
            return left.frame != nil
        }
        return left.label < right.label
    }.first
}

func elementAtPoint(app: AXUIElement, x: Double, y: Double) -> AXUIElement? {
    var element: AXUIElement?
    guard AXUIElementCopyElementAtPosition(app, Float(x), Float(y), &element) == .success else {
        return nil
    }
    return element
}

func parent(of element: AXUIElement) -> AXUIElement? {
    guard let value = attribute(element, kAXParentAttribute as String) else {
        return nil
    }
    return axElement(from: value)
}

private func collectCandidates(
    _ element: AXUIElement,
    selector: UiSelector,
    targetPid: pid_t,
    depth: Int,
    visited: inout Set<CFHashCode>,
    budget: inout Int,
    candidates: inout [ElementCandidate]
) {
    guard depth <= 24, budget > 0 else {
        return
    }
    budget -= 1
    let key = CFHash(element)
    if visited.contains(key) {
        return
    }
    visited.insert(key)

    if pid(of: element) == targetPid, let candidate = matchCandidate(element, selector: selector) {
        candidates.append(candidate)
    }
    for child in childElements(element) {
        collectCandidates(child, selector: selector, targetPid: targetPid, depth: depth + 1, visited: &visited, budget: &budget, candidates: &candidates)
    }
}

private func matchCandidate(_ element: AXUIElement, selector: UiSelector) -> ElementCandidate? {
    let textResult = selector.text.map { bestScore(element, keys: [
        "AXLabel",
        kAXTitleAttribute as String,
        kAXDescriptionAttribute as String,
        kAXValueAttribute as String,
    ], needle: $0) } ?? (1, nil)
    guard selector.text == nil || textResult.0 > 0 else {
        return nil
    }

    let idResult = selector.identifier.map { bestScore(element, keys: [
        kAXIdentifierAttribute as String,
    ], needle: $0) } ?? (1, nil)
    guard selector.identifier == nil || idResult.0 > 0 else {
        return nil
    }

    let score = textResult.0 + idResult.0
    guard score > 0 else {
        return nil
    }
    let displayLabel = label(for: element) ?? textResult.1 ?? idResult.1 ?? selector.summary
    return ElementCandidate(element: element, score: score, label: displayLabel, frame: frame(of: element))
}

private func bestScore(_ element: AXUIElement, keys: [String], needle: String) -> (Int, String?) {
    var best = 0
    var bestLabel: String?
    for key in keys {
        let value: String?
        if key == "AXLabel" {
            value = label(for: element)
        } else {
            value = stringAttribute(element, key)
        }
        guard let value else {
            continue
        }
        let score = matchScore(value, needle: needle)
        if score > best {
            best = score
            bestLabel = value
        }
    }
    return (best, bestLabel)
}

private func matchScore(_ value: String, needle: String) -> Int {
    if value == needle {
        return 3
    }
    if value.caseInsensitiveCompare(needle) == .orderedSame {
        return 2
    }
    if value.localizedCaseInsensitiveContains(needle) {
        return 1
    }
    return 0
}

private func jsonValue(_ value: Any?) -> Any? {
    guard let value else {
        return nil
    }
    if let string = value as? String {
        return string
    }
    if let number = value as? NSNumber {
        return number
    }
    if let array = value as? [Any] {
        return array.compactMap(jsonValue)
    }
    if let point = cgPoint(from: value) {
        return ["x": point.x, "y": point.y]
    }
    if let size = cgSize(from: value) {
        return ["width": size.width, "height": size.height]
    }
    return String(describing: value)
}

private func cgPoint(from value: Any) -> CGPoint? {
    let cfValue = value as CFTypeRef
    guard CFGetTypeID(cfValue) == AXValueGetTypeID() else {
        return nil
    }
    let axValue = value as! AXValue
    guard AXValueGetType(axValue) == .cgPoint else {
        return nil
    }
    var point = CGPoint.zero
    return AXValueGetValue(axValue, .cgPoint, &point) ? point : nil
}

private func cgSize(from value: Any) -> CGSize? {
    let cfValue = value as CFTypeRef
    guard CFGetTypeID(cfValue) == AXValueGetTypeID() else {
        return nil
    }
    let axValue = value as! AXValue
    guard AXValueGetType(axValue) == .cgSize else {
        return nil
    }
    var size = CGSize.zero
    return AXValueGetValue(axValue, .cgSize, &size) ? size : nil
}

func axElement(from value: Any) -> AXUIElement? {
    let cfValue = value as CFTypeRef
    guard CFGetTypeID(cfValue) == AXUIElementGetTypeID() else {
        return nil
    }
    return (value as! AXUIElement)
}

func frameDictionary(_ frame: CGRect) -> [String: Any] {
    [
        "x": frame.origin.x,
        "y": frame.origin.y,
        "width": frame.size.width,
        "height": frame.size.height,
    ]
}
