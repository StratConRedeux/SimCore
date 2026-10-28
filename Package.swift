// swift-tools-version: 6.2
// The manifest SwiftPM consumers resolve: pin to a release tag, for example
//   .package(url: "https://github.com/StratConRedeux/SimCore.git", exact: "0.2.0")
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
            url: "https://github.com/StratConRedeux/SimCore/releases/download/0.6.0/SimCore.xcframework.zip",
            checksum: "ee089723937ff1ff597bb1a9b78f63dffa4deae0ab92ea7721a830c86d93f098"
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
