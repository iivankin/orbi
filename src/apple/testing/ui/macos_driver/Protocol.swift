import Foundation

struct HelperRequest {
    let id: Int
    let command: String
    let params: [String: Any]

    init(line: String) throws {
        guard let data = line.data(using: .utf8) else {
            throw HelperFailure("request line is not UTF-8")
        }
        let raw = try JSONSerialization.jsonObject(with: data)
        guard let object = raw as? [String: Any] else {
            throw HelperFailure("request must be a JSON object")
        }
        guard let id = object["id"] as? Int else {
            throw HelperFailure("request is missing numeric `id`")
        }
        guard let command = object["command"] as? String, !command.isEmpty else {
            throw HelperFailure("request is missing string `command`")
        }
        self.id = id
        self.command = command
        self.params = object["params"] as? [String: Any] ?? [:]
    }
}

struct HelperFailure: Error, CustomStringConvertible {
    let description: String

    init(_ description: String) {
        self.description = description
    }
}

func requiredString(_ params: [String: Any], _ key: String) throws -> String {
    guard let value = params[key] as? String, !value.isEmpty else {
        throw HelperFailure("missing string `\(key)`")
    }
    return value
}

func optionalString(_ params: [String: Any], _ key: String) -> String? {
    params[key] as? String
}

func requiredBool(_ params: [String: Any], _ key: String) throws -> Bool {
    guard let value = params[key] as? Bool else {
        throw HelperFailure("missing bool `\(key)`")
    }
    return value
}

func optionalBool(_ params: [String: Any], _ key: String) -> Bool? {
    params[key] as? Bool
}

func requiredDouble(_ params: [String: Any], _ key: String) throws -> Double {
    if let value = params[key] as? Double {
        return value
    }
    if let value = params[key] as? Int {
        return Double(value)
    }
    throw HelperFailure("missing numeric `\(key)`")
}

func optionalInt(_ params: [String: Any], _ key: String) -> Int? {
    if let value = params[key] as? Int {
        return value
    }
    if let value = params[key] as? Double {
        return Int(value)
    }
    return nil
}

func requiredStringArray(_ params: [String: Any], _ key: String) throws -> [String] {
    guard let values = params[key] as? [String] else {
        throw HelperFailure("missing string array `\(key)`")
    }
    return values
}

func optionalDictionary(_ params: [String: Any], _ key: String) -> [String: Any]? {
    params[key] as? [String: Any]
}

func writeResponse(id: Int, ok: Bool, result: Any?, error: String?) {
    let response: [String: Any] = [
        "id": id,
        "ok": ok,
        "result": result ?? NSNull(),
        "error": error ?? NSNull(),
    ]
    do {
        let data = try JSONSerialization.data(withJSONObject: response)
        FileHandle.standardOutput.write(data)
        FileHandle.standardOutput.write(Data([0x0A]))
    } catch {
        let fallback = #"{"id":\#(id),"ok":false,"result":null,"error":"failed to encode helper response"}"#
        FileHandle.standardOutput.write(Data(fallback.utf8))
        FileHandle.standardOutput.write(Data([0x0A]))
    }
}
