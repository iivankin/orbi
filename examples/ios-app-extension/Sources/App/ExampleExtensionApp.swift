import SwiftUI

@main
struct ExampleExtensionApp: App {
    var body: some Scene {
        WindowGroup {
            Text(SharedConfig.displayName)
                .padding()
        }
    }
}
