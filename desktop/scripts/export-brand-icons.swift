import AppKit

guard CommandLine.arguments.count == 4,
      let icon = NSImage(contentsOfFile: CommandLine.arguments[1]) else { exit(1) }
let output = URL(fileURLWithPath: CommandLine.arguments[3])

func bitmap(_ width: Int, _ height: Int) -> NSBitmapImageRep {
    return NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: width, pixelsHigh: height,
        bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
        colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
}

func write(_ image: NSImage, size: Int, name: String) throws {
    let result = bitmap(size, size)
    let context = NSGraphicsContext(bitmapImageRep: result)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = context
    context.imageInterpolation = .high
    NSColor.clear.setFill()
    NSRect(x: 0, y: 0, width: size, height: size).fill()
    image.draw(in: NSRect(x: 0, y: 0, width: size, height: size),
        from: .zero, operation: .sourceOver, fraction: 1)
    NSGraphicsContext.restoreGraphicsState()
    try result.representation(using: .png, properties: [:])!.write(to: output.appendingPathComponent(name))
}

for size in [16, 24, 32, 48, 64, 128, 256, 512, 1024] {
    if size == 1024 {
        // Preserve the original bytes for the largest representation.
        try Data(contentsOf: URL(fileURLWithPath: CommandLine.arguments[1])).write(to: output.appendingPathComponent("1024.png"))
    } else {
        try write(icon, size: size, name: "\(size).png")
    }
}

// The menu bar uses the same white square, not a black/tinted template variant.
try write(icon, size: 44, name: "trayTemplate.png")
