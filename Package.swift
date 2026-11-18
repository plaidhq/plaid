// swift-tools-version:5.10
import PackageDescription

let package = Package(
    name: "Plaid",
    platforms: [.macOS("14.0")],
    targets: [.executableTarget(name: "Plaid", path: "Sources/Plaid")]
)
