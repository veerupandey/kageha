// Listen-only intervention metadata. Never retain key codes, text or positions.
import AppKit
import Carbon
import ApplicationServices
import Foundation

final class HumanInputMonitor {
  static let shared = HumanInputMonitor()
  private let lock = NSLock()
  private let ready = DispatchSemaphore(value: 0)
  private var started = false
  private var available = false
  private var sequence: UInt64 = 0
  private var backgroundSequence: UInt64 = 0
  private var coverageSequence: UInt64 = 0
  private var backgroundTarget: (pid: Int, window: Int)?
  private var lastTargetActivity: TimeInterval?
  private var activationObserver: NSObjectProtocol?
  private let nonce = UUID().uuidString
  private var tap: CFMachPort?

  private func start() {
    lock.lock()
    let launch = !started
    started = true
    lock.unlock()
    if launch {
      activationObserver = NSWorkspace.shared.notificationCenter.addObserver(
        forName: NSWorkspace.didActivateApplicationNotification, object: nil, queue: nil
      ) { [weak self] notice in
        guard let self,
          let app = notice.userInfo?[NSWorkspace.applicationUserInfoKey] as? NSRunningApplication
        else { return }
        self.lock.lock()
        if self.backgroundTarget?.pid == Int(app.processIdentifier) {
          self.backgroundSequence &+= 1
        }
        self.lock.unlock()
      }
      Thread.detachNewThread { [self] in
        let types: [CGEventType] = [.keyDown,.keyUp,.flagsChanged,.mouseMoved,
          .leftMouseDown,.leftMouseUp,.rightMouseDown,.rightMouseUp,.otherMouseDown,
          .otherMouseUp,.leftMouseDragged,.rightMouseDragged,.otherMouseDragged,
          .scrollWheel,.tabletPointer,.tabletProximity]
        let mask = types.reduce(CGEventMask(0)) { $0 | (CGEventMask(1) << $1.rawValue) }
        let created = CGEvent.tapCreate(tap:.cgSessionEventTap,place:.headInsertEventTap,
          options:.listenOnly,eventsOfInterest:mask,callback:{ _,type,event,context in
            if let context = context {
              Unmanaged<HumanInputMonitor>.fromOpaque(context).takeUnretainedValue().receive(type,event)
            }
            return Unmanaged.passUnretained(event)
          },userInfo:Unmanaged.passUnretained(self).toOpaque())
        guard let created = created,
          let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault,created,0) else {
          ready.signal(); return
        }
        lock.lock();tap=created;available=true;lock.unlock()
        CFRunLoopAddSource(CFRunLoopGetCurrent(),source,.commonModes)
        CGEvent.tapEnable(tap:created,enable:true)
        ready.signal()
        CFRunLoopRun()
        lock.lock();available=false;sequence &+= 1;coverageSequence &+= 1;lock.unlock()
      }
      _ = ready.wait(timeout:.now()+2)
    }
  }
  private func receive(_ type:CGEventType,_ event:CGEvent) {
    if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
      lock.lock();sequence &+= 1;coverageSequence &+= 1;let current=tap;lock.unlock()
      // A coverage gap changes the revision even if re-enabling succeeds.
      if let current=current {CGEvent.tapEnable(tap:current,enable:true)}
      return
    }
    if event.getIntegerValueField(.eventSourceUnixProcessID) == Int64(getpid()) {return}
    lock.lock()
    defer { lock.unlock() }
    sequence &+= 1
    guard let target = backgroundTarget else { return }
    if event.getIntegerValueField(.eventTargetUnixProcessID) == Int64(target.pid) {
      backgroundSequence &+= 1
      lastTargetActivity = ProcessInfo.processInfo.systemUptime
      return
    }
    switch type {
    case .leftMouseDown, .rightMouseDown, .otherMouseDown,
         .leftMouseUp, .rightMouseUp, .otherMouseUp, .scrollWheel,
         .leftMouseDragged, .rightMouseDragged, .otherMouseDragged, .tabletPointer:
      if pointerMayReachTarget(event.location, pid: target.pid) {
        backgroundSequence &+= 1
        lastTargetActivity = ProcessInfo.processInfo.systemUptime
      }
    default:
      break
    }
  }

  // WindowServer returns the visible windows in front-to-back order. A
  // window with ambiguous ownership or geometry cannot clear the guard.
  private func pointerMayReachTarget(_ point: CGPoint, pid: Int) -> Bool {
    guard let rows = CGWindowListCopyWindowInfo(
      [.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID
    ) as? [[String: Any]] else { return true }
    for row in rows {
      guard let bounds = row[kCGWindowBounds as String] as? [String: Any],
        let rect = CGRect(dictionaryRepresentation: bounds as CFDictionary)
      else { return true }
      if !rect.contains(point) || (row[kCGWindowAlpha as String] as? Double) == 0 { continue }
      guard let owner = row[kCGWindowOwnerPID as String] as? Int else { return true }
      return owner == pid
    }
    return false
  }

  func clearBackgroundTarget() {
    lock.lock()
    backgroundTarget = nil
    lastTargetActivity = nil
    backgroundSequence &+= 1
    lock.unlock()
  }

  func snapshot(target: (pid: Int, window: Int)? = nil, bind: Bool = true) -> [String: Any] {
    start()
    let permitted=CGPreflightListenEventAccess()
    let secureInput=IsSecureEventInputEnabled()
    let focusedPID = target == nil ? nil : axPID(focusedApplication())
    lock.lock()
    let healthy = permitted && !secureInput && available
      && tap.map { CFMachPortIsValid($0) && CGEvent.tapIsEnabled(tap: $0) } == true
    var backgroundBound = false
    if let target {
      if bind && (backgroundTarget?.pid != target.pid || backgroundTarget?.window != target.window) {
        backgroundTarget = target
        backgroundSequence &+= 1
        lastTargetActivity = nil
      }
      backgroundBound = backgroundTarget?.pid == target.pid && backgroundTarget?.window == target.window
    }
    let revision = if let target {
      "\(nonce):background:\(target.pid):\(target.window):\(coverageSequence):\(backgroundSequence)"
    } else {
      "\(nonce):\(sequence)"
    }
    let coverage = coverageSequence
    let recentTargetActivity = lastTargetActivity.map { ProcessInfo.processInfo.systemUptime - $0 < 0.35 } ?? false
    lock.unlock()
    // Only a boolean is retained; no key identity is exposed or stored.
    let heldKey=(0..<128).contains { code in code != 57 && code != 71 && CGEventSource.keyState(.hidSystemState,key:CGKeyCode(code)) }
    let heldMouse=[CGMouseButton.left,.right,.center].contains{CGEventSource.buttonState(.hidSystemState,button:$0)}
    let active = target.map { !backgroundBound || recentTargetActivity || focusedPID == nil || focusedPID == $0.pid }
      ?? (heldKey || heldMouse)
    return ["available":healthy,"revision":revision,"active":active,"coverage_revision":coverage,
      "method":"listen_only_metadata","captures_key_content":false,
      "unavailable_reason":permitted ? (secureInput ? "secure_input" : (healthy ? "" : "tap_unavailable")) : "permission_denied"]
  }
  func require(_ expected: String, target: (pid: Int, window: Int)? = nil) throws {
    let current = snapshot(target: target, bind: false)
    guard current["available"] as? Bool == true,
      target == nil || current["active"] as? Bool == false,
      current["revision"] as? String == expected else {
      throw NativeFailure(message:"Human input or lost monitoring coverage interrupted computer control",executionPhase:"not_dispatched")
    }
  }
}
var activeIntervention: (revision: String, target: (pid: Int, window: Int)?)?
func checkIntervention() throws {
  if let guardState = activeIntervention {
    try HumanInputMonitor.shared.require(guardState.revision, target: guardState.target)
  }
}
