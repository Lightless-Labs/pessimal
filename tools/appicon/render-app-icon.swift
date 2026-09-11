// Renders Pessimal's app icon at every size the asset catalog declares.
//
//     swift tools/appicon/render-app-icon.swift <output-directory>
//
// The icon is generated rather than drawn by hand so it stays reproducible: the file in the repo can
// be rebuilt and diffed, and changing the design means changing code rather than a binary nobody can
// read. CoreGraphics only -- no dependencies, no design tool.
//
// The mark is a heartbeat that stops: one sharp spike, then a flat line to the right edge. That is
// exactly what the app is for, it survives being scaled to 20 points, and it needs no text.

import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

// Every size iOS asks for, as pixel dimensions. Declared explicitly rather than left to actool to
// infer from a single 1024, because App Store validation names exact pixel sizes (120 for iPhone and
// 152 for iPad among them) and a missing one is a rejection after a full build and upload.
//
// No @1x iPad sizes: actool rejects them outright above a deployment target of iOS 10 --
// "76x76@1x app icons only apply to iPad apps targeting releases of iOS prior to 10.0" -- and this app
// starts at 17.0. That is why 20, 29 and 76 pixels are absent while 40, 58 and 152 are present.
let sizes: [Int] = [40, 58, 60, 80, 87, 120, 152, 167, 180, 1024]

let background = (r: 0.071, g: 0.094, b: 0.137)   // deep slate, near-black but not black
let line       = (r: 0.984, g: 0.749, b: 0.290)   // amber: a warning, not an error
let flat       = (r: 0.392, g: 0.447, b: 0.553)   // muted steel for the part that has flatlined

func render(pixels: Int) -> CGImage? {
    let space = CGColorSpace(name: CGColorSpace.sRGB)!
    guard let context = CGContext(
        data: nil, width: pixels, height: pixels, bitsPerComponent: 8, bytesPerRow: 0,
        // No alpha, deliberately: App Store validation rejects a 1024 marketing icon that carries an
        // alpha channel, and an app icon has no use for one at any size.
        space: space, bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue
    ) else { return nil }

    let side = CGFloat(pixels)
    context.setFillColor(red: background.r, green: background.g, blue: background.b, alpha: 1)
    context.fill(CGRect(x: 0, y: 0, width: side, height: side))

    // One coordinate system for every size: fractions of the icon's side, so the mark is identical
    // at 20 pixels and at 1024 and the only difference is resolution.
    func point(_ x: CGFloat, _ y: CGFloat) -> CGPoint { CGPoint(x: x * side, y: y * side) }

    let width = max(side * 0.075, 1.5)
    context.setLineCap(.round)
    context.setLineJoin(.round)
    context.setLineWidth(width)

    // The live part: flat, one spike up, one overshoot down, back to the baseline.
    context.setStrokeColor(red: line.r, green: line.g, blue: line.b, alpha: 1)
    context.beginPath()
    context.move(to: point(0.12, 0.5))
    context.addLine(to: point(0.28, 0.5))
    context.addLine(to: point(0.36, 0.78))
    context.addLine(to: point(0.44, 0.24))
    context.addLine(to: point(0.50, 0.5))
    context.strokePath()

    // The part that stopped: the same baseline continuing, dimmed, to the edge.
    context.setStrokeColor(red: flat.r, green: flat.g, blue: flat.b, alpha: 1)
    context.beginPath()
    context.move(to: point(0.50, 0.5))
    context.addLine(to: point(0.88, 0.5))
    context.strokePath()

    return context.makeImage()
}

guard CommandLine.arguments.count == 2 else {
    FileHandle.standardError.write("usage: render-app-icon.swift <output-directory>\n".data(using: .utf8)!)
    exit(2)
}
let outputDirectory = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
try FileManager.default.createDirectory(at: outputDirectory, withIntermediateDirectories: true)

for pixels in sizes {
    guard let image = render(pixels: pixels) else {
        FileHandle.standardError.write("could not render \(pixels)px\n".data(using: .utf8)!)
        exit(1)
    }
    let url = outputDirectory.appendingPathComponent("icon-\(pixels).png")
    guard let destination = CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil) else {
        FileHandle.standardError.write("could not create \(url.path)\n".data(using: .utf8)!)
        exit(1)
    }
    CGImageDestinationAddImage(destination, image, nil)
    guard CGImageDestinationFinalize(destination) else {
        FileHandle.standardError.write("could not write \(url.path)\n".data(using: .utf8)!)
        exit(1)
    }
    print("wrote icon-\(pixels).png")
}
