// Owned benchmark surface. Setup/oracle commands never complete benchmark tasks.
import AppKit
import Carbon
let app = NSApplication.shared
app.setActivationPolicy(.regular)
// Standard responder-chain commands must have menu equivalents, as they do
// in real document apps. Without an Edit menu, cmd+A is not a Select All task.
let menu = NSMenu()
let editRoot = NSMenuItem(title:"Edit",action:nil,keyEquivalent:"")
let editMenu = NSMenu(title:"Edit")
editMenu.addItem(withTitle:"Select All",action:#selector(NSText.selectAll(_:)),keyEquivalent:"a")
editRoot.submenu=editMenu
menu.addItem(editRoot)
app.mainMenu=menu
let output = URL(fileURLWithPath: CommandLine.arguments[1])
let seed = Int(CommandLine.arguments[2])!
class FixtureWindow: NSWindow {
  override func sendEvent(_ event: NSEvent) {
    if [.leftMouseDown,.leftMouseUp,.rightMouseDown,.rightMouseUp].contains(event.type) {
      let point=event.locationInWindow
      let record:[String:Any] = ["window":windowNumber,"type":event.type.rawValue,"x":point.x,"y":point.y]
      if let data=try? JSONSerialization.data(withJSONObject:record), let line=String(data:data,encoding:.utf8) {
        let target=output.deletingLastPathComponent().appendingPathComponent("mouse-events.jsonl")
        let prior=(try? String(contentsOf:target,encoding:.utf8)) ?? ""
        try? (prior+line+"\n").write(to:target,atomically:true,encoding:.utf8)
      }
    }
    super.sendEvent(event)
  }
}
let window = FixtureWindow(contentRect:NSRect(x:80 + seed % 60,y:80,width:650,height:400),styleMask:[.titled,.closable,.resizable],backing:.buffered,defer:false)
let other = FixtureWindow(contentRect:NSRect(x:120 + seed % 60,y:120,width:650,height:400),styleMask:[.titled,.closable,.resizable],backing:.buffered,defer:false)
window.title = "Runtime benchmark primary \(seed)"
other.title = "Runtime benchmark secondary \(seed)"
func field(_ label:String,_ x:CGFloat,_ y:CGFloat)->NSTextField {
  let value = NSTextField(frame:NSRect(x:x,y:y,width:280,height:28))
  value.setAccessibilityLabel(label)
  return value
}
let primary = field("Primary field",20,340)
let secondary = field("Primary field",20,340)
let disabled = field("Disabled field",20,290)
disabled.isEnabled = false
let secure = NSSecureTextField(frame:NSRect(x:20,y:240,width:280,height:28))
secure.setAccessibilityLabel("Owned secure field")
class Events:NSObject {
  var primaryClicks = 0
  var secondaryClicks = 0
  var disabledClicks = 0
  var saves = 0
  var delayedClicks = 0
  @objc func delayedAction(_ sender:Any?) {
    // Model a responsive app whose AX action takes longer than a snapshot read.
    Thread.sleep(forTimeInterval:0.25)
    delayedClicks += 1
  }
  @objc func disabledAction(_ sender:Any?) { disabledClicks += 1 }
  @objc func saveDocument(_ sender:Any?) {
    guard primary.stringValue.hasPrefix("doc-"), checkbox.state == .on, radioB.state == .on else { return }
    let document:[String:Any] = ["title":primary.stringValue,"enabled":true,"option":"B"]
    if let data = try? JSONSerialization.data(withJSONObject:document,options:[.sortedKeys]) {
      do { try data.write(to:output.deletingLastPathComponent().appendingPathComponent("saved.json"),options:.atomic); saves += 1 } catch {}
    }
  }
  @objc func incrementPrimary(_ sender:Any?) { primaryClicks += 1 }
  @objc func incrementSecondary(_ sender:Any?) { secondaryClicks += 1 }
  @objc func changed(_ sender:Any?) {
    if let button = sender as? NSButton {
      if button === radioA { radioA.state = .on; radioB.state = .off }
      if button === radioB { radioB.state = .on; radioA.state = .off }
    }
  }
}
let events = Events()
let button = NSButton(title:"Increment once",target:events,action:#selector(Events.incrementPrimary(_:)))
let otherButton = NSButton(title:"Increment once",target:events,action:#selector(Events.incrementSecondary(_:)))
button.frame = NSRect(x:330,y:340,width:180,height:28)
otherButton.frame = button.frame
let disabledButton = NSButton(title:"Disabled button",target:events,action:#selector(Events.disabledAction(_:)))
disabledButton.frame = NSRect(x:330,y:290,width:180,height:28)
disabledButton.isEnabled = false
let saveButton = NSButton(title:"Save document",target:events,action:#selector(Events.saveDocument(_:)))
saveButton.frame = NSRect(x:330,y:240,width:180,height:28)
let delayedButton = NSButton(title:"Delayed action",target:events,action:#selector(Events.delayedAction(_:)))
delayedButton.frame = NSRect(x:330,y:185,width:180,height:28)
let checkbox = NSButton(checkboxWithTitle:"Enable setting",target:events,action:#selector(Events.changed(_:)))
checkbox.frame = NSRect(x:20,y:185,width:240,height:28)
let radioA = NSButton(radioButtonWithTitle:"Option A",target:events,action:#selector(Events.changed(_:)))
let radioB = NSButton(radioButtonWithTitle:"Option B",target:events,action:#selector(Events.changed(_:)))
radioA.frame = NSRect(x:20,y:140,width:160,height:28)
radioB.frame = NSRect(x:200,y:140,width:160,height:28)
window.contentView!.addSubview(primary)
window.contentView!.addSubview(disabled)
window.contentView!.addSubview(secure)
window.contentView!.addSubview(button)
window.contentView!.addSubview(disabledButton)
window.contentView!.addSubview(saveButton)
window.contentView!.addSubview(delayedButton)
window.contentView!.addSubview(checkbox)
window.contentView!.addSubview(radioA)
window.contentView!.addSubview(radioB)
other.contentView!.addSubview(secondary)
other.contentView!.addSubview(otherButton)
var version = 0
var ready = false
func persist() {
  version += 1
  let state:[String:Any] = ["pid":getpid(),"seed":seed,"version":version,"ready":ready,
    "primary_window":window.windowNumber,"secondary_window":other.windowNumber,
    "primary":primary.stringValue,"secondary":secondary.stringValue,
    "primary_visible":(primary.currentEditor() as? NSTextView)?.string ?? primary.stringValue,
    "selected_length":(primary.currentEditor() as? NSTextView)?.selectedRange().length ?? 0,
    "primary_focused":primary.currentEditor() != nil && window.firstResponder === primary.currentEditor(),
    "primary_owner":primary.window?.windowNumber ?? -1,"button_owner":button.window?.windowNumber ?? -1,
    "disabled_clicks":events.disabledClicks,"saves":events.saves,"delayed_clicks":events.delayedClicks,
    "secure":secure.stringValue,"disabled":disabled.stringValue,
    "primary_clicks":events.primaryClicks,"secondary_clicks":events.secondaryClicks,
    "checkbox":checkbox.state.rawValue,"radio_a":radioA.state.rawValue,"radio_b":radioB.state.rawValue]
  if let data = try? JSONSerialization.data(withJSONObject:state,options:[.sortedKeys]) {
    try? data.write(to:output,options:.atomic)
  }
}
func reset() {
  window.endEditing(for:nil)
  other.endEditing(for:nil)
  if primary.superview !== window.contentView { primary.removeFromSuperview(); window.contentView!.addSubview(primary) }
  if button.superview !== window.contentView { button.removeFromSuperview(); window.contentView!.addSubview(button) }
  primary.frame = NSRect(x:CGFloat(20 + seed % 45),y:340,width:280,height:28)
  primary.stringValue = "initial-primary"
  secondary.stringValue = "initial-secondary"
  disabled.stringValue = "unchanged-disabled"
  secure.stringValue = ""
  checkbox.state = .off
  radioA.state = .on
  radioB.state = .off
  events.primaryClicks = 0
  events.secondaryClicks = 0
  events.disabledClicks = 0
  events.saves = 0
  events.delayedClicks = 0
  try? FileManager.default.removeItem(at:output.deletingLastPathComponent().appendingPathComponent("saved.json"))
  other.orderFront(nil)
  window.makeKeyAndOrderFront(nil)
  window.makeFirstResponder(button)
  app.activate(ignoringOtherApps:true)
  persist()
}
reset()
Timer.scheduledTimer(withTimeInterval:0.025,repeats:true) { _ in persist() }
DispatchQueue.global().async {
  while let line = readLine() {
    DispatchQueue.main.async {
      switch line {
      case "secure_on": _ = EnableSecureEventInput()
      case "secure_off": _ = DisableSecureEventInput()
      case "reset": reset()
      case "reparent": primary.removeFromSuperview(); other.contentView!.addSubview(primary)
      case "reparent_button": button.removeFromSuperview(); other.contentView!.addSubview(button)
      case "secondary": other.makeKeyAndOrderFront(nil); other.makeFirstResponder(otherButton)
      case "move": window.setFrameOrigin(NSPoint(x:window.frame.minX + 37,y:window.frame.minY + 11))
      default: break
      }
      persist()
      print(line)
      fflush(stdout)
    }
  }
  // A killed benchmark worker closes stdin; never leave an orphan GUI behind.
  DispatchQueue.main.async { app.terminate(nil) }
}
DispatchQueue.main.asyncAfter(deadline:.now()+0.35) { ready = true; persist() }
app.run()
