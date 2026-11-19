import AppKit
import ApplicationServices
// Owned platform bridge using Apple's public APIs only. Rust owns transport,
// framing and helper lifetime. No child process is launched for an operation.
import Foundation
import ImageIO
import ScreenCaptureKit
import UniformTypeIdentifiers

struct NativeFailure: Error {
  let message: String
  var executionPhase: String = "unknown"
}
func fail(_ message: String) throws -> Never { throw NativeFailure(message: message) }
func integer(_ args: [String: Any], _ name: String) throws -> Int {
  guard let n = args[name] as? NSNumber, CFGetTypeID(n) != CFBooleanGetTypeID(),
    n.doubleValue.isFinite,
    n.doubleValue.rounded() == n.doubleValue
  else { try fail("\(name) must be an integer") }
  let maximum = name == "pid" ? Double(Int32.max) : Double(UInt32.max)
  guard n.doubleValue > 0, n.doubleValue <= maximum else {
    try fail("\(name) is outside the native identifier range")
  }
  return n.intValue
}
func finite(_ args: [String: Any], _ name: String) throws -> Double {
  guard let n = args[name] as? NSNumber, CFGetTypeID(n) != CFBooleanGetTypeID(),
    n.doubleValue.isFinite
  else { try fail("\(name) must be finite") }
  return n.doubleValue
}
func string(_ args: [String: Any], _ name: String) throws -> String {
  guard let s = args[name] as? String else { try fail("\(name) must be text") }
  return s
}
func attr(_ e: AXUIElement, _ name: String) -> CFTypeRef? {
  var value: CFTypeRef?
  return AXUIElementCopyAttributeValue(e, name as CFString, &value) == .success ? value : nil
}
func axString(_ e: AXUIElement, _ name: String) -> String { return attr(e, name) as? String ?? "" }
func axPoint(_ e: AXUIElement) -> CGPoint? { return pointValue(attr(e, kAXPositionAttribute)) }
func pointValue(_ value: CFTypeRef?) -> CGPoint? {
  guard let raw = value, CFGetTypeID(raw) == AXValueGetTypeID() else {
    return nil
  }
  var p = CGPoint.zero
  return AXValueGetValue(raw as! AXValue, .cgPoint, &p) ? p : nil
}
func axSize(_ e: AXUIElement) -> CGSize? { return sizeValue(attr(e, kAXSizeAttribute)) }
func sizeValue(_ value: CFTypeRef?) -> CGSize? {
  guard let raw = value, CFGetTypeID(raw) == AXValueGetTypeID() else {
    return nil
  }
  var p = CGSize.zero
  return AXValueGetValue(raw as! AXValue, .cgSize, &p) ? p : nil
}
func bounds(_ r: CGRect) -> [String: Any] {
  return [
    "x": Double(r.minX), "y": Double(r.minY), "width": Double(r.width), "height": Double(r.height),
  ]
}
func windowRows(pid: Int? = nil) -> [[String: Any]] {
  let raw =
    CGWindowListCopyWindowInfo([.optionAll, .excludeDesktopElements], kCGNullWindowID)
    as? [[String: Any]] ?? []
  return raw.compactMap { row in
    guard let owner = row[kCGWindowOwnerPID as String] as? Int,
      pid == nil || owner == pid,
      row[kCGWindowLayer as String] as? Int == 0,
      let wid = row[kCGWindowNumber as String] as? Int,
      let b = row[kCGWindowBounds as String] as? [String: Any],
      let rect = CGRect(dictionaryRepresentation: b as CFDictionary), rect.width > 1,
      rect.height > 1
    else { return nil }
    return [
      "window_id": wid, "pid": owner, "title": row[kCGWindowName as String] as? String ?? "",
      "bounds": bounds(rect), "is_on_screen": row[kCGWindowIsOnscreen as String] as? Bool ?? false,
    ]
  }
}
func windowRect(_ row: [String: Any]) -> CGRect {
  let b = row["bounds"] as! [String: Any]
  return CGRect(
    x: b["x"] as! Double, y: b["y"] as! Double, width: b["width"] as! Double,
    height: b["height"] as! Double)
}
func exactWindow(_ pid: Int, _ wid: Int) throws -> [String: Any] {
  guard let row = windowRows(pid: pid).first(where: { $0["window_id"] as? Int == wid }) else {
    try fail("target window no longer belongs to the requested pid")
  }
  return row
}
func axElement(_ e: AXUIElement, _ name: String) -> AXUIElement? {
  guard let value = attr(e, name), CFGetTypeID(value) == AXUIElementGetTypeID() else { return nil }
  return (value as! AXUIElement)
}
// The JSONL helper does not run an AppKit event loop. NSWorkspace's cached
// frontmost application and NSRunningApplication.isActive can therefore stay
// stale after another app activates. Ask Accessibility for the live owner.
func focusedApplication() -> AXUIElement? {
  guard AXIsProcessTrusted() else { return nil }
  let system = AXUIElementCreateSystemWide()
  AXUIElementSetMessagingTimeout(system, 0.15)
  return axElement(system, kAXFocusedApplicationAttribute)
}
func axPID(_ e: AXUIElement?) -> Int? {
  guard let e = e else { return nil }
  var pid: pid_t = 0
  return AXUIElementGetPid(e, &pid) == .success && pid > 0 ? Int(pid) : nil
}
func containingWindow(_ e: AXUIElement) -> AXUIElement? {
  var current: AXUIElement? = e
  var visited: [AXUIElement] = []
  for _ in 0..<30 {
    guard let node = current, !visited.contains(where: { CFEqual($0, node) }) else { return nil }
    visited.append(node)
    let role = axString(node, kAXRoleAttribute)
    // The nearest container matters: an attached sheet must not be treated
    // as permission to send a key to its underlying application window.
    if role == kAXSheetRole || role == kAXWindowRole { return node }
    if role == kAXApplicationRole { break }
    current = axElement(node, kAXParentAttribute)
  }
  return axElement(e, kAXWindowAttribute)
}
struct LiveFocus {
  let pid: Int?
  let window: AXUIElement?
  let element: AXUIElement?
  init() {
    let app = focusedApplication()
    pid = axPID(app)
    if let app = app {
      AXUIElementSetMessagingTimeout(app, 0.15)
      element = axElement(app, kAXFocusedUIElementAttribute)
      window = element.flatMap { containingWindow($0) } ?? axElement(app, kAXFocusedWindowAttribute)
    } else {
      element = nil
      window = nil
    }
  }
  func matches(_ pid: Int, _ requested: AXUIElement) -> Bool {
    return self.pid == pid && window.map { CFEqual($0, requested) } == true
  }
  func diagnostics(_ pid: Int, _ requested: AXUIElement?) -> [String: Any] {
    var result: [String: Any] = [
      "requested_window_focused": requested.map { matches(pid, $0) } ?? false,
      "focus_available": self.pid != nil && window != nil,
    ]
    if let owner = self.pid { result["focused_pid"] = owner }
    if let window = window {
      result["focused_window_title"] = axString(window, kAXTitleAttribute)
      result["focused_window_role"] = axString(window, kAXRoleAttribute)
    }
    return result
  }
}
func axWindow(_ pid: Int, _ row: [String: Any]) throws -> AXUIElement {
  guard AXIsProcessTrusted() else {
    try fail(
      "Accessibility permission is not granted to this native helper; no prompt was requested")
  }
  let app = AXUIElementCreateApplication(pid_t(pid))
  AXUIElementSetMessagingTimeout(app, 0.15)
  var pending = (attr(app, kAXWindowsAttribute) as? [AXUIElement] ?? []).map { ($0, 0) }
  if let focused = axElement(app, kAXFocusedWindowAttribute) { pending.append((focused, 0)) }
  let deadline = Date().addingTimeInterval(0.5)
  var candidates: [AXUIElement] = []
  // Sheets need not appear in AXWindows. Enumerate only window/sheet edges,
  // not arbitrary controls, with bounded depth, count, time and exact ownership.
  while !pending.isEmpty {
    guard candidates.count < 64, Date() < deadline else {
      try fail("AX window/sheet inventory incomplete; refusing another window")
    }
    let (node, depth) = pending.removeLast()
    if candidates.contains(where: { CFEqual($0, node) }) { continue }
    guard axPID(node) == pid else { continue }
    AXUIElementSetMessagingTimeout(node, 0.1)
    candidates.append(node)
    if depth < 4 {
      let children = attr(node, kAXChildrenAttribute) as? [AXUIElement] ?? []
      guard children.count <= 64 else { try fail("AX window/sheet inventory incomplete") }
      pending += children.filter {
        [kAXSheetRole, kAXWindowRole].contains(axString($0, kAXRoleAttribute))
      }.map { ($0, depth + 1) }
    }
  }
  // An exhausted search cannot establish uniqueness among unseen candidates.
  guard pending.isEmpty else {
    try fail("AX window/sheet inventory incomplete; refusing another window")
  }
  let target = windowRect(row)
  let identities = candidates.map { e in
    let rect = axPoint(e).flatMap { point in axSize(e).map { CGRect(origin: point, size: $0) } }
    // Some applications expose a native number through the public AX attribute
    // query API. Missing attributes fall back to unique process + geometry.
    return WindowIdentity(pid: axPID(e) ?? -1, nativeID: (attr(e, "AXWindowNumber") as? NSNumber)?.intValue, bounds: rect)
  }
  let identity = WindowIdentity(pid: pid, nativeID: row["window_id"] as? Int, bounds: target)
  guard let index = matchingWindow(identity, identities) else {
    let geometry = candidates.map { e in
      "\(axString(e, kAXRoleAttribute)): \(String(describing: axPoint(e))) / \(String(describing: axSize(e)))"
    }.joined(separator: "; ")
    try fail(
      "exact AX window/sheet is unavailable or ambiguous (candidates: \(candidates.count), requested: \(target), inventory: \(geometry)); refusing another window"
    )
  }
  return candidates[index]
}
struct Snapshot {
  let id: String
  let pid: Int
  let wid: Int
  let elements: [AXUIElement]
  let rows: [[String: Any]]
  let capture: CGSize?
  let rect: CGRect
}
var snapshots: [String: Snapshot] = [:]
var snapshotOrder: [String] = []
var executingBatch = false
var shareable: SCShareableContent?
var shareableAt = Date.distantPast
func key(_ session: String, _ pid: Int, _ wid: Int) -> String { return "\(session):\(pid):\(wid)" }
func session(_ args: [String: Any]) -> String { return args["session"] as? String ?? "default" }
func putSnapshot(_ k: String, _ snap: Snapshot) {
  snapshots[k] = snap
  snapshotOrder.removeAll { $0 == k }
  snapshotOrder.append(k)
  while snapshotOrder.count > 32 { snapshots.removeValue(forKey: snapshotOrder.removeFirst()) }
}
func getContent(force: Bool = false) throws -> SCShareableContent {
  if !force, let cached = shareable, Date().timeIntervalSince(shareableAt) < 2 { return cached }
  let semaphore = DispatchSemaphore(value: 0)
  var result: SCShareableContent?
  var failure: Error?
  SCShareableContent.getExcludingDesktopWindows(true, onScreenWindowsOnly: false) {
    content, error in
    result = content
    failure = error
    semaphore.signal()
  }
  guard semaphore.wait(timeout: .now() + 5) == .success else {
    try fail("ScreenCaptureKit window inventory timed out")
  }
  if let error = failure {
    try fail("ScreenCaptureKit inventory failed: \(error.localizedDescription)")
  }
  guard let content = result else { try fail("ScreenCaptureKit inventory returned no content") }
  shareable = content
  shareableAt = Date()
  return content
}
func screenshot(_ pid: Int, _ wid: Int, _ args: [String: Any]) throws -> (String, CGSize) {
  guard CGPreflightScreenCaptureAccess() else {
    try fail(
      "Screen Recording permission is not granted to this native helper; no prompt was requested")
  }
  var content = try getContent()
  let liveRect = windowRect(try exactWindow(pid, wid))
  if !content.windows.contains(where: {
    $0.windowID == CGWindowID(wid) && $0.owningApplication?.processID == pid_t(pid)
      && $0.frame == liveRect
  }) {
    content = try getContent(force: true)
  }
  guard
    let window = content.windows.first(where: {
      $0.windowID == CGWindowID(wid) && $0.owningApplication?.processID == pid_t(pid)
    })
  else { try fail("window absent from ScreenCaptureKit inventory") }
  let filter = SCContentFilter(desktopIndependentWindow: window)
  let config = SCStreamConfiguration()
  let scale = Double(filter.pointPixelScale)
  let requested = (args["max_image_dimension"] as? Int) ?? 1920
  let maximum =
    requested > 0 ? Double(requested) : max(window.frame.width, window.frame.height) * scale
  let ratio = min(1, maximum / max(window.frame.width * scale, window.frame.height * scale))
  config.width = max(1, Int((window.frame.width * scale * ratio).rounded()))
  config.height = max(1, Int((window.frame.height * scale * ratio).rounded()))
  config.showsCursor = false
  config.ignoreShadowsSingleWindow = true
  let semaphore = DispatchSemaphore(value: 0)
  var result: CGImage?
  var failure: Error?
  SCScreenshotManager.captureImage(contentFilter: filter, configuration: config) { image, error in
    result = image
    failure = error
    semaphore.signal()
  }
  guard semaphore.wait(timeout: .now() + 5) == .success else {
    try fail("ScreenCaptureKit screenshot timed out")
  }
  if let error = failure {
    try fail("ScreenCaptureKit screenshot failed: \(error.localizedDescription)")
  }
  guard let image = result else { try fail("ScreenCaptureKit returned no screenshot") }
  let format = args["image_format"] as? String ?? "png"
  guard ["png", "jpeg"].contains(format) else { try fail("image_format must be png or jpeg") }
  let output =
    (args["screenshot_out_file"] as? String) ?? NSTemporaryDirectory()
    + "kageha-native-\(getpid())-\(wid).\(format == "jpeg" ? "jpg" : "png")"
  let url = URL(fileURLWithPath: output)
  try FileManager.default.createDirectory(
    at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
  guard
    let dest = CGImageDestinationCreateWithURL(
      url as CFURL, (format == "jpeg" ? UTType.jpeg : UTType.png).identifier as CFString, 1, nil)
  else { try fail("cannot create screenshot destination") }
  let properties: CFDictionary? =
    format == "jpeg"
    ? [kCGImageDestinationLossyCompressionQuality: 0.9] as CFDictionary : nil
  CGImageDestinationAddImage(dest, image, properties)
  guard CGImageDestinationFinalize(dest) else { try fail("cannot encode screenshot") }
  return (output, CGSize(width: image.width, height: image.height))
}
func walk(_ root: AXUIElement, _ cap: Int, _ deadline: Date, _ sid: String) -> (
  [AXUIElement], [[String: Any]], Bool
) {
  var elements: [AXUIElement] = []
  var rows: [[String: Any]] = []
  var truncated = false
  var pending: [(AXUIElement, Int, Int?)] = [(root, 0, nil)]
  var visited: [CFHashCode: [AXUIElement]] = [:]
  let names =
    [
      kAXRoleAttribute, kAXTitleAttribute, kAXDescriptionAttribute, kAXValueAttribute,
      kAXEnabledAttribute, kAXSubroleAttribute, kAXHelpAttribute, kAXFocusedAttribute,
      kAXPositionAttribute, kAXSizeAttribute, kAXChildrenAttribute,
    ] as CFArray
  while let (element, depth, parent) = pending.popLast() {
    if elements.count >= cap || Date() > deadline {
      truncated = true
      break
    }
    let identity = CFHash(element)
    if (visited[identity] ?? []).contains(where: { CFEqual($0, element) }) { continue }
    visited[identity, default: []].append(element)
    AXUIElementSetMessagingTimeout(element, 0.1)
    var raw: CFArray?
    let success = AXUIElementCopyMultipleAttributeValues(element, names, [], &raw) == .success
    let values = success ? (raw as? [Any] ?? []) : []
    func value(_ index: Int) -> Any? { return values.count > index ? values[index] : nil }
    let role = value(0) as? String ?? axString(element, kAXRoleAttribute)
    let subrole = value(5) as? String ?? ""
    let secure = role.lowercased().contains("secure") || subrole.lowercased().contains("secure")
    let label = (value(1) as? String).flatMap { $0.isEmpty ? nil : $0 } ?? value(2) as? String ?? ""
    let index = elements.count
    var row: [String: Any] = [
      "element_index": index, "element_token": "\(sid):\(index)", "role": role,
      "label": secure ? "[secure control]" : label, "depth": depth,
    ]
    if let parent = parent { row["parent_index"] = parent }
    if let enabled = value(4) as? Bool { row["enabled"] = enabled }
    if let focused = value(7) as? Bool { row["focused"] = focused }
    var settable = DarwinBoolean(false)
    if AXUIElementIsAttributeSettable(element, kAXValueAttribute as CFString, &settable) == .success
    {
      row["settable"] = settable.boolValue
    }
    if !secure, let value = value(3), value is String || value is NSNumber { row["value"] = value }
    let position = success ? pointValue(value(8) as CFTypeRef?) : axPoint(element)
    let dimensions = success ? sizeValue(value(9) as CFTypeRef?) : axSize(element)
    if let position = position, let dimensions = dimensions {
      row["bounds"] = bounds(CGRect(origin: position, size: dimensions))
    }
    var actions: CFArray?
    if AXUIElementCopyActionNames(element, &actions) == .success {
      row["actions"] = actions as? [String] ?? []
    }
    elements.append(element)
    rows.append(row)
    let children =
      success
      ? value(10) as? [AXUIElement] : attr(element, kAXChildrenAttribute) as? [AXUIElement]
    if depth < 25, let children = children {
      for child in children.reversed() { pending.append((child, depth + 1, index)) }
    }
  }
  return (elements, rows, truncated)
}
func state(_ args: [String: Any]) throws -> [String: Any] {
  let started = Date()
  let pid = try integer(args, "pid")
  var wid = try integer(args, "window_id")
  var selectedRow = try? exactWindow(pid, wid)
  let focus = LiveFocus()
  let app = AXUIElementCreateApplication(pid_t(pid))
  let elementWindow = axElement(app, kAXFocusedUIElementAttribute).flatMap { containingWindow($0) }
  let appWindow =
    elementWindow.map { axString($0, kAXRoleAttribute) == kAXSheetRole } == true
    ? elementWindow : (axElement(app, kAXFocusedWindowAttribute) ?? elementWindow)
  if args["follow_focus"] as? Bool == true,
    let focused = appWindow, let point = axPoint(focused), let size = axSize(focused)
  {
    let title = axString(focused, kAXTitleAttribute)
    let matches = windowRows(pid: pid).filter { candidate in
      let rect = windowRect(candidate)
      return abs(rect.minX - point.x) < 2 && abs(rect.minY - point.y) < 2
        && abs(rect.width - size.width) < 2 && abs(rect.height - size.height) < 2
        && ((candidate["title"] as? String ?? "").isEmpty || title.isEmpty
          || candidate["title"] as? String == title)
    }
    if matches.count == 1, let selected = matches.first {
      selectedRow = selected
      wid = selected["window_id"] as! Int
    }
  }
  guard let row = selectedRow else {
    try fail("target window no longer belongs to the requested pid")
  }
  var timings: [String: Double] = ["resolve_ms": Date().timeIntervalSince(started) * 1000]
  let focusStarted = Date()
  let ax = args["include_accessibility_tree"] as? Bool ?? true
  let pixels = (args["include_screenshot"] as? Bool ?? true) || args["screenshot_out_file"] != nil
  guard ax || pixels else { try fail("observation must request AX or pixels") }
  let sid = try string(args, "native_snapshot_id")
  let k = key(session(args), pid, wid)
  if ax { snapshots.removeValue(forKey: k) }
  var output: [String: Any] = [
    "ok": true, "pid": pid, "window_id": wid, "bounds": row["bounds"]!,
    "window_title": row["title"]!, "snapshot_id": sid,
  ]
  var elements: [AXUIElement] = []
  var rows: [[String: Any]] = []
  let requestedWindow = ax ? try axWindow(pid, row) : nil
  if let window = requestedWindow, (output["window_title"] as? String ?? "").isEmpty {
    // CGWindow titles may be redacted without Screen Recording permission.
    // We already resolved this exact AX window; its public title is available.
    output["window_title"] = axString(window, kAXTitleAttribute)
  }
  if ax { output["focus"] = focus.diagnostics(pid, requestedWindow) }
  timings["focus_ms"] = Date().timeIntervalSince(focusStarted) * 1000
  let axStarted = Date()
  if ax, let window = requestedWindow {
    // Omit unsupported attributes: absence must never imply a saved document.
    if let edited = attr(window, kAXEditedAttribute),
      CFGetTypeID(edited) == CFBooleanGetTypeID()
    {
      output["document_edited"] = edited as! Bool
    }
    let cap = max(1, min(2000, (args["max_elements"] as? Int) ?? 400))
    let timeout = max(100, min(5000, (args["timeout_ms"] as? Int) ?? 650))
    var truncated = false
    (elements, rows, truncated) = walk(
      window, cap, Date().addingTimeInterval(Double(timeout) / 1000), sid)
    output["elements"] = rows
    output["truncated"] = truncated
    output["elements_complete"] = !truncated

  }
  timings["ax_ms"] = Date().timeIntervalSince(axStarted) * 1000
  var capture: CGSize?
  let pixelsStarted = Date()
  if pixels {
    let (path, size) = try screenshot(pid, wid, args)
    output["screenshot_file_path"] = path
    output["screenshot_width"] = size.width
    output["screenshot_height"] = size.height
    capture = size
  }
  timings["pixels_ms"] = Date().timeIntervalSince(pixelsStarted) * 1000
  if ax {
    putSnapshot(
      k,
      Snapshot(
        id: sid, pid: pid, wid: wid, elements: elements, rows: rows, capture: capture,
        rect: windowRect(row)))
  } else if let existing = snapshots[k] {
    putSnapshot(
      k,
      Snapshot(
        id: existing.id, pid: pid, wid: wid, elements: existing.elements, rows: existing.rows,
        capture: capture, rect: windowRect(row)))
  } else {
    putSnapshot(
      k,
      Snapshot(
        id: sid, pid: pid, wid: wid, elements: [], rows: [], capture: capture, rect: windowRect(row)
      ))
  }
  timings["native_ms"] = Date().timeIntervalSince(started) * 1000
  output["timing"] = timings
  return output
}
func targetElement(_ args: [String: Any]) throws -> AXUIElement {
  let pid = try integer(args, "pid")
  let wid = try integer(args, "window_id")
  let k = key(session(args), pid, wid)
  guard let snapshot = snapshots[k] else {
    try fail("stale observation: observe this window first")
  }
  var index: Int
  if let token = args["element_token"] as? String {
    let prefix = snapshot.id + ":"
    guard token.hasPrefix(prefix), let parsed = Int(token.dropFirst(prefix.count)) else {
      try fail("stale element token")
    }
    index = parsed
    if let supplied = args["element_index"] as? Int, supplied != index {
      try fail("element token/index mismatch")
    }
  } else {
    try fail("element_token is required for native indexed actions")
  }
  guard snapshot.elements.indices.contains(index) else { try fail("element token is absent") }
  _ = try exactWindow(pid, wid)
  let element = snapshot.elements[index]
  guard attr(element, kAXEnabledAttribute) as? Bool != false else { try fail("observed control is disabled") }
  return element
}
func requireNativeText(_ e: AXUIElement) throws {
  var current: AXUIElement? = e
  for _ in 0..<30 {
    guard let node = current else { try fail("text target ancestry is unavailable") }
    let role = axString(node, kAXRoleAttribute)
    if role == "AXWebArea" {
      try fail("web-content AX writes cannot prove renderer input; use the browser backend")
    }
    if role.lowercased().contains("secure")
      || axString(node, kAXSubroleAttribute).lowercased().contains("secure")
    {
      try fail("secure text fields require human input")
    }
    if role == "AXApplication" { return }
    if let parent = attr(node, kAXParentAttribute), CFGetTypeID(parent) == AXUIElementGetTypeID() {
      current = (parent as! AXUIElement)
    } else {
      try fail("text target ancestry is ambiguous")
    }
  }
  try fail("text target ancestry is ambiguous")
}
func performed(_ code: AXError) throws {
  guard code == .success else { try fail("accessibility action failed with code \(code.rawValue)") }
}
func performNativeMutation(_ element: AXUIElement, _ send: () -> AXError) throws {
  try checkIntervention()
  // Snapshot traversal uses 100 ms read timeouts. AXPress includes application
  // processing/animation and must not inherit that much shorter read budget.
  // Configure once, send once, and keep any in-flight error uncertain.
  guard AXUIElementSetMessagingTimeout(element, 1.0) == .success else {
    throw NativeFailure(message: "cannot configure native action timeout", executionPhase: "not_dispatched")
  }
  defer { AXUIElementSetMessagingTimeout(element, 0.1) }
  try performed(send())
}
func coordinate(_ args: [String: Any], _ x: String = "x", _ y: String = "y") throws -> CGPoint {
  let pid = try integer(args, "pid")
  let wid = try integer(args, "window_id")
  let row = try exactWindow(pid, wid)
  let rect = windowRect(row)
  guard row["is_on_screen"] as? Bool == true else {
    try fail("coordinate input requires an on-screen window")
  }
  guard let snap = snapshots[key(session(args), pid, wid)], let size = snap.capture else {
    try fail("coordinate input requires a screenshot observation")
  }
  guard snap.rect == rect else {
    try fail("window geometry changed; observe again before coordinate input")
  }
  let px = try finite(args, x)
  let py = try finite(args, y)
  guard px >= 0, py >= 0, px < Double(size.width), py < Double(size.height) else {
    try fail("coordinates are outside the observed window")
  }
  return CGPoint(
    x: rect.minX + px * rect.width / size.width, y: rect.minY + py * rect.height / size.height)
}
// Keep synthetic foreground modifiers/buttons separate from physical input.
let controlledEventSource = CGEventSource(stateID: .privateState)

// Foreground delivery is an explicit mode, never a fallback for failed PID
// delivery. Check focus and the topmost window at each mouse-event point.
func requireForegroundPoint(_ point: CGPoint, _ pid: Int, _ wid: Int) throws {
  let row = try exactWindow(pid, wid)
  guard LiveFocus().matches(pid, try axWindow(pid, row)) else { try fail("foreground mouse input requires the exact focused window") }
  guard let rows = CGWindowListCopyWindowInfo([.optionOnScreenOnly,.excludeDesktopElements],kCGNullWindowID) as? [[String:Any]] else { try fail("window visibility unavailable") }
  for candidate in rows {
    guard (candidate[kCGWindowAlpha as String] as? Double ?? 1) > 0,
      let raw=candidate[kCGWindowBounds as String] as? [String:Any],
      let rect=CGRect(dictionaryRepresentation:raw as CFDictionary), rect.contains(point) else { continue }
    guard (candidate[kCGWindowOwnerPID as String] as? Int)==pid,
      (candidate[kCGWindowNumber as String] as? Int)==wid else {try fail("another window obscures the requested mouse point")}
    return
  }
  try fail("requested mouse point is not visible")
}
func postMouse(_ type: CGEventType, _ point: CGPoint, _ pid: Int, _ wid: Int, _ button: CGMouseButton = .left, _ count: Int = 1, _ foreground: Bool = false) throws {
  let releasing = type == .leftMouseUp || type == .rightMouseUp
  // Release an already-started foreground gesture even if focus/window changes.
  if !releasing {try checkIntervention()}
  if !(foreground && releasing) {
    _ = try exactWindow(pid, wid)
    if foreground { try requireForegroundPoint(point,pid,wid) }
  }
  guard let event = CGEvent(mouseEventSource: foreground ? controlledEventSource : nil, mouseType: type,
    mouseCursorPosition: point, mouseButton: button) else { try fail("cannot create window-bound mouse event") }
  event.flags = []
  event.setIntegerValueField(.eventSourceUnixProcessID,value:Int64(getpid()))
  event.setIntegerValueField(.mouseEventClickState, value: Int64(count))
  event.setIntegerValueField(.eventTargetUnixProcessID, value: Int64(pid))
  event.setIntegerValueField(.mouseEventWindowUnderMousePointer, value: Int64(wid))
  event.setIntegerValueField(.mouseEventWindowUnderMousePointerThatCanHandleThisEvent, value: Int64(wid))
  if foreground { event.post(tap:.cghidEventTap) } else { event.postToPid(pid_t(pid)) }
}
let nativeKeyCodes: [String: CGKeyCode] = [
  "return": 36, "enter": 36, "tab": 48, "space": 49, "escape": 53, "backspace": 51,
  "delete": 117, "left": 123, "right": 124, "down": 125, "up": 126, "home": 115, "end": 119,
  "pageup": 116, "pagedown": 121, "a": 0, "s": 1, "d": 2, "f": 3, "h": 4, "g": 5, "z": 6,
  "x": 7, "c": 8, "v": 9, "b": 11, "q": 12, "w": 13, "e": 14, "r": 15, "y": 16, "t": 17,
  "1": 18, "2": 19, "3": 20, "4": 21, "6": 22, "5": 23, "9": 25, "7": 26, "8": 28, "0": 29,
  "o": 31, "u": 32, "i": 34, "p": 35, "l": 37, "j": 38, "k": 40, "n": 45, "m": 46,
  "[": 33, "]": 30, "=": 24, "-": 27, "'": 39, ";": 41, "\\": 42,
  ",": 43, "/": 44, ".": 47, "`": 50, "bracketleft": 33, "bracketright": 30,
  "equal": 24, "minus": 27, "plus": 24,
]

func nativeKeyParts(_ input: String) -> [String] {
  var key = input.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
  if key == "+" {
    key = "plus"
  } else if key.hasSuffix("++") {
    key = String(key.dropLast()) + "plus"
  }
  return key.split(separator: "+").map(String.init)
}
func focusableElement(_ args: [String: Any], _ pid: Int, _ row: [String: Any]) throws -> AXUIElement {
  let element = try targetElement(args)
  let requested = try axWindow(pid, row)
  guard LiveFocus().matches(pid, requested) else { try fail("focus_element requires the exact window to be focused first") }
  guard let owner = containingWindow(element), CFEqual(owner, requested) else { try fail("observed control no longer belongs to the requested window") }
  var writable = DarwinBoolean(false)
  guard AXUIElementIsAttributeSettable(element, kAXFocusedAttribute as CFString, &writable) == .success, writable.boolValue else { try fail("element focus is not settable") }
  return element
}
func validateAction(_ method: String, _ args: [String: Any]) throws {
  if let expected=args["activity_revision"] as? String {try HumanInputMonitor.shared.require(expected)}
  guard AXIsProcessTrusted() else {
    try fail("Accessibility permission is not granted to this native helper")
  }
  let pid = try integer(args, "pid")
  let wid = try integer(args, "window_id")
  guard snapshots[key(session(args), pid, wid)] != nil else {
    try fail("stale observation: observe this window first")
  }
  let row = try exactWindow(pid, wid)
  if let mode = args["delivery_mode"] as? String, !["background","foreground"].contains(mode) { try fail("unsupported delivery mode") }
  if args["delivery_mode"] as? String == "foreground", !["click","scroll","drag","press_key"].contains(method) {try fail("foreground delivery supports mouse and key actions only")}
  switch method {
  case "focus_element": _ = try focusableElement(args, pid, row)
  case "focus_window":
    let requested = try axWindow(pid, row)
    let app = AXUIElementCreateApplication(pid_t(pid))
    if let element = axElement(app, kAXFocusedUIElementAttribute),
      let container = containingWindow(element),
      axString(container, kAXRoleAttribute) == kAXSheetRole
        || attr(container, kAXModalAttribute) as? Bool == true,
      !CFEqual(container, requested)
    {
      try fail("an attached sheet blocks the requested window; observe the sheet before focusing")
    }
  case "click":
    let button = args["button"] as? String ?? "left"
    guard button == "left" || button == "right" else { try fail("unsupported click button") }
    if args["element_token"] != nil {
      let element = try targetElement(args)
      var names: CFArray?
      _ = AXUIElementCopyActionNames(element, &names)
      let requested = button == "right" ? kAXShowMenuAction : kAXPressAction
      guard (names as? [String] ?? []).contains(requested) else {
        try fail("requested AX action is absent")
      }
    } else {
      let point = try coordinate(args)
      if args["delivery_mode"] as? String == "foreground" {try requireForegroundPoint(point,pid,wid)}
    }
    let count = (args["count"] as? Int) ?? 1
    guard (1...3).contains(count) else { try fail("invalid click count") }
    if args["element_token"] != nil && count != 1 {
      try fail("multiple clicks require pixel addressing")
    }
  case "set_value":
    let e = try targetElement(args)
    try requireNativeText(e)
    _ = try string(args, "value")
    var writable = DarwinBoolean(false)
    guard AXUIElementIsAttributeSettable(e, kAXValueAttribute as CFString, &writable) == .success,
      writable.boolValue
    else { try fail("value is not settable") }
  case "select_text":
    let e = try targetElement(args)
    try requireNativeText(e)
    let mode = args["mode"] as? String ?? "all"
    guard ["all", "text", "cursor_before", "cursor_after"].contains(mode) else {
      try fail("unsupported text selection mode")
    }
    var writable = DarwinBoolean(false)
    guard
      AXUIElementIsAttributeSettable(e, kAXSelectedTextRangeAttribute as CFString, &writable)
        == .success,
      writable.boolValue
    else { try fail("text selection is not settable") }
  case "type_text":
    _ = try string(args, "text")
    if args["element_token"] != nil {
      try requireNativeText(targetElement(args))
    } else {
      let app = AXUIElementCreateApplication(pid_t(pid))
      guard let focused = attr(app, kAXFocusedUIElementAttribute),
        CFGetTypeID(focused) == AXUIElementGetTypeID()
      else { try fail("focused control unavailable") }
      try requireNativeText(focused as! AXUIElement)
    }
  case "press_key":
    let parts = nativeKeyParts(try string(args, "key"))
    let modifiers = Set(["cmd", "command", "super", "ctrl", "control", "alt", "option", "shift"])
    guard let last = parts.last, nativeKeyCodes[last] != nil,
      parts.dropLast().allSatisfy({ modifiers.contains($0) })
    else { try fail("unsupported key or modifier") }
    let focus = LiveFocus()
    guard focus.matches(pid, try axWindow(pid, row)) else {
      try fail(
        "key input requires the exact focused window/sheet; observe focus diagnostics before further input"
      )
    }
    if args["element_token"] != nil {
      let e = try targetElement(args)
      guard let focused = focus.element, CFEqual(e, focused)
      else { try fail("requested key target is not focused") }
    }
  case "scroll":
    let direction = args["direction"] as? String ?? "down"
    let amount = (args["amount"] as? Int) ?? 3
    guard ["up", "down", "left", "right"].contains(direction), (1...100).contains(amount),
      row["is_on_screen"] as? Bool == true
    else { try fail("invalid or off-screen scroll") }
    guard ["line", "page"].contains(args["by"] as? String ?? "line") else {
      try fail("unsupported scroll unit")
    }
    if args["element_token"] != nil {
      let e = try targetElement(args)
      guard let point = axPoint(e), let size = axSize(e),
        windowRect(row).contains(CGPoint(x: point.x + size.width / 2, y: point.y + size.height / 2))
      else { try fail("scroll target has no visible center") }
    }
  case "drag":
    let from = try coordinate(args, "from_x", "from_y")
    let to = try coordinate(args, "to_x", "to_y")
    if args["delivery_mode"] as? String == "foreground" {
      for n in 0...12 {let t=Double(n)/12;try requireForegroundPoint(CGPoint(x:from.x+(to.x-from.x)*t,y:from.y+(to.y-from.y)*t),pid,wid)}
    }
  case "perform_secondary_action":
    let e = try targetElement(args)
    let requested = try string(args, "action")
    var names: CFArray?
    _ = AXUIElementCopyActionNames(e, &names)
    guard (names as? [String] ?? []).contains(requested) else {
      try fail("secondary action is absent")
    }
  default: try fail("unsupported native action")
  }
}
func prepareAction(_ method: String, _ args: [String: Any]) throws -> (Int, Int, [String: Any]) {
  try validateAction(method, args)
  guard AXIsProcessTrusted() else {
    try fail("Accessibility permission is not granted to this native helper")
  }
  let pid = try integer(args, "pid")
  let wid = try integer(args, "window_id")
  let row = try exactWindow(pid, wid)
  guard snapshots[key(session(args), pid, wid)] != nil else {
    try fail("stale observation: observe this window before input")
  }
  if args["delivery_mode"] as? String == "foreground" {
    guard LiveFocus().matches(pid, try axWindow(pid,row)) else {try fail("focus the exact window before foreground input")}
  }
  return (pid, wid, row)
}
func action(_ method: String, _ args: [String: Any]) throws -> [String: Any] {
  let previousActivity=activeInterventionRevision
  activeInterventionRevision=args["activity_revision"] as? String
  defer {activeInterventionRevision=previousActivity}
  try checkIntervention()
  let prepared: (Int, Int, [String: Any])
  do { prepared = try prepareAction(method, args) }
  catch let failure as NativeFailure {
    // This boundary contains validation and AX reads only. Never tag errors
    // from the actual action switch below as not dispatched.
    throw NativeFailure(message: failure.message, executionPhase: "not_dispatched")
  }
  let (pid, wid, row) = prepared
  defer { if !executingBatch { snapshots.removeValue(forKey: key(session(args), pid, wid)) } }
  switch method {
  case "focus_element":
    let element: AXUIElement
    do { element = try focusableElement(args, pid, row) }
    catch let failure as NativeFailure { throw NativeFailure(message: failure.message, executionPhase: "not_dispatched") }
    try performNativeMutation(element) { AXUIElementSetAttributeValue(element, kAXFocusedAttribute as CFString, kCFBooleanTrue) }
    let requested = try axWindow(pid, row)
    let focus = LiveFocus()
    let verified = focus.matches(pid, requested) && focus.element.map { CFEqual($0, element) } == true
    return ["ok":true,"verified":verified,"effect":verified ? "confirmed" : "unverifiable","focus":focus.diagnostics(pid, requested)]
  case "focus_window":
    let requested = try axWindow(pid, row)
    if !LiveFocus().matches(pid, requested) {
      guard let app = NSRunningApplication(processIdentifier: pid_t(pid)), !app.isTerminated else {
        try fail("target application no longer runs")
      }
      // Focus is an explicit operation, never a fallback for a failed input.
      guard app.activate(options: [.activateIgnoringOtherApps]) else {
        try fail("native activation was refused; no keyboard input was sent")
      }
      _ = AXUIElementSetAttributeValue(
        AXUIElementCreateApplication(pid_t(pid)), kAXFrontmostAttribute as CFString, kCFBooleanTrue)
      _ = AXUIElementSetAttributeValue(requested, kAXMainAttribute as CFString, kCFBooleanTrue)
      _ = AXUIElementSetAttributeValue(requested, kAXFocusedAttribute as CFString, kCFBooleanTrue)
      _ = AXUIElementPerformAction(requested, kAXRaiseAction as CFString)
      let deadline = Date().addingTimeInterval(0.8)
      while Date() < deadline && !LiveFocus().matches(pid, requested) {
        RunLoop.current.run(until: Date().addingTimeInterval(0.02))
      }
    }
    let focus = LiveFocus()
    let verified = focus.matches(pid, requested)
    return [
      "ok": verified, "verified": verified,
      "effect": verified ? "confirmed" : "unverifiable",
      "focus": focus.diagnostics(pid, requested),
      "hint": "Observe again before keyboard input; focus may change after this result.",
    ]

  case "click":
    if args["element_token"] != nil {
      let e = try targetElement(args)
      let button = args["button"] as? String ?? "left"
      guard button == "left" || button == "right" else {
        try fail("middle AX click is unsupported")
      }
      try performNativeMutation(e) {
        AXUIElementPerformAction(
          e, (button == "right" ? kAXShowMenuAction : kAXPressAction) as CFString)
      }
    } else {
      let p = try coordinate(args)
      let button = args["button"] as? String ?? "left"
      guard button == "left" || button == "right" else {
        try fail("middle pixel click is unsupported")
      }
      let count = (args["count"] as? Int) ?? 1
      guard (1...3).contains(count) else { try fail("click count must be 1..3") }
      let foreground = args["delivery_mode"] as? String == "foreground"
      try postMouse(.mouseMoved, p, pid, wid, .left, 1, foreground)
      if foreground {Thread.sleep(forTimeInterval:0.02)}
      for n in 1...count {
        let b: CGMouseButton = button == "right" ? .right : .left
        for type: CGEventType in [
          button == "right" ? .rightMouseDown : .leftMouseDown,
          button == "right" ? .rightMouseUp : .leftMouseUp,
        ] {
          try postMouse(type, p, pid, wid, b, n, foreground)
          if foreground {Thread.sleep(forTimeInterval:0.03)}
        }
      }
    }
  case "perform_secondary_action":
    let e = try targetElement(args)
    let requested = try string(args, "action")
    var names: CFArray?
    _ = AXUIElementCopyActionNames(e, &names)
    guard (names as? [String] ?? []).contains(requested) else {
      try fail("secondary action is absent from this element")
    }
    try performNativeMutation(e) { AXUIElementPerformAction(e, requested as CFString) }
  case "set_value":
    let e = try targetElement(args)
    try requireNativeText(e)
    let text = try string(args, "value")
    try performNativeMutation(e) { AXUIElementSetAttributeValue(e, kAXValueAttribute as CFString, text as CFString) }
    return [
      "ok": true, "effect": axString(e, kAXValueAttribute) == text ? "confirmed" : "unverifiable",
      "verified": axString(e, kAXValueAttribute) == text,
    ]
  case "select_text":
    let e = try targetElement(args)
    let value = axString(e, kAXValueAttribute) as NSString
    let mode = args["mode"] as? String ?? "all"
    var range = CFRange(location: 0, length: value.length)
    if mode != "all" {
      let requested = try string(args, "text")
      guard !requested.isEmpty else { try fail("selection text is required") }
      let prefix = args["prefix"] as? String ?? ""
      let suffix = args["suffix"] as? String ?? ""
      let needle = prefix + requested + suffix
      let found = value.range(of: needle)
      guard found.location != NSNotFound else { try fail("selection text was not found") }
      let rest = NSRange(
        location: found.location + found.length,
        length: value.length - found.location - found.length)
      guard value.range(of: needle, options: [], range: rest).location == NSNotFound else {
        try fail("selection text is ambiguous; provide prefix/suffix")
      }
      range = CFRange(
        location: found.location + (prefix as NSString).length,
        length: (requested as NSString).length)
      if mode == "cursor_before" { range.length = 0 }
      if mode == "cursor_after" {
        range.location += range.length
        range.length = 0
      }
    }
    guard let selection = AXValueCreate(.cfRange, &range) else {
      try fail("cannot create selection range")
    }
    try performNativeMutation(e) {
      AXUIElementSetAttributeValue(e, kAXSelectedTextRangeAttribute as CFString, selection)
    }
    var actual = CFRange()
    let raw = attr(e, kAXSelectedTextRangeAttribute)
    let verified =
      raw != nil && CFGetTypeID(raw!) == AXValueGetTypeID()
      && AXValueGetValue(raw as! AXValue, .cfRange, &actual) && actual.location == range.location
      && actual.length == range.length
    return ["ok": true, "verified": verified, "effect": verified ? "confirmed" : "unverifiable"]
  case "type_text":
    let e: AXUIElement
    if args["element_token"] != nil {
      e = try targetElement(args)
    } else {
      let app = AXUIElementCreateApplication(pid_t(pid))
      guard let focused = attr(app, kAXFocusedUIElementAttribute),
        CFGetTypeID(focused) == AXUIElementGetTypeID()
      else { try fail("focused text element unavailable") }
      e = focused as! AXUIElement
      guard let elementWindow = containingWindow(e),
        CFEqual(elementWindow, try axWindow(pid, row))
      else { try fail("focused control does not belong to the requested window") }
    }
    try requireNativeText(e)
    let text = try string(args, "text")
    // AXSelectedText inserts at the caret without pacing or synthesizing
    // keystrokes. No fallback is attempted after an uncertain write.
    let prior = axString(e, kAXValueAttribute)
    try performNativeMutation(e) {
      AXUIElementSetAttributeValue(e, kAXSelectedTextAttribute as CFString, text as CFString)
    }
    let after = axString(e, kAXValueAttribute)
    let verified = after != prior && after.contains(text)
    return ["ok": true, "effect": verified ? "confirmed" : "unverifiable", "verified": verified]
  case "press_key":
    // Physical key delivery has no independent completion proof. Check
    // exact frontmost focus first to avoid global-shortcut side effects.
    guard LiveFocus().matches(pid, try axWindow(pid, row)) else {
      try fail(
        "key input requires this exact window/sheet to be focused; no focus change was performed")
    }
    let key = try string(args, "key").lowercased()
    let parts = nativeKeyParts(key)
    guard let last = parts.last, let code = nativeKeyCodes[last] else {
      try fail("unsupported key")
    }
    var flags = CGEventFlags()
    if last == "plus" { flags.insert(.maskShift) }
    for modifier in parts.dropLast() {
      switch modifier {
      case "cmd", "command", "super": flags.insert(.maskCommand)
      case "ctrl", "control": flags.insert(.maskControl)
      case "alt", "option": flags.insert(.maskAlternate)
      case "shift": flags.insert(.maskShift)
      default: try fail("unsupported key modifier")
      }
    }
    for down in [true, false] {
      guard let event = CGEvent(keyboardEventSource: args["delivery_mode"] as? String == "foreground" ? controlledEventSource : nil, virtualKey: code, keyDown: down) else {
        try fail("cannot create keyboard event")
      }
      if down {try checkIntervention()}
      event.setIntegerValueField(.eventSourceUnixProcessID,value:Int64(getpid()))
      // Release the chord modifiers too; retaining them on key-up leaves
      // macOS reporting a synthetic Command/Shift key as held indefinitely.
      event.flags = down ? flags : []
      if args["delivery_mode"] as? String == "foreground" {
        if down {guard LiveFocus().matches(pid,try axWindow(pid,row)) else {try fail("key focus changed before dispatch")}}
        event.post(tap:.cghidEventTap)
        if down {Thread.sleep(forTimeInterval:0.03)}
      } else {event.postToPid(pid_t(pid))}
    }
  case "scroll":
    let direction = args["direction"] as? String ?? "down"
    let amount = (args["amount"] as? Int) ?? 3
    guard ["up", "down", "left", "right"].contains(direction), (1...100).contains(amount) else {
      try fail("invalid scroll direction/amount")
    }
    let vertical = direction == "up" || direction == "down"
    let pages = args["by"] as? String == "page"
    let distance = pages ? (vertical ? windowRect(row).height : windowRect(row).width) : 1
    let delta = Int32(
      max(
        -100000,
        min(100000, Double(amount) * distance * (direction == "up" || direction == "left" ? 1 : -1))
      ))
    guard
      let event = CGEvent(
        scrollWheelEvent2Source: args["delivery_mode"] as? String == "foreground" ? controlledEventSource : nil, units: pages ? .pixel : .line, wheelCount: 2,
        wheel1: vertical ? delta : 0,
        wheel2: vertical ? 0 : delta, wheel3: 0)
    else { try fail("cannot create scroll event") }
    event.location = CGPoint(x: windowRect(row).midX, y: windowRect(row).midY)
    if args["element_token"] != nil {
      let e = try targetElement(args)
      if let point = axPoint(e), let size = axSize(e) {
        event.location = CGPoint(x: point.x + size.width / 2, y: point.y + size.height / 2)
      }
    }
    let foreground = args["delivery_mode"] as? String == "foreground"
    try postMouse(.mouseMoved, event.location, pid, wid, .left, 1, foreground)
    if foreground {Thread.sleep(forTimeInterval:0.02)}
    event.setIntegerValueField(.eventTargetUnixProcessID, value: Int64(pid))
    event.setIntegerValueField(.mouseEventWindowUnderMousePointer, value: Int64(wid))
    event.setIntegerValueField(.mouseEventWindowUnderMousePointerThatCanHandleThisEvent, value: Int64(wid))
    event.flags = []
    try checkIntervention()
    event.setIntegerValueField(.eventSourceUnixProcessID,value:Int64(getpid()))
    if foreground { try requireForegroundPoint(event.location,pid,wid);event.post(tap:.cghidEventTap) } else {event.postToPid(pid_t(pid))}
  case "drag":
    let from = try coordinate(args, "from_x", "from_y")
    let to = try coordinate(args, "to_x", "to_y")
    let foreground = args["delivery_mode"] as? String == "foreground"
    try postMouse(.mouseMoved, from, pid, wid, .left, 1, foreground)
    try postMouse(.leftMouseDown, from, pid, wid, .left, 1, foreground)
    var lastPoint=from
    defer { try? postMouse(.leftMouseUp, lastPoint, pid, wid, .left, 1, foreground) }
    for n in 1...12 {
      let t = Double(n) / 12
      let point=CGPoint(x: from.x + (to.x - from.x) * t, y: from.y + (to.y - from.y) * t)
      try postMouse(.leftMouseDragged,point,pid,wid,.left,1,foreground)
      lastPoint=point
      Thread.sleep(forTimeInterval: 0.008)
    }
  default: try fail("unsupported native action")
  }
  return ["ok": true, "effect": "unverifiable", "verified": false, "status": "dispatched"]
}
func call(_ request: [String: Any]) throws -> [String: Any] {
  guard let method = request["method"] as? String, let args = request["args"] as? [String: Any]
  else { try fail("invalid platform request") }
  switch method {
  case "request_permission":
    let kind = try string(args, "kind")
    if kind == "screen_recording" {
      _ = CGRequestScreenCaptureAccess()
    } else if kind == "accessibility" {
      _ = AXIsProcessTrustedWithOptions([kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary)
    } else if kind == "input_monitoring" {
      _ = CGRequestListenEventAccess()
    } else { try fail("unsupported permission request") }
    return ["ok":true,"accessibility":AXIsProcessTrusted(),"screen_recording":CGPreflightScreenCaptureAccess(),"restart_may_be_required":true]
  case "input_activity": return HumanInputMonitor.shared.snapshot()
  case "check_permissions":
    return [
      "ok": true, "accessibility": AXIsProcessTrusted(),
      "screen_recording": CGPreflightScreenCaptureAccess(),
      "input_monitoring":CGPreflightListenEventAccess(),
    ]
  case "list_apps":
    let windows = windowRows()
    let frontmostPID = axPID(focusedApplication())
    let apps = NSWorkspace.shared.runningApplications.filter { $0.activationPolicy == .regular }.map
    { app -> [String: Any] in
      let pid = Int(app.processIdentifier)
      return [
        "pid": pid, "name": app.localizedName ?? "", "bundle_id": app.bundleIdentifier ?? "",
        "running": true, "active": frontmostPID == pid, "frontmost": frontmostPID == pid,
        "windows": windows.filter { $0["pid"] as? Int == pid },
      ]
    }
    return ["ok": true, "apps": apps]
  case "list_windows":
    let pid = try integer(args, "pid")
    let rows = windowRows(pid: pid).map { original -> [String: Any] in
      var row = original
      if (row["title"] as? String ?? "").isEmpty, let element = try? axWindow(pid, row) {
        row["title"] = axString(element, kAXTitleAttribute)
      }
      return row
    }
    return ["ok": true, "windows": rows]
  case "capture_window":
    let pid = try integer(args, "pid")
    let wid = try integer(args, "window_id")
    _ = try exactWindow(pid, wid)
    let (path, size) = try screenshot(pid, wid, args)
    return ["ok":true,"pid":pid,"window_id":wid,"screenshot_file_path":path,"width":size.width,"height":size.height]
  case "get_window_state": return try state(args)
  case "execute_batch":
    guard let steps = args["steps"] as? [[String: Any]], (1...24).contains(steps.count) else {
      try fail("batch requires 1..24 steps")
    }
    // Preflight the whole plan before any mutation, including native
    // handle ownership and public AX action availability.
    for step in steps {
      guard let method = step["method"] as? String, let input = step["args"] as? [String: Any]
      else { try fail("invalid batch step") }
      try validateAction(method, input)
    }
    executingBatch = true
    defer {
      executingBatch = false
      for step in steps {
        if let input = step["args"] as? [String: Any], let pid = input["pid"] as? Int,
          let wid = input["window_id"] as? Int
        {
          snapshots.removeValue(forKey: key(session(input), pid, wid))
        }
      }
    }
    var results: [[String: Any]] = []
    for step in steps {
      let method = step["method"] as! String
      let input = step["args"] as! [String: Any]
      let started = Date()
      do {
        let result = try action(method, input)
        let uncertain =
          ["type_text", "set_value"].contains(method)
          && result["verified"] as? Bool == false
        results.append([
          "method": method, "ok": !uncertain, "result": result,
          "wall_ms": Date().timeIntervalSince(started) * 1000,
        ])
        // A key can navigate or change focus. Its dispatch is acknowledged,
        // but no later preflighted ref may execute before fresh observation.
        if uncertain || method == "press_key" { break }
      } catch let failure as NativeFailure {
        results.append(["method": method, "ok": false, "error": failure.message])
        break
      }
    }
    return [
      "ok": results.count == steps.count && results.allSatisfy { $0["ok"] as? Bool == true },
      "steps": results,
    ]
  case "launch_app":
    // Launching the application is separate from selecting a window.
    let identifier: String
    let url: URL?
    if let bundleID = args["bundle_id"] as? String, !bundleID.isEmpty {
      identifier = bundleID
      url = NSWorkspace.shared.urlForApplication(withBundleIdentifier: bundleID)
    } else if let name = args["name"] as? String, !name.isEmpty {
      identifier = name
      url = NSWorkspace.shared.fullPath(forApplication: name).map { URL(fileURLWithPath: $0) }
    } else {
      try fail("launch_app requires an installed app name or bundle_id")
    }
    guard let url = url, url.pathExtension.lowercased() == "app",
      Bundle(url: url)?.bundleIdentifier != nil
    else { try fail("installed application was not found: \(identifier)") }
    let config = NSWorkspace.OpenConfiguration()
    config.activates = false
    let semaphore = DispatchSemaphore(value: 0)
    var running: NSRunningApplication?
    var failure: Error?
    NSWorkspace.shared.openApplication(at: url, configuration: config) { app, error in
      running = app
      failure = error
      semaphore.signal()
    }
    guard semaphore.wait(timeout: .now() + 5) == .success, let app = running else {
      try fail("app launch failed: \(failure?.localizedDescription ?? "timeout")")
    }
    return [
      "ok": true, "pid": app.processIdentifier, "name": app.localizedName ?? identifier,
      "bundle_id": app.bundleIdentifier ?? identifier,
      "windows": windowRows(pid: Int(app.processIdentifier)),
    ]
  case "bring_to_front":
    try fail("bring_to_front is unsupported by the background-only native backend")
  default: return try action(method, args)
  }
}
@_cdecl("kageha_platform_call")
public func kagehaPlatformCall(_ input: UnsafePointer<CChar>) -> UnsafeMutablePointer<CChar>? {
  autoreleasepool {
    let output: [String: Any]
    do {
      guard
        let request = try JSONSerialization.jsonObject(with: Data(String(cString: input).utf8))
          as? [String: Any]
      else { try fail("request must be an object") }
      output = try call(request)
    } catch let failure as NativeFailure { output = ["error": failure.message, "execution_phase": failure.executionPhase] } catch {
      output = ["error": error.localizedDescription]
    }
    guard let data = try? JSONSerialization.data(withJSONObject: output, options: [.sortedKeys]),
      let text = String(data: data, encoding: .utf8)
    else { return strdup("{\"error\":\"platform serialization failed\"}") }
    return strdup(text)
  }
}
@_cdecl("kageha_platform_free")
public func kagehaPlatformFree(_ input: UnsafeMutablePointer<CChar>?) { free(input) }
