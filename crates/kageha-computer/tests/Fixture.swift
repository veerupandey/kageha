// Disposable AppKit surface for the native backend's opt-in integration test.
import AppKit

let app = NSApplication.shared
app.setActivationPolicy(.regular)
let window = NSWindow(
  contentRect: NSRect(x: 100, y: 100, width: 520, height: 220), styleMask: [.titled, .closable],
  backing: .buffered, defer: false)
window.title = "Kageha native integration test"
let first = NSTextField(frame: NSRect(x: 20, y: 150, width: 260, height: 28))
first.stringValue = "initial-one"
first.setAccessibilityLabel("First field")
let second = NSTextField(frame: NSRect(x: 20, y: 105, width: 260, height: 28))
second.stringValue = "initial-two"
second.setAccessibilityLabel("Second field")
window.contentView!.addSubview(first)
window.contentView!.addSubview(second)
if CommandLine.arguments.contains("--performance-check") {
  let scroll = NSScrollView(frame: NSRect(x: 300, y: 20, width: 200, height: 180))
  let content = NSView(frame: NSRect(x: 0, y: 0, width: 180, height: 3600))
  for index in 0..<120 {
    let field = NSTextField(frame: NSRect(x: 0, y: index * 30, width: 170, height: 26))
    field.stringValue = "Sample \(index)"
    field.setAccessibilityLabel("Benchmark field \(index)")
    content.addSubview(field)
  }
  scroll.documentView = content
  scroll.hasVerticalScroller = true
  window.contentView!.addSubview(scroll)
}
window.makeKeyAndOrderFront(nil)
app.activate(ignoringOtherApps: true)
print(ProcessInfo.processInfo.processIdentifier)
fflush(stdout)
if CommandLine.arguments.contains("--focus-check") {
  let other = NSWindow(
    contentRect: NSRect(x: 700, y: 120, width: 420, height: 220), styleMask: [.titled, .closable],
    backing: .buffered, defer: false)
  other.title = "Native focus second window"
  let otherField = NSTextField(frame: NSRect(x: 20, y: 100, width: 260, height: 28))
  otherField.stringValue = "other-window"
  other.contentView!.addSubview(otherField)
  other.orderFront(nil)
  let sheet = NSPanel(
    contentRect: NSRect(x: 0, y: 0, width: 360, height: 140), styleMask: [.titled],
    backing: .buffered, defer: false)
  sheet.title = "Native focus attached sheet"
  let sheetField = NSTextField(frame: NSRect(x: 20, y: 50, width: 260, height: 28))
  sheetField.stringValue = "sheet-window"
  sheet.contentView!.addSubview(sheetField)
  DispatchQueue.global().async {
    while let line = readLine() {
      DispatchQueue.main.async {
        switch line {
        case "first":
          window.makeKeyAndOrderFront(nil)
          window.makeFirstResponder(first)
        case "second":
          other.makeKeyAndOrderFront(nil)
          other.makeFirstResponder(otherField)
        case "sheet":
          window.makeKeyAndOrderFront(nil)
          window.beginSheet(sheet)
          sheet.makeFirstResponder(sheetField)
        case "close_sheet":
          window.endSheet(sheet)
          sheet.orderOut(nil)
          window.makeKeyAndOrderFront(nil)
          window.makeFirstResponder(first)
        default: break
        }
        app.activate(ignoringOtherApps: true)
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.2) {
          let message: [String: Any] = [
            "command": line, "first": window.windowNumber,
            "second": other.windowNumber, "sheet": sheet.windowNumber,
          ]
          let data = try! JSONSerialization.data(withJSONObject: message)
          print(String(data: data, encoding: .utf8)!)
          fflush(stdout)
        }
      }
    }
  }
}
app.run()
