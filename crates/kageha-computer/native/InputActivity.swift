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
  private let nonce = UUID().uuidString
  private var tap: CFMachPort?

  private func start() {
    lock.lock()
    let launch = !started
    started = true
    lock.unlock()
    if launch {
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
        lock.lock();available=false;sequence &+= 1;lock.unlock()
      }
      _ = ready.wait(timeout:.now()+2)
    }
  }
  private func receive(_ type:CGEventType,_ event:CGEvent) {
    if type == .tapDisabledByTimeout || type == .tapDisabledByUserInput {
      lock.lock();sequence &+= 1;let current=tap;lock.unlock()
      // A coverage gap changes the revision even if re-enabling succeeds.
      if let current=current {CGEvent.tapEnable(tap:current,enable:true)}
      return
    }
    if event.getIntegerValueField(.eventSourceUnixProcessID) == Int64(getpid()) {return}
    lock.lock();sequence &+= 1;lock.unlock()
  }
  func snapshot()->[String:Any] {
    start()
    let permitted=CGPreflightListenEventAccess()
    let secureInput=IsSecureEventInputEnabled()
    lock.lock();let healthy=permitted && !secureInput && available && tap.map{CFMachPortIsValid($0) && CGEvent.tapIsEnabled(tap:$0)} == true
    let revision="\(nonce):\(sequence)";lock.unlock()
    // Only a boolean is retained; no key identity is exposed or stored.
    let heldKey=(0..<128).contains { code in code != 57 && code != 71 && CGEventSource.keyState(.hidSystemState,key:CGKeyCode(code)) }
    let heldMouse=[CGMouseButton.left,.right,.center].contains{CGEventSource.buttonState(.hidSystemState,button:$0)}
    return ["available":healthy,"revision":revision,"active":heldKey || heldMouse,
      "method":"listen_only_metadata","captures_key_content":false,
      "unavailable_reason":permitted ? (secureInput ? "secure_input" : (healthy ? "" : "tap_unavailable")) : "permission_denied"]
  }
  func require(_ expected:String) throws {
    let current=snapshot()
    guard current["available"] as? Bool == true,
      current["revision"] as? String == expected else {
      throw NativeFailure(message:"Human input or lost monitoring coverage interrupted computer control",executionPhase:"not_dispatched")
    }
  }
}
var activeInterventionRevision: String?
func checkIntervention() throws {
  if let expected=activeInterventionRevision {try HumanInputMonitor.shared.require(expected)}
}
