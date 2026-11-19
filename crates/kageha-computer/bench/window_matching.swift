import Foundation
import CoreGraphics

@main struct WindowMatchingTests {
  static func main() {
    let rect = CGRect(x: 10, y: 20, width: 800, height: 600)
    let target = WindowIdentity(pid: 42, nativeID: 7, bounds: rect)
    let unknown = WindowIdentity(pid: 42, nativeID: nil, bounds: rect)
    assert(matchingWindow(target, [unknown]) == 0) // titles are intentionally irrelevant
    assert(matchingWindow(target, [unknown, unknown]) == nil)
    assert(matchingWindow(target, [WindowIdentity(pid: 43, nativeID: 7, bounds: rect)]) == nil)
    assert(matchingWindow(target, [WindowIdentity(pid: 42, nativeID: 8, bounds: rect)]) == nil)
    assert(matchingWindow(target, [unknown, target]) == 1)
    assert(matchingWindow(target, [target, target]) == nil)
    assert(matchingWindow(target, [WindowIdentity(pid: 42, nativeID: nil, bounds: nil)]) == nil)
    assert(matchingWindow(target, [WindowIdentity(pid: 42, nativeID: 7, bounds: nil)]) == 0)
    print("Passed native identity, changing titles, unique geometry, and ambiguous window checks")
  }
}
