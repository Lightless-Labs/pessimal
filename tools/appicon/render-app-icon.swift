// Renders Pessimal's app icon at every size the iOS asset catalog declares, or as a macOS iconset.
//
//     swift tools/appicon/render-app-icon.swift <output-directory>            # iOS, icon-<pixels>.png
//     swift tools/appicon/render-app-icon.swift --macos <output-directory>    # macOS .iconset for iconutil
//
// The two differ in shape, not in the mark. iOS masks the icon itself, so an iOS icon fills its
// canvas and carries no alpha. macOS does not mask anything: a Mac icon draws its own rounded square
// on a transparent canvas, and one that fills the canvas instead looks larger than every other icon
// in the Dock and in Finder. The rounded square here is 824 points across a 1024 canvas with a
// corner radius of 185.4, which is Apple's own macOS icon grid.
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

// What `iconutil -c icns` expects, by name. Each pair is one PNG: the file name and its pixel side.
// Both members of a @1x/@2x pair at the same pixel size are written, because iconutil reads the name
// rather than the size, and a missing name makes it refuse the whole iconset.
let macOSIcons: [(name: String, pixels: Int)] = [
    ("icon_16x16", 16), ("icon_16x16@2x", 32),
    ("icon_32x32", 32), ("icon_32x32@2x", 64),
    ("icon_128x128", 128), ("icon_128x128@2x", 256),
    ("icon_256x256", 256), ("icon_256x256@2x", 512),
    ("icon_512x512", 512), ("icon_512x512@2x", 1024),
]

let background = (r: 0.071, g: 0.094, b: 0.137)   // deep slate, near-black but not black
let line       = (r: 0.984, g: 0.749, b: 0.290)   // amber: a warning, not an error
let flat       = (r: 0.392, g: 0.447, b: 0.553)   // muted steel for the part that has flatlined

enum Shape {
    /// Fills the canvas, no alpha. iOS masks the icon itself.
    case fullBleed
    /// Apple's macOS grid: a rounded square inset in a transparent canvas.
    case roundedSquare
}

func render(pixels: Int, shape: Shape = .fullBleed) -> CGImage? {
    let space = CGColorSpace(name: CGColorSpace.sRGB)!
    guard let context = CGContext(
        data: nil, width: pixels, height: pixels, bitsPerComponent: 8, bytesPerRow: 0,
        // iOS carries no alpha, deliberately: App Store validation rejects a 1024 marketing icon that
        // has an alpha channel. A macOS icon needs one, for the margin around its rounded square.
        space: space,
        bitmapInfo: (shape == .fullBleed ? CGImageAlphaInfo.noneSkipLast : CGImageAlphaInfo.premultipliedLast).rawValue
    ) else { return nil }

    let canvas = CGFloat(pixels)
    // The mark is laid out in fractions of the square it is drawn in, not of the canvas, so it is
    // identical on both platforms and at every size; only the square's size and origin differ.
    let side: CGFloat
    let origin: CGPoint
    switch shape {
    case .fullBleed:
        side = canvas
        origin = .zero
    case .roundedSquare:
        side = canvas * 824.0 / 1024.0
        origin = CGPoint(x: (canvas - side) / 2, y: (canvas - side) / 2)
    }

    let square = CGRect(origin: origin, size: CGSize(width: side, height: side))
    context.setFillColor(red: background.r, green: background.g, blue: background.b, alpha: 1)
    switch shape {
    case .fullBleed:
        context.fill(square)
    case .roundedSquare:
        let path = CGPath(roundedRect: square, cornerWidth: side * 185.4 / 824.0,
                          cornerHeight: side * 185.4 / 824.0, transform: nil)
        context.addPath(path)
        context.fillPath()
        // Everything after this is clipped to the rounded square, so no stroke can spill into the
        // margin at a size where the line is wide relative to the icon.
        context.addPath(path)
        context.clip()
    }

    func point(_ x: CGFloat, _ y: CGFloat) -> CGPoint {
        CGPoint(x: origin.x + x * side, y: origin.y + y * side)
    }

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

var arguments = Array(CommandLine.arguments.dropFirst())
let renderMacOS = arguments.first == "--macos"
if renderMacOS { arguments.removeFirst() }

guard arguments.count == 1 else {
    FileHandle.standardError.write("usage: render-app-icon.swift [--macos] <output-directory>\n".data(using: .utf8)!)
    exit(2)
}
let outputDirectory = URL(fileURLWithPath: arguments[0], isDirectory: true)
try FileManager.default.createDirectory(at: outputDirectory, withIntermediateDirectories: true)

func write(_ image: CGImage, named name: String) {
    let url = outputDirectory.appendingPathComponent("\(name).png")
    guard let destination = CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil) else {
        FileHandle.standardError.write("could not create \(url.path)\n".data(using: .utf8)!)
        exit(1)
    }
    CGImageDestinationAddImage(destination, image, nil)
    guard CGImageDestinationFinalize(destination) else {
        FileHandle.standardError.write("could not write \(url.path)\n".data(using: .utf8)!)
        exit(1)
    }
    print("wrote \(name).png")
}

let requested: [(name: String, pixels: Int, shape: Shape)] = renderMacOS
    ? macOSIcons.map { (name: $0.name, pixels: $0.pixels, shape: Shape.roundedSquare) }
    : sizes.map { (name: "icon-\($0)", pixels: $0, shape: Shape.fullBleed) }

for icon in requested {
    guard let image = render(pixels: icon.pixels, shape: icon.shape) else {
        FileHandle.standardError.write("could not render \(icon.pixels)px\n".data(using: .utf8)!)
        exit(1)
    }
    write(image, named: icon.name)
}
