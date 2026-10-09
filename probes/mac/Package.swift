// swift-tools-version:5.9
import PackageDescription

let package = Package(
    name: "kvmprobe",
    platforms: [.macOS(.v13)],
    targets: [
        .target(name: "m1ddcShim", path: "Sources/m1ddcShim"),
        .executableTarget(name: "kvmprobe", dependencies: ["m1ddcShim"], path: "Sources/kvmprobe",
                          linkerSettings: [.linkedFramework("IOKit"), .linkedFramework("CoreGraphics"), .linkedFramework("CoreDisplay")])
    ]
)
