// Independent decoding check for images of the disposable benchmark window.
import Foundation
import ImageIO
import CoreGraphics
let url = URL(fileURLWithPath: CommandLine.arguments[1])
guard let source = CGImageSourceCreateWithURL(url as CFURL,nil),
      let image = CGImageSourceCreateImageAtIndex(source,0,nil) else { exit(2) }
let width = image.width
let height = image.height
var bytes = [UInt8](repeating:0,count:width*height*4)
var minimum = 255
var maximum = 0
let rendered = bytes.withUnsafeMutableBytes { memory -> Bool in
  guard let context = CGContext(data:memory.baseAddress,width:width,height:height,bitsPerComponent:8,bytesPerRow:width*4,space:CGColorSpaceCreateDeviceRGB(),bitmapInfo:CGImageAlphaInfo.premultipliedLast.rawValue) else { return false }
  context.draw(image,in:CGRect(x:0,y:0,width:width,height:height))
  return true
}
if rendered {
  for at in stride(from:0,to:bytes.count,by:4) {
    for channel in 0..<3 { minimum = min(minimum,Int(bytes[at+channel])); maximum = max(maximum,Int(bytes[at+channel])) }
  }
}
let result:[String:Any] = ["decoded":rendered,"width":width,"height":height,"contrast":maximum-minimum]
let data = try JSONSerialization.data(withJSONObject:result,options:[.sortedKeys])
print(String(data:data,encoding:.utf8)!)
