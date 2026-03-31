import Darwin
import Foundation
import SwiftFormat

private struct FormatRequest: Decodable {
    enum Mode: String, Decodable {
        case check
        case write
    }

    let workingDirectory: String
    let configurationJson: String?
    let mode: Mode
    let files: [String]
}

private enum FormatToolError: LocalizedError {
    case usage
    case invalidWorkingDirectory(String)

    var errorDescription: String? {
        switch self {
        case .usage:
            return "usage: orbit-swift-format <request.json>"
        case let .invalidWorkingDirectory(path):
            return "failed to change directory to \(path)"
        }
    }
}

@main
struct OrbitSwiftFormatTool {
    static func main() {
        do {
            try run()
        } catch {
            writeToStandardError("error: \(error.localizedDescription)\n")
            exit(1)
        }
    }

    private static func run() throws {
        guard CommandLine.arguments.count == 2 else {
            throw FormatToolError.usage
        }

        let request = try decodeRequest(at: CommandLine.arguments[1])
        guard FileManager.default.changeCurrentDirectoryPath(request.workingDirectory) else {
            throw FormatToolError.invalidWorkingDirectory(request.workingDirectory)
        }

        let configuration = try loadConfiguration(from: request.configurationJson)
        switch request.mode {
        case .check:
            let findings = try lint(files: request.files, configuration: configuration)
            guard findings.isEmpty else {
                emit(findings: findings)
                exit(2)
            }
        case .write:
            try format(files: request.files, configuration: configuration)
        }
    }

    private static func decodeRequest(at path: String) throws -> FormatRequest {
        let data = try Data(contentsOf: URL(fileURLWithPath: path))
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try decoder.decode(FormatRequest.self, from: data)
    }

    private static func loadConfiguration(from json: String?) throws -> Configuration {
        guard let json else {
            return Configuration()
        }
        return try Configuration(data: Data(json.utf8))
    }

    private static func lint(files: [String], configuration: Configuration) throws -> [Finding] {
        var findings = [Finding]()
        for path in files {
            let consumer: (Finding) -> Void = { finding in
                findings.append(finding)
            }
            let linter = SwiftLinter(configuration: configuration, findingConsumer: consumer)
            try linter.lint(contentsOf: URL(fileURLWithPath: path))
        }
        return findings
    }

    private static func format(files: [String], configuration: Configuration) throws {
        let formatter = SwiftFormatter(configuration: configuration)
        for path in files {
            let url = URL(fileURLWithPath: path)
            let original = try String(contentsOf: url, encoding: .utf8)
            var formatted = ""
            try formatter.format(contentsOf: url, to: &formatted)
            guard original != formatted else {
                continue
            }
            try formatted.write(to: url, atomically: true, encoding: .utf8)
        }
    }

    private static func emit(findings: [Finding]) {
        for finding in findings {
            let location = finding.location ?? Finding.Location(file: "<unknown>", line: 1, column: 1)
            writeToStandardOutput(
                "\(location.file):\(location.line):\(location.column): error: [\(finding.category)] \(finding.message)\n"
            )
            for note in finding.notes {
                let noteLocation = note.location ?? location
                writeToStandardOutput(
                    "\(noteLocation.file):\(noteLocation.line):\(noteLocation.column): note: \(note.message)\n"
                )
            }
        }
    }
}

private func writeToStandardOutput(_ message: String) {
    FileHandle.standardOutput.write(Data(message.utf8))
}

private func writeToStandardError(_ message: String) {
    FileHandle.standardError.write(Data(message.utf8))
}
