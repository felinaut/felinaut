// Native developer-only picker. stdout is a small protocol for the Python launcher.
import AppKit

func fail(_ message: String) -> Never {
    FileHandle.standardError.write(Data((message + "\n").utf8))
    exit(2)
}

let arguments = Array(CommandLine.arguments.dropFirst())
guard arguments.first == "--supported-modes", (2...4).contains(arguments.count) else {
    fail("Expected --supported-modes followed by built comparison modes")
}
let supplied = Array(arguments.dropFirst())
let supported = Set(supplied)
let known: Set<String> = ["system", "upstream", "both"]
guard supported.count == supplied.count, supported.isSubset(of: known),
      supported.contains("both") == (supported.contains("system") && supported.contains("upstream")) else {
    fail("Invalid, duplicate or inconsistent supported comparison modes")
}

let app = NSApplication.shared
app.setActivationPolicy(.accessory)
NSRunningApplication.current.activate(options: [])

let alert = NSAlert()
alert.messageText = "Compare Cocoa WebKit"
alert.icon = NSImage(systemSymbolName: "globe", accessibilityDescription: "WebKit runtime comparison")
alert.informativeText = "Choose a runtime included in this build. Disabled choices were excluded at build time. Upstream also requires trusted, separately prepared WebKit products.\n\nEach window uses a temporary private home, a nonpersistent page store and a trusted local fixture. This is a developer comparison, not WPE, a performance benchmark or an app update. No signatures or security settings are changed.\n\nClose the comparison windows to finish. Temporary homes are removed after browser roots stop; WebKit helper lifecycle remains unverified."
alert.alertStyle = .informational
let choices = ["system", "upstream", "both", "cancel"]
let titles = ["System WebKit", "Upstream (Experimental)", "Both", "Cancel"]
for (mode, title) in zip(choices, titles) {
    let enabled = mode == "cancel" || supported.contains(mode)
    let button = alert.addButton(withTitle: title)
    button.isEnabled = enabled
    button.keyEquivalent = ""
}
if let index = choices.firstIndex(where: { supported.contains($0) }) {
    alert.buttons[index].keyEquivalent = "\r"
}
alert.buttons.last?.keyEquivalent = "\u{1b}"
let choice = alert.runModal().rawValue - NSApplication.ModalResponse.alertFirstButtonReturn.rawValue
if choice >= 0 && choice < choices.count {
    let selected = choices[choice]
    guard selected == "cancel" || supported.contains(selected) else {
        fail("Picker returned a disabled comparison mode")
    }
    print(selected)
} else {
    print("cancel")
}
