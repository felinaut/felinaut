// Select only the launched browser's largest on-screen, normal-level window.
import AppKit
import CoreGraphics

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data((message + "\n").utf8))
    exit(1)
}

guard CommandLine.arguments.count == 2,
      let pid = Int32(CommandLine.arguments[1]), pid > 0,
      let app = NSRunningApplication(processIdentifier: pid), !app.isTerminated else {
    fail("Expected the PID of a running browser process")
}
app.activate(options: [])
let windows = CGWindowListCopyWindowInfo(
    [.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID
) as? [[String: Any]] ?? []
let own = windows.filter {
    ($0[kCGWindowOwnerPID as String] as? Int32) == pid &&
    ($0[kCGWindowLayer as String] as? Int) == 0
}
func area(_ window: [String: Any]) -> Double {
    let bounds = window[kCGWindowBounds as String] as? [String: Double] ?? [:]
    return (bounds["Width"] ?? 0) * (bounds["Height"] ?? 0)
}
if let window = own.filter({ area($0) > 0 }).max(by: { area($0) < area($1) }),
   let number = window[kCGWindowNumber as String] as? Int, number > 0 {
    print(number)
} else {
    fail("No on-screen browser window for PID \(pid); a macOS desktop session is required")
}
