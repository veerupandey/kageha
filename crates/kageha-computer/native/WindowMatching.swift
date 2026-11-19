import Foundation
import CoreGraphics

struct WindowIdentity {
  let pid: Int
  let nativeID: Int?
  let bounds: CGRect?
}

// A title can change between WindowServer and Accessibility observations. It
// cannot veto a unique identity or establish identity among coincident windows.
func matchingWindow(_ target: WindowIdentity, _ candidates: [WindowIdentity]) -> Int? {
  let owned = candidates.indices.filter { candidates[$0].pid == target.pid }
  if let id = target.nativeID {
    let exact = owned.filter { candidates[$0].nativeID == id }
    if exact.count == 1 { return exact[0] }
    if !exact.isEmpty { return nil }
  }
  guard let bounds = target.bounds else { return nil }
  var geometric: [Int] = []
  for index in owned {
    // A known conflicting native identity must never fall back to geometry.
    if let id = candidates[index].nativeID, let requested = target.nativeID, id != requested { continue }
    guard let rect = candidates[index].bounds else { continue }
    if abs(rect.minX - bounds.minX) < 2 && abs(rect.minY - bounds.minY) < 2
      && abs(rect.width - bounds.width) < 2 && abs(rect.height - bounds.height) < 2 {
      geometric.append(index)
    }
  }
  return geometric.count == 1 ? geometric[0] : nil
}
