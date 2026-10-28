// swift-tools-version: 6.2
import PackageDescription

let package = Package(
    name: "SimCoreKit",
    platforms: [.iOS("27.0"), .macOS("26.0")],
    products: [
        .library(name: "SimCoreKit", targets: ["SimCoreKit"]),
        .executable(name: "simcore-bench", targets: ["simcore-bench"]),
    ],
    targets: [
        // Built by scripts/build-xcframework.sh; exposes the C module `SimCoreC`.
        .binaryTarget(name: "SimCore", path: "../../build/SimCore.xcframework"),
        .target(name: "SimCoreKit", dependencies: ["SimCore"]),
        // Bridge benchmark, shared by the executable (macOS) and a test (iOS simulator).
        .target(name: "SimCoreBench", dependencies: ["SimCoreKit"]),
        .executableTarget(name: "simcore-bench", dependencies: ["SimCoreBench"]),
        .testTarget(
            name: "SimCoreKitTests",
            dependencies: ["SimCoreKit", "SimCoreBench"]
        ),
    ]
)
