import SwiftUI
import OrbitGreeting

@main
struct ExampleIOSApp: App {
    var body: some Scene {
        WindowGroup {
            VStack(spacing: 18) {
                Image(systemName: "swift")
                    .font(.system(size: 48))
                    .foregroundStyle(Color("AccentColor"))
                Text("Orbit")
                    .font(.largeTitle.weight(.bold))
                Text("swiftc + manifest-driven simulator launch")
                    .font(.headline)
                    .multilineTextAlignment(.center)
                    .foregroundStyle(.secondary)
                Text(OrbitGreeting.headline)
                    .font(.subheadline.weight(.medium))
                    .multilineTextAlignment(.center)
            }
            .padding(32)
        }
    }
}
