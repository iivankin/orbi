// swift-tools-version: 6.0
import PackageDescription

let package = Package(
    name: "OrbitGreeting",
    products: [
        .library(name: "OrbitGreeting", targets: ["OrbitGreeting"]),
    ],
    targets: [
        .target(name: "OrbitGreeting"),
    ]
)
