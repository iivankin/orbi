import AppKit
import SwiftUI

struct SwiftUICommandCoverageFixture: View {
    @State private var fieldValue = ""
    @State private var status = "SwiftUI command idle"
    @State private var doubleTapCount = 0
    @State private var pickedToken: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("SwiftUI Command Coverage")
                .font(.title3.bold())
                .accessibilityIdentifier("swiftui-coverage-title")

            TextField("SwiftUI Field", text: $fieldValue)
                .textFieldStyle(.roundedBorder)
                .frame(width: 280)
                .accessibilityIdentifier("swiftui-command-field")

            Text("SwiftUI field value: \(fieldValue)")
                .accessibilityIdentifier("swiftui-field-status")

            Text("SwiftUI copied value")
                .accessibilityIdentifier("swiftui-copy-source")

            HStack(spacing: 10) {
                Button("SwiftUI Tap") {
                    status = "SwiftUI tap recognized"
                }
                .accessibilityIdentifier("swiftui-tap-button")

                Button("SwiftUI Double") {
                    doubleTapCount += 1
                    status = "SwiftUI double count \(doubleTapCount)"
                }
                .accessibilityIdentifier("swiftui-double-button")

                Menu("SwiftUI Coverage Menu") {
                    Button("SwiftUI Menu Action") {
                        status = "SwiftUI menu selected"
                    }
                }
                .accessibilityIdentifier("swiftui-coverage-menu")
            }

            HStack(spacing: 10) {
                swiftUIActionBox("SwiftUI Long Target", id: "swiftui-long-target")
                    .onLongPressGesture(minimumDuration: 0.3) {
                        status = "SwiftUI long press recognized"
                    }
                    .accessibilityAction(named: Text("AXPress")) {
                        status = "SwiftUI long press recognized"
                    }

                swiftUIActionBox("SwiftUI Hover Target", id: "swiftui-hover-target")
                    .onHover { inside in
                        if inside {
                            status = "SwiftUI hover recognized"
                        }
                    }
                    .accessibilityAction(named: Text("AXShowDefaultUI")) {
                        status = "SwiftUI hover recognized"
                    }

                swiftUIActionBox("SwiftUI Right Target", id: "swiftui-right-target")
                    .accessibilityAction(named: Text("AXShowMenu")) {
                        status = "SwiftUI right click recognized"
                    }
            }

            HStack(spacing: 10) {
                swiftUIActionBox("SwiftUI Drag Source", id: "swiftui-drag-source")
                    .accessibilityAction(named: Text("AXPick")) {
                        pickedToken = "swiftui-token"
                    }

                swiftUIActionBox("SwiftUI Drop Target", id: "swiftui-drop-target")
                    .accessibilityAction(named: Text("AXConfirm")) {
                        if pickedToken == "swiftui-token" {
                            status = "SwiftUI dropped swiftui-token"
                            pickedToken = nil
                        }
                    }
            }

            Text(status)
                .font(.headline)
                .accessibilityIdentifier("swiftui-command-status")

            ScrollView {
                LazyVStack(alignment: .leading, spacing: 8) {
                    ForEach(1...24, id: \.self) { index in
                        Text("SwiftUI Coverage Row \(index)")
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.vertical, 3)
                    }

                    Text("SwiftUI Coverage Footer")
                        .font(.headline)
                        .accessibilityIdentifier("swiftui-scroll-footer")
                        .padding(.top, 8)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
            }
            .frame(width: 360, height: 150)
            .accessibilityIdentifier("swiftui-command-scroll")
        }
        .padding(24)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private func swiftUIActionBox(_ title: String, id: String) -> some View {
        Text(title)
            .font(.system(size: 13, weight: .semibold))
            .multilineTextAlignment(.center)
            .frame(width: 150, height: 62)
            .background(.teal.opacity(0.12))
            .overlay {
                RoundedRectangle(cornerRadius: 8)
                    .strokeBorder(.teal.opacity(0.35), lineWidth: 1)
            }
            .clipShape(RoundedRectangle(cornerRadius: 8))
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(title)
            .accessibilityIdentifier(id)
    }
}

struct AppKitCommandCoverageFixture: NSViewRepresentable {
    func makeNSView(context: Context) -> AppKitCommandCoverageRootView {
        AppKitCommandCoverageRootView()
    }

    func updateNSView(_ nsView: AppKitCommandCoverageRootView, context: Context) {}
}

final class AppKitCommandCoverageRootView: NSView {
    private let statusLabel = NSTextField(labelWithString: "AppKit command idle")
    private let textField = NSTextField(string: "")
    private var doubleTapCount = 0
    private var pickedToken: String?

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        buildView()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        nil
    }

    private func buildView() {
        let stack = NSStackView()
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 10
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)

        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 24),
            stack.topAnchor.constraint(equalTo: topAnchor, constant: 24),
            stack.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -24),
            stack.bottomAnchor.constraint(lessThanOrEqualTo: bottomAnchor, constant: -24),
        ])

        stack.addArrangedSubview(label("AppKit Command Coverage", id: "appkit-coverage-title", font: .boldSystemFont(ofSize: 15)))

        textField.placeholderString = "AppKit Field"
        textField.setAccessibilityIdentifier("appkit-command-field")
        textField.widthAnchor.constraint(equalToConstant: 280).isActive = true
        stack.addArrangedSubview(textField)

        stack.addArrangedSubview(label("AppKit copied value", id: "appkit-copy-source"))

        let buttonRow = NSStackView(views: [
            button("AppKit Tap", id: "appkit-tap-button", action: #selector(tapPressed)),
            button("AppKit Double", id: "appkit-double-button", action: #selector(doublePressed)),
            menuButton(),
        ])
        buttonRow.orientation = .horizontal
        buttonRow.spacing = 10
        stack.addArrangedSubview(buttonRow)

        let interactionRow = NSStackView(views: [
            box("AppKit Long Target", id: "appkit-long-target", onLongPress: { [weak self] in
                self?.setStatus("AppKit long press recognized")
            }),
            box("AppKit Hover Target", id: "appkit-hover-target", onHover: { [weak self] in
                self?.setStatus("AppKit hover recognized")
            }),
            box("AppKit Right Target", id: "appkit-right-target", onRightClick: { [weak self] in
                self?.setStatus("AppKit right click recognized")
            }),
        ])
        interactionRow.orientation = .horizontal
        interactionRow.spacing = 10
        stack.addArrangedSubview(interactionRow)

        let dragRow = NSStackView(views: [
            box("AppKit Drag Source", id: "appkit-drag-source", onPick: { [weak self] in
                self?.pickedToken = "appkit-token"
            }),
            box("AppKit Drop Target", id: "appkit-drop-target", onConfirm: { [weak self] in
                guard let self, pickedToken == "appkit-token" else {
                    return false
                }
                pickedToken = nil
                setStatus("AppKit dropped appkit-token")
                return true
            }),
        ])
        dragRow.orientation = .horizontal
        dragRow.spacing = 10
        stack.addArrangedSubview(dragRow)

        statusLabel.font = .boldSystemFont(ofSize: 13)
        statusLabel.setAccessibilityIdentifier("appkit-command-status")
        stack.addArrangedSubview(statusLabel)

        stack.addArrangedSubview(scrollView())
    }

    private func label(_ text: String, id: String? = nil, font: NSFont = .systemFont(ofSize: 13)) -> NSTextField {
        let label = NSTextField(labelWithString: text)
        label.font = font
        if let id {
            label.setAccessibilityIdentifier(id)
        }
        return label
    }

    private func button(_ title: String, id: String, action: Selector) -> NSButton {
        let button = NSButton(title: title, target: self, action: action)
        button.bezelStyle = .rounded
        button.setAccessibilityIdentifier(id)
        return button
    }

    private func menuButton() -> NSPopUpButton {
        let button = NSPopUpButton(frame: .zero, pullsDown: false)
        button.addItems(withTitles: ["Choose AppKit Action", "AppKit Menu Action"])
        button.target = self
        button.action = #selector(menuChanged(_:))
        button.setAccessibilityIdentifier("appkit-coverage-menu")
        return button
    }

    private func box(
        _ title: String,
        id: String,
        onLongPress: (() -> Void)? = nil,
        onHover: (() -> Void)? = nil,
        onRightClick: (() -> Void)? = nil,
        onPick: (() -> Void)? = nil,
        onConfirm: (() -> Bool)? = nil
    ) -> AppKitCommandBoxView {
        let box = AppKitCommandBoxView(title: title)
        box.onLongPress = onLongPress
        box.onHover = onHover
        box.onRightClick = onRightClick
        box.onPick = onPick
        box.onConfirm = onConfirm
        box.setAccessibilityIdentifier(id)
        box.widthAnchor.constraint(equalToConstant: 150).isActive = true
        box.heightAnchor.constraint(equalToConstant: 62).isActive = true
        return box
    }

    private func scrollView() -> NSScrollView {
        let scrollView = NSScrollView()
        scrollView.hasVerticalScroller = true
        scrollView.borderType = .bezelBorder
        scrollView.setAccessibilityIdentifier("appkit-command-scroll")
        scrollView.widthAnchor.constraint(equalToConstant: 360).isActive = true
        scrollView.heightAnchor.constraint(equalToConstant: 150).isActive = true

        let stack = NSStackView()
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 8
        stack.translatesAutoresizingMaskIntoConstraints = false

        for index in 1...24 {
            stack.addArrangedSubview(label("AppKit Coverage Row \(index)"))
        }
        stack.addArrangedSubview(label("AppKit Coverage Footer", id: "appkit-scroll-footer", font: .boldSystemFont(ofSize: 13)))

        let document = NSView()
        document.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: document.leadingAnchor, constant: 8),
            stack.topAnchor.constraint(equalTo: document.topAnchor, constant: 8),
            stack.trailingAnchor.constraint(lessThanOrEqualTo: document.trailingAnchor, constant: -8),
            stack.bottomAnchor.constraint(equalTo: document.bottomAnchor, constant: -8),
            document.widthAnchor.constraint(equalToConstant: 330),
        ])
        scrollView.documentView = document
        return scrollView
    }

    private func setStatus(_ value: String) {
        statusLabel.stringValue = value
    }

    @objc private func tapPressed() {
        setStatus("AppKit tap recognized")
    }

    @objc private func doublePressed() {
        doubleTapCount += 1
        setStatus("AppKit double count \(doubleTapCount)")
    }

    @objc private func menuChanged(_ sender: NSPopUpButton) {
        if sender.titleOfSelectedItem == "AppKit Menu Action" {
            setStatus("AppKit menu selected")
        }
    }
}

private final class AppKitCommandBoxView: NSView {
    var onLongPress: (() -> Void)?
    var onHover: (() -> Void)?
    var onRightClick: (() -> Void)?
    var onPick: (() -> Void)?
    var onConfirm: (() -> Bool)?

    private let label = NSTextField(labelWithString: "")
    private var trackingArea: NSTrackingArea?
    private var mouseDownDate: Date?

    init(title: String) {
        super.init(frame: .zero)
        wantsLayer = true
        layer?.backgroundColor = NSColor.systemTeal.withAlphaComponent(0.12).cgColor
        layer?.cornerRadius = 8
        layer?.borderWidth = 1
        layer?.borderColor = NSColor.systemTeal.withAlphaComponent(0.35).cgColor

        label.stringValue = title
        label.alignment = .center
        label.font = .systemFont(ofSize: 13, weight: .semibold)
        label.lineBreakMode = .byWordWrapping
        label.maximumNumberOfLines = 2
        label.translatesAutoresizingMaskIntoConstraints = false
        addSubview(label)
        NSLayoutConstraint.activate([
            label.centerXAnchor.constraint(equalTo: centerXAnchor),
            label.centerYAnchor.constraint(equalTo: centerYAnchor),
            label.leadingAnchor.constraint(greaterThanOrEqualTo: leadingAnchor, constant: 8),
            label.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -8),
        ])

        setAccessibilityElement(true)
        setAccessibilityRole(.button)
        setAccessibilityLabel(title)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        nil
    }

    override func updateTrackingAreas() {
        if let trackingArea {
            removeTrackingArea(trackingArea)
        }
        let trackingArea = NSTrackingArea(
            rect: bounds,
            options: [.activeAlways, .mouseEnteredAndExited, .mouseMoved, .inVisibleRect],
            owner: self,
            userInfo: nil
        )
        addTrackingArea(trackingArea)
        self.trackingArea = trackingArea
        super.updateTrackingAreas()
    }

    override func viewDidMoveToWindow() {
        super.viewDidMoveToWindow()
        window?.acceptsMouseMovedEvents = true
    }

    override func mouseDown(with event: NSEvent) {
        mouseDownDate = Date()
    }

    override func mouseUp(with event: NSEvent) {
        guard let mouseDownDate else {
            return
        }
        if Date().timeIntervalSince(mouseDownDate) >= 0.25 {
            onLongPress?()
        }
        self.mouseDownDate = nil
    }

    override func mouseEntered(with event: NSEvent) {
        onHover?()
    }

    override func mouseMoved(with event: NSEvent) {
        onHover?()
    }

    override func rightMouseDown(with event: NSEvent) {
        onRightClick?()
    }

    override func accessibilityPerformShowDefaultUI() -> Bool {
        guard let onHover else {
            return false
        }
        onHover()
        return true
    }

    override func accessibilityPerformPress() -> Bool {
        guard let onLongPress else {
            return false
        }
        onLongPress()
        return true
    }

    override func accessibilityPerformShowMenu() -> Bool {
        guard let onRightClick else {
            return false
        }
        onRightClick()
        return true
    }

    override func accessibilityPerformPick() -> Bool {
        guard let onPick else {
            return false
        }
        onPick()
        return true
    }

    override func accessibilityPerformConfirm() -> Bool {
        onConfirm?() ?? false
    }
}
