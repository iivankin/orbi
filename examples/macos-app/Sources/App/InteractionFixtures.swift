import AppKit
import SwiftUI

struct SecondaryClickFixture: View {
    @Binding var status: String

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Secondary Click")
                .font(.headline)

            SecondaryClickCaptureView {
                status = "Secondary click recognized"
            }
            .frame(width: 180, height: 84)

            Text(status)
                .font(.subheadline.weight(.medium))
                .accessibilityIdentifier("secondary-click-status")
        }
    }
}

struct DragAndDropFixture: View {
    @Binding var status: String
    @State private var pickedToken: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Drag And Drop")
                .font(.headline)

            HStack(spacing: 12) {
                SemanticDragSourceView(title: "Orbi token") {
                    pickedToken = "orbi-token"
                }
                .frame(width: 110, height: 84)

                SemanticDropTargetView(title: status) {
                    guard let pickedToken else {
                        return false
                    }
                    status = "Dropped \(pickedToken)"
                    self.pickedToken = nil
                    return true
                }
                .frame(width: 150, height: 84)
            }
        }
    }
}

struct AppKitTableFixture: View {
    @Binding var status: String

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("AppKit Table")
                .font(.headline)

            AppKitTableView { row in
                status = "Activated \(row)"
            }
            .frame(width: 220, height: 120)

            Text(status)
                .font(.subheadline.weight(.medium))
                .accessibilityIdentifier("appkit-table-status")
        }
    }
}

private struct SecondaryClickCaptureView: NSViewRepresentable {
    let onSecondaryClick: () -> Void

    func makeNSView(context: Context) -> SecondaryClickTargetView {
        let view = SecondaryClickTargetView()
        view.onSecondaryClick = onSecondaryClick
        view.setAccessibilityElement(true)
        view.setAccessibilityRole(.button)
        view.setAccessibilityLabel("Secondary click area")
        view.setAccessibilityIdentifier("secondary-click-area")
        return view
    }

    func updateNSView(_ nsView: SecondaryClickTargetView, context: Context) {
        nsView.onSecondaryClick = onSecondaryClick
    }
}

private struct AppKitTableView: NSViewRepresentable {
    let onActivate: (String) -> Void

    func makeCoordinator() -> Coordinator {
        Coordinator(onActivate: onActivate)
    }

    func makeNSView(context: Context) -> NSScrollView {
        let scrollView = NSScrollView()
        scrollView.hasVerticalScroller = true
        scrollView.borderType = .bezelBorder

        let tableView = NSTableView()
        tableView.headerView = nil
        tableView.usesAutomaticRowHeights = false
        tableView.rowHeight = 28
        tableView.intercellSpacing = .zero
        tableView.selectionHighlightStyle = .regular
        tableView.allowsEmptySelection = false
        tableView.delegate = context.coordinator
        tableView.dataSource = context.coordinator
        tableView.target = context.coordinator
        tableView.action = #selector(Coordinator.activateSelection(_:))
        tableView.setAccessibilityIdentifier("appkit-table")

        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("title"))
        column.width = 200
        tableView.addTableColumn(column)

        scrollView.documentView = tableView
        context.coordinator.tableView = tableView
        tableView.reloadData()
        return scrollView
    }

    func updateNSView(_ nsView: NSScrollView, context: Context) {
        context.coordinator.onActivate = onActivate
    }

    final class Coordinator: NSObject, NSTableViewDataSource, NSTableViewDelegate {
        let rows = ["Inbox", "Sent", "Archive"]
        weak var tableView: NSTableView?
        var onActivate: (String) -> Void

        init(onActivate: @escaping (String) -> Void) {
            self.onActivate = onActivate
        }

        func numberOfRows(in tableView: NSTableView) -> Int {
            rows.count
        }

        func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
            let identifier = NSUserInterfaceItemIdentifier("cell")
            let label: NSTextField
            if let reused = tableView.makeView(withIdentifier: identifier, owner: nil) as? NSTextField {
                label = reused
            } else {
                label = NSTextField(labelWithString: "")
                label.identifier = identifier
                label.lineBreakMode = .byTruncatingTail
            }

            label.stringValue = rows[row]
            return label
        }

        func tableViewSelectionDidChange(_ notification: Notification) {
            guard let tableView,
                  tableView.selectedRow >= 0,
                  tableView.selectedRow < rows.count
            else {
                return
            }
            onActivate(rows[tableView.selectedRow])
        }

        @objc func activateSelection(_ sender: NSTableView) {
            let row = sender.clickedRow >= 0 ? sender.clickedRow : sender.selectedRow
            guard row >= 0, row < rows.count else {
                return
            }
            onActivate(rows[row])
        }
    }
}

private struct SemanticDragSourceView: NSViewRepresentable {
    let title: String
    let onPick: () -> Void

    func makeNSView(context: Context) -> SemanticActionBoxView {
        let view = SemanticActionBoxView(title: title)
        view.onPick = onPick
        view.setAccessibilityElement(true)
        view.setAccessibilityRole(.staticText)
        view.setAccessibilityLabel(title)
        view.setAccessibilityIdentifier("drag-source")
        return view
    }

    func updateNSView(_ nsView: SemanticActionBoxView, context: Context) {
        nsView.setTitle(title)
        nsView.onPick = onPick
    }
}

private struct SemanticDropTargetView: NSViewRepresentable {
    let title: String
    let onConfirm: () -> Bool

    func makeNSView(context: Context) -> SemanticActionBoxView {
        let view = SemanticActionBoxView(title: title, drawsBorder: true)
        view.onConfirm = onConfirm
        view.setAccessibilityElement(true)
        view.setAccessibilityRole(.staticText)
        view.setAccessibilityLabel(title)
        view.setAccessibilityIdentifier("drop-target")
        return view
    }

    func updateNSView(_ nsView: SemanticActionBoxView, context: Context) {
        nsView.setTitle(title)
        nsView.onConfirm = onConfirm
    }
}

private final class SemanticActionBoxView: NSView {
    var onPick: (() -> Void)?
    var onConfirm: (() -> Bool)?

    private let label = NSTextField(labelWithString: "")

    init(title: String, drawsBorder: Bool = false) {
        super.init(frame: .zero)
        wantsLayer = true
        layer?.backgroundColor = NSColor.quaternaryLabelColor.withAlphaComponent(0.2).cgColor
        layer?.cornerRadius = 12
        layer?.borderWidth = drawsBorder ? 1 : 0
        layer?.borderColor = NSColor.separatorColor.cgColor

        label.alignment = .center
        label.font = .systemFont(ofSize: 13, weight: .semibold)
        label.lineBreakMode = .byWordWrapping
        label.maximumNumberOfLines = 2
        label.translatesAutoresizingMaskIntoConstraints = false
        addSubview(label)
        NSLayoutConstraint.activate([
            label.centerXAnchor.constraint(equalTo: centerXAnchor),
            label.centerYAnchor.constraint(equalTo: centerYAnchor),
            label.leadingAnchor.constraint(greaterThanOrEqualTo: leadingAnchor, constant: 12),
            label.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -12),
        ])
        setTitle(title)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        nil
    }

    func setTitle(_ title: String) {
        label.stringValue = title
        setAccessibilityLabel(title)
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

private final class SecondaryClickTargetView: NSView {
    var onSecondaryClick: (() -> Void)?

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        wantsLayer = true
        layer?.backgroundColor = NSColor.quaternaryLabelColor.withAlphaComponent(0.2).cgColor
        layer?.cornerRadius = 12
        layer?.borderWidth = 1
        layer?.borderColor = NSColor.separatorColor.cgColor
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) {
        nil
    }

    override func rightMouseDown(with event: NSEvent) {
        onSecondaryClick?()
    }

    override func accessibilityPerformShowMenu() -> Bool {
        onSecondaryClick?()
        return true
    }
}
