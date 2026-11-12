#!/usr/bin/env swift
import AppKit
import Foundation
import Darwin

// Build-time rasterization only; the source artwork remains SVG.
enum ArtError: Error, CustomStringConvertible {
    case message(String)
    var description: String {
        switch self { case .message(let text): return text }
    }
}

func color(_ hex: UInt32, alpha: CGFloat = 1) -> NSColor {
    NSColor(srgbRed: CGFloat((hex >> 16) & 255) / 255,
            green: CGFloat((hex >> 8) & 255) / 255,
            blue: CGFloat(hex & 255) / 255, alpha: alpha)
}

let purple = color(0x383353)
let fuchsia = color(0xFF72B6)
let amber = color(0xFFB45E)
let aqua = color(0x78D9EF)

// All drawing is unflipped. Only layout measurements use a top-origin convention.
func topRect(_ x: CGFloat, _ top: CGFloat, _ width: CGFloat,
             _ height: CGFloat, canvasHeight: CGFloat) -> NSRect {
    NSRect(x: x, y: canvasHeight - top - height, width: width, height: height)
}

func gradient(_ colors: [NSColor]) throws -> NSGradient {
    guard let result = NSGradient(colors: colors) else {
        throw ArtError.message("Could not create an artwork gradient.")
    }
    return result
}

func loadSVG(_ path: String) throws -> NSImage {
    guard let image = NSImage(contentsOfFile: path), image.isValid,
          image.size.width > 0, image.size.height > 0 else {
        throw ArtError.message("Could not load SVG artwork at '\(path)'. Expected an AppKit-readable SVG file.")
    }
    return image
}

func drawImage(_ image: NSImage, in rect: NSRect) {
    image.draw(in: rect, from: .zero, operation: .sourceOver,
               fraction: 1, respectFlipped: false, hints: [.interpolation: NSImageInterpolation.high])
}

func text(_ string: String, in rect: NSRect, font: NSFont,
          color: NSColor, alignment: NSTextAlignment = .left) {
    let paragraph = NSMutableParagraphStyle()
    paragraph.alignment = alignment
    paragraph.lineBreakMode = .byTruncatingTail
    NSAttributedString(string: string, attributes: [
        .font: font, .foregroundColor: color, .paragraphStyle: paragraph
    ]).draw(in: rect)
}

func star(at center: NSPoint, radius: CGFloat, color: NSColor) {
    let path = NSBezierPath()
    for index in 0..<8 {
        let angle = CGFloat(index) * .pi / 4
        let length = index.isMultiple(of: 2) ? radius : radius * 0.29
        let point = NSPoint(x: center.x + sin(angle) * length,
                            y: center.y + cos(angle) * length)
        if index == 0 { path.move(to: point) } else { path.line(to: point) }
    }
    path.close()
    color.setFill()
    path.fill()
}

func stroke(_ path: NSBezierPath, color: NSColor, width: CGFloat) {
    path.lineWidth = width
    path.lineCapStyle = .round
    path.lineJoinStyle = .round
    color.setStroke()
    path.stroke()
}

func renderPNG(to url: URL, pixels: NSSize, logicalSize: NSSize,
               draw: () throws -> Void) throws {
    guard let bitmap = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: Int(pixels.width), pixelsHigh: Int(pixels.height),
        bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
        colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0
    ), let context = NSGraphicsContext(bitmapImageRep: bitmap) else {
        throw ArtError.message("Could not allocate bitmap for '\(url.path)'.")
    }
    NSGraphicsContext.saveGraphicsState()
    defer { NSGraphicsContext.restoreGraphicsState() }
    NSGraphicsContext.current = context
    context.imageInterpolation = .high
    context.shouldAntialias = true
    context.cgContext.clear(CGRect(origin: .zero, size: pixels))
    context.cgContext.scaleBy(x: pixels.width / logicalSize.width,
                             y: pixels.height / logicalSize.height)
    try draw()
    context.cgContext.flush()
    // PNG pHYs metadata derives from pixels / points: 2x is 144 DPI, not 72.
    bitmap.size = logicalSize
    guard let png = bitmap.representation(using: .png, properties: [:]) else {
        throw ArtError.message("Could not encode PNG for '\(url.path)'.")
    }
    do { try png.write(to: url, options: .atomic) }
    catch { throw ArtError.message("Could not write PNG to '\(url.path)': \(error.localizedDescription)") }
}

func drawYarnPlanet(at center: NSPoint, radius: CGFloat, tint: NSColor) throws {
    let sphere = NSBezierPath(ovalIn: NSRect(x: center.x - radius, y: center.y - radius,
                                            width: radius * 2, height: radius * 2))
    let thread = NSBezierPath()
    thread.move(to: NSPoint(x: center.x + radius * 0.6, y: center.y - radius * 0.7))
    thread.curve(to: NSPoint(x: center.x + radius * 1.8, y: center.y - radius * 1.1),
                 controlPoint1: NSPoint(x: center.x + radius * 2, y: center.y - radius * 0.6),
                 controlPoint2: NSPoint(x: center.x + radius, y: center.y - radius * 1.9))
    stroke(thread, color: tint, width: radius * 0.07)
    do {
        NSGraphicsContext.saveGraphicsState()
        defer { NSGraphicsContext.restoreGraphicsState() }
        sphere.addClip()
        try gradient([color(0xF7F6FF), tint, purple]).draw(in: sphere, angle: -55)
        // Two curved crossing families wrap around a shaded sphere, not a flat striped disc.
        for direction: CGFloat in [-1, 1] {
            for offset: CGFloat in [-0.65, -0.25, 0.15, 0.55] {
                let strand = NSBezierPath()
                strand.move(to: NSPoint(x: center.x - radius, y: center.y + radius * offset))
                strand.curve(to: NSPoint(x: center.x + radius, y: center.y - radius * offset),
                             controlPoint1: NSPoint(x: center.x - radius * 0.45,
                                                    y: center.y + radius * direction),
                             controlPoint2: NSPoint(x: center.x + radius * 0.45,
                                                    y: center.y + radius * direction))
                stroke(strand, color: color(0xF7F6FF, alpha: 0.65), width: radius * 0.055)
            }
        }
    }
    stroke(sphere, color: purple.withAlphaComponent(0.65), width: radius * 0.045)
}

func drawAppIcon(symbol: NSImage) throws {
    let tile = NSBezierPath(roundedRect: NSRect(x: 64, y: 64, width: 896, height: 896),
                            xRadius: 200, yRadius: 200)
    NSGraphicsContext.saveGraphicsState()
    defer { NSGraphicsContext.restoreGraphicsState() }
    tile.addClip()
    try gradient([color(0x0B1026), color(0x131B36), color(0x1D2948)])
        .draw(in: tile, angle: 70)
    try gradient([color(0xB3A0FF, alpha: 0.38), color(0xB3A0FF, alpha: 0)])
        .draw(fromCenter: NSPoint(x: 515, y: 545), radius: 12,
              toCenter: NSPoint(x: 515, y: 545), radius: 440, options: [.drawsBeforeStartingLocation])

    let ribbon = NSBezierPath()
    ribbon.move(to: NSPoint(x: 90, y: 285))
    ribbon.curve(to: NSPoint(x: 921, y: 266), controlPoint1: NSPoint(x: 304, y: 81),
                 controlPoint2: NSPoint(x: 699, y: 118))
    stroke(ribbon, color: color(0xBE64F4, alpha: 0.22), width: 30)
    let glint = NSBezierPath()
    glint.move(to: NSPoint(x: 130, y: 739))
    glint.curve(to: NSPoint(x: 361, y: 871), controlPoint1: NSPoint(x: 194, y: 818),
                controlPoint2: NSPoint(x: 269, y: 855))
    stroke(glint, color: aqua.withAlphaComponent(0.65), width: 7)

    // The canonical helmet stays readable at small sizes; no miniature full-body scene.
    drawImage(symbol, in: NSRect(x: 182, y: 168, width: 660, height: 660))
    try drawYarnPlanet(at: NSPoint(x: 819, y: 776), radius: 48, tint: fuchsia)
    star(at: NSPoint(x: 754, y: 862), radius: 18, color: amber)
    star(at: NSPoint(x: 214, y: 369), radius: 13, color: aqua)
    star(at: NSPoint(x: 842, y: 457), radius: 12, color: fuchsia)
    let rim = NSBezierPath(roundedRect: NSRect(x: 66, y: 66, width: 892, height: 892),
                           xRadius: 198, yRadius: 198)
    stroke(rim, color: color(0xE4CFFF, alpha: 0.18), width: 3)
}

func drawDMG(mascot: NSImage, symbol: NSImage, version: String) throws {
    let height: CGFloat = 480
    func rect(_ x: CGFloat, _ top: CGFloat, _ w: CGFloat, _ h: CGFloat) -> NSRect {
        topRect(x, top, w, h, canvasHeight: height)
    }
    // Light moon-dust surfaces keep Finder's black filename labels legible.
    try gradient([color(0xECEBFF), color(0xF7F6FF)])
        .draw(in: NSRect(x: 0, y: 0, width: 720, height: height), angle: 90)
    drawImage(mascot, in: rect(30, 15, 170, 127.5))
    guard let wordmarkFont = NSFont(name: "Georgia-BoldItalic", size: 46) else {
        throw ArtError.message("Required macOS font Georgia-BoldItalic is unavailable.")
    }
    text("Felinaut", in: rect(235, 35, 440, 58), font: wordmarkFont, color: purple)
    try drawYarnPlanet(at: NSPoint(x: 646, y: 418), radius: 29, tint: fuchsia)
    star(at: NSPoint(x: 594, y: 449), radius: 7, color: color(0x8C72CD))
    star(at: NSPoint(x: 688, y: 372), radius: 5, color: color(0x438DA0))
    drawImage(symbol, in: rect(235, 98, 22, 22))
    text(version, in: rect(265, 99, 410, 24),
         font: .systemFont(ofSize: 14, weight: .semibold), color: purple)
    text("A little curiosity. A whole universe.", in: rect(235, 129, 440, 25),
         font: .systemFont(ofSize: 14), color: color(0x655B81))

    // Finder supplies the actual icons and black filename labels. Keep these zones quiet.
    for x: CGFloat in [190, 530] {
        let circle = NSBezierPath(ovalIn: rect(x - 65, 180, 130, 130))
        color(0xFFFFFF, alpha: 0.55).setFill()
        circle.fill()
        stroke(circle, color: color(0xDFD3EE, alpha: 0.7), width: 1)
    }
    let arrow = NSBezierPath()
    arrow.move(to: NSPoint(x: 330, y: height - 245))
    arrow.curve(to: NSPoint(x: 390, y: height - 245),
                controlPoint1: NSPoint(x: 350, y: height - 238),
                controlPoint2: NSPoint(x: 370, y: height - 238))
    stroke(arrow, color: purple.withAlphaComponent(0.75), width: 2.5)
    let tip = NSBezierPath()
    tip.move(to: NSPoint(x: 380, y: height - 236))
    tip.line(to: NSPoint(x: 390, y: height - 245))
    tip.line(to: NSPoint(x: 378, y: height - 251))
    stroke(tip, color: purple.withAlphaComponent(0.75), width: 2.5)

    text("Drag Felinaut into Applications", in: rect(40, 365, 640, 30),
         font: .systemFont(ofSize: 22, weight: .semibold), color: purple, alignment: .center)
    text("Then open it and explore.", in: rect(40, 402, 640, 24),
         font: .systemFont(ofSize: 14), color: color(0x655B81), alignment: .center)
    try drawYarnPlanet(at: NSPoint(x: 54, y: 73), radius: 23, tint: aqua)

    // Ribbons stay below the instructions and well clear of Finder's filename labels.
    for (offset, tint, width): (CGFloat, NSColor, CGFloat) in [
        (0, color(0xC4A2EB, alpha: 0.65), 13),
        (9, fuchsia.withAlphaComponent(0.48), 6),
        (18, amber.withAlphaComponent(0.8), 7),
        (26, aqua.withAlphaComponent(0.65), 5)
    ] {
        let path = NSBezierPath()
        path.move(to: NSPoint(x: -25, y: 10 + offset))
        path.curve(to: NSPoint(x: 745, y: 4 + offset),
                   controlPoint1: NSPoint(x: 185, y: 61 + offset),
                   controlPoint2: NSPoint(x: 470, y: -47 + offset))
        stroke(path, color: tint, width: width)
    }
}

func main() throws {
    guard CommandLine.arguments.count == 5 else {
        throw ArtError.message("Usage: swift scripts/render_macos_art.swift MASCOT_SVG SYMBOL_SVG OUTPUT_DIR DISPLAY_VERSION")
    }
    let args = CommandLine.arguments
    let mascot = try loadSVG(args[1])
    let symbol = try loadSVG(args[2])
    let output = URL(fileURLWithPath: args[3], isDirectory: true)
    do { try FileManager.default.createDirectory(at: output, withIntermediateDirectories: true) }
    catch { throw ArtError.message("Could not create output directory '\(output.path)': \(error.localizedDescription)") }
    try renderPNG(to: output.appendingPathComponent("app-icon.png"),
                  pixels: NSSize(width: 1024, height: 1024),
                  logicalSize: NSSize(width: 1024, height: 1024)) {
        try drawAppIcon(symbol: symbol)
    }
    try renderPNG(to: output.appendingPathComponent("dmg-background.png"),
                  pixels: NSSize(width: 1440, height: 960),
                  logicalSize: NSSize(width: 720, height: 480)) {
        try drawDMG(mascot: mascot, symbol: symbol, version: args[4])
    }
    print("Rendered app-icon.png (1024×1024) and dmg-background.png (1440×960, 720×480 points) in \(output.path)")
}

autoreleasepool {
    do { try main() }
    catch {
        FileHandle.standardError.write(Data("render_macos_art: \(error)\n".utf8))
        exit(EXIT_FAILURE)
    }
}
