import AppKit

guard CommandLine.arguments.count == 3 else {
    fputs("usage: render-app-icon.swift <foreground.png> <output.png>\n", stderr)
    exit(2)
}

let foregroundURL = URL(fileURLWithPath: CommandLine.arguments[1])
let outputURL = URL(fileURLWithPath: CommandLine.arguments[2])
let size = 1024

guard
    let foreground = NSImage(contentsOf: foregroundURL),
    let bitmap = NSBitmapImageRep(
        bitmapDataPlanes: nil,
        pixelsWide: size,
        pixelsHigh: size,
        bitsPerSample: 8,
        samplesPerPixel: 4,
        hasAlpha: true,
        isPlanar: false,
        colorSpaceName: .deviceRGB,
        bytesPerRow: 0,
        bitsPerPixel: 0
    )
else {
    fputs("unable to load the icon foreground or create the canvas\n", stderr)
    exit(1)
}

NSGraphicsContext.saveGraphicsState()
guard let context = NSGraphicsContext(bitmapImageRep: bitmap) else {
    fputs("unable to create the drawing context\n", stderr)
    exit(1)
}
NSGraphicsContext.current = context
context.imageInterpolation = .high

NSColor.clear.setFill()
NSRect(x: 0, y: 0, width: size, height: size).fill()

let tileRect = NSRect(x: 42, y: 54, width: 940, height: 940)
let tile = NSBezierPath(roundedRect: tileRect, xRadius: 214, yRadius: 214)
let shadow = NSShadow()
shadow.shadowColor = NSColor(calibratedWhite: 0.08, alpha: 0.16)
shadow.shadowBlurRadius = 18
shadow.shadowOffset = NSSize(width: 0, height: -11)
shadow.set()
NSColor(calibratedRed: 0.969, green: 0.957, blue: 0.922, alpha: 1).setFill()
tile.fill()

NSGraphicsContext.restoreGraphicsState()
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = context
NSColor(calibratedRed: 0.82, green: 0.81, blue: 0.77, alpha: 1).setStroke()
tile.lineWidth = 2
tile.stroke()

foreground.draw(
    in: NSRect(x: 0, y: 0, width: size, height: size),
    from: NSRect(origin: .zero, size: foreground.size),
    operation: .sourceOver,
    fraction: 1
)

context.flushGraphics()
NSGraphicsContext.restoreGraphicsState()

guard let data = bitmap.representation(using: .png, properties: [:]) else {
    fputs("unable to encode the icon as PNG\n", stderr)
    exit(1)
}

try data.write(to: outputURL, options: .atomic)
