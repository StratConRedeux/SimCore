// swift-tools-version: 6.2
// The manifest SwiftPM consumers resolve: pin to a release tag, for example
//   .package(url: "https://github.com/StratConRedeux/SimCore.git", exact: "<version>")
// The XCFramework comes from that tag's GitHub Release asset. scripts/release.sh rewrites the
// `url` and `checksum` below; don't edit them by hand. For local development against a fresh
// build/SimCore.xcframework, use bindings/swift/Package.swift instead (docs/releasing.md).
import PackageDescription

let package = Package(
    name: "SimCore",
    platforms: [.iOS("27.0"), .macOS("26.0")],
    products: [
        .library(name: "SimCoreKit", targets: ["SimCoreKit"]),
    ],
    targets: [
        // Exposes the C module `SimCoreC`.
        .binaryTarget(
            name: "SimCore",
            url: "https://github.com/StratConRedeux/SimCore/releases/download/0.9.0/SimCore.xcframework.zip",
            checksum: "59489aab423f31ddcc8ba6f837a5b23505cb6211a24943bafabea7a6cee2938b"
        ),
        // Same sources as bindings/swift; nothing under crates/, target/ or build/ is read.
        .target(
            name: "SimCoreKit",
            dependencies: ["SimCore"],
            path: "bindings/swift/Sources/SimCoreKit"
        ),
        .target(
            name: "SimCoreBench",
            dependencies: ["SimCoreKit"],
            path: "bindings/swift/Sources/SimCoreBench"
        ),
        .testTarget(
            name: "SimCoreKitTests",
            dependencies: ["SimCoreKit", "SimCoreBench"],
            path: "bindings/swift/Tests/SimCoreKitTests"
        ),
    ]
)
