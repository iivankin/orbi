// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "orbit-swiftlint",
    platforms: [.macOS(.v13)],
    products: [
        .executable(name: "orbit-swiftlint", targets: ["orbit-swiftlint"]),
    ],
    dependencies: [
        .package(url: "https://github.com/realm/SwiftLint.git", exact: "0.63.2"),
    ],
    targets: [
        .executableTarget(
            name: "orbit-swiftlint",
            dependencies: [
                .product(name: "SwiftLintFramework", package: "SwiftLint"),
            ]
        ),
    ]
)
