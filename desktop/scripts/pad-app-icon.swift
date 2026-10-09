import AppKit
import Foundation

guard CommandLine.arguments.count >= 3, CommandLine.arguments.count <= 4 else {
    fputs("usage: pad-app-icon.swift <input.png> <output.png> [scale]\n", stderr)
    exit(2)
}

let inputURL = URL(fileURLWithPath: CommandLine.arguments[1])
let outputURL = URL(fileURLWithPath: CommandLine.arguments[2])
let scale = CommandLine.arguments.count == 4 ? (Double(CommandLine.arguments[3]) ?? 0.86) : 0.86
guard scale > 0 && scale <= 1 else {
    fputs("scale must be greater than 0 and no greater than 1\n", stderr)
    exit(2)
}

let canvasSize = 1024
let inset = CGFloat(canvasSize) * (1 - CGFloat(scale)) / 2

guard
    let image = NSImage(contentsOf: inputURL),
    let bitmap = NSBitmapImageRep(
        bitmapDataPlanes: nil,
        pixelsWide: canvasSize,
        pixelsHigh: canvasSize,
        bitsPerSample: 8,
        samplesPerPixel: 4,
        hasAlpha: true,
        isPlanar: false,
        colorSpaceName: .deviceRGB,
        bytesPerRow: 0,
        bitsPerPixel: 0
    ),
    let context = NSGraphicsContext(bitmapImageRep: bitmap)
else {
    fputs("unable to load the source icon or create the canvas\n", stderr)
    exit(1)
}

NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = context
context.imageInterpolation = .high
NSColor.clear.setFill()
NSRect(x: 0, y: 0, width: canvasSize, height: canvasSize).fill()

image.draw(
    in: NSRect(x: inset, y: inset, width: CGFloat(canvasSize) * CGFloat(scale), height: CGFloat(canvasSize) * CGFloat(scale)),
    from: NSRect(origin: .zero, size: image.size),
    operation: .sourceOver,
    fraction: 1
)

context.flushGraphics()
NSGraphicsContext.restoreGraphicsState()

guard let data = bitmap.representation(using: .png, properties: [:]) else {
    fputs("unable to encode the padded icon as PNG\n", stderr)
    exit(1)
}

do {
    try data.write(to: outputURL, options: .atomic)
} catch {
    fputs("unable to write the padded icon: \(error)\n", stderr)
    exit(1)
}
