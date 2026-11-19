// Packaging-only helper. Atomically exchange app directories on one filesystem
// so an interrupted update never leaves the installed launcher path missing.
import Darwin
import Foundation
let arguments = CommandLine.arguments
if arguments.count != 3 { fputs("usage: atomic_install.swift STAGED_APP DESTINATION_APP\n", stderr); exit(2) }
let source = arguments[1], destination = arguments[2]
let result: Int32
if FileManager.default.fileExists(atPath: destination) {
  result = renamex_np(source, destination, UInt32(RENAME_SWAP))
} else {
  result = rename(source, destination)
}
if result != 0 { perror("atomic app installation"); exit(1) }
