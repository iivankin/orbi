import Foundation

@main
enum OrbiMacosUiHelper {
    static func main() async {
        let automation = MacosAutomation()
        while let line = readLine() {
            do {
                let request = try HelperRequest(line: line)
                do {
                    let result = try await automation.handle(request)
                    writeResponse(id: request.id, ok: true, result: result, error: nil)
                } catch {
                    writeResponse(id: request.id, ok: false, result: nil, error: String(describing: error))
                }
            } catch {
                writeResponse(id: -1, ok: false, result: nil, error: String(describing: error))
            }
        }
    }
}
