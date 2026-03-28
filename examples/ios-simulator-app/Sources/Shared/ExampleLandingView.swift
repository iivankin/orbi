import OrbitGreeting
import SwiftUI

struct ExampleLandingView: View {
    var body: some View {
        VStack(spacing: 18) {
            Image(systemName: "swift")
                .font(.system(size: 48))
                .foregroundStyle(Color("AccentColor"))
            Text("Orbit")
                .font(.largeTitle.weight(.bold))
            Text("swiftc + manifest-driven Apple app launch")
                .font(.headline)
                .multilineTextAlignment(.center)
                .foregroundStyle(.secondary)
            Text(OrbitGreeting.headline)
                .font(.subheadline.weight(.medium))
                .multilineTextAlignment(.center)
        }
        .padding(32)
        .frame(minWidth: 360, minHeight: 280)
    }
}
