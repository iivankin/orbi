// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "orbit-swift-format",
    platforms: [.macOS(.v13)],
    products: [
        .executable(name: "orbit-swift-format", targets: ["orbit-swift-format"]),
    ],
    dependencies: [
        .package(url: "https://github.com/swiftlang/swift-format.git", exact: "602.0.0"),
    ],
    targets: [
        .executableTarget(
            name: "orbit-swift-format",
            dependencies: [
                .product(name: "SwiftFormat", package: "swift-format"),
            ]
        ),
    ]
)
