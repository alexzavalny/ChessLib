import AppKit
let source = URL(fileURLWithPath: CommandLine.arguments[1])
let destination = CommandLine.arguments[2]
guard let original = NSImage(contentsOf: source) else {
    throw CocoaError(.fileReadCorruptFile)
}
// Fit the bookcloth tile closely while retaining its transparent rounded corners.
let tile = NSRect(x: 24, y: 25, width: 1188, height: 1188)
let temporary = URL(fileURLWithPath: NSTemporaryDirectory()).appendingPathComponent(UUID().uuidString + ".iconset")
try FileManager.default.createDirectory(at: temporary, withIntermediateDirectories: true)
defer { try? FileManager.default.removeItem(at: temporary) }
for size in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let pixels = size * scale
        let image = NSImage(size: NSSize(width: pixels, height: pixels))
        image.lockFocus()
        NSGraphicsContext.current?.imageInterpolation = .high
        original.draw(in: NSRect(origin: .zero, size: image.size), from: tile,
                      operation: .sourceOver, fraction: 1)
        image.unlockFocus()
        let bitmap = NSBitmapImageRep(data: image.tiffRepresentation!)!
        let name = "icon_\(size)x\(size)\(scale == 2 ? "@2x" : "").png"
        try bitmap.representation(using: .png, properties: [:])!.write(to: temporary.appendingPathComponent(name))
    }
}
let process = Process()
process.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
process.arguments = ["-c", "icns", temporary.path, "-o", destination]
try process.run()
process.waitUntilExit()
if process.terminationStatus != 0 { exit(process.terminationStatus) }
