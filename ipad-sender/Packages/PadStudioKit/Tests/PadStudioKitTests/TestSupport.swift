import Foundation
@testable import PadStudioKit

/// Relógio manual para testes (µs).
final class ManualClock: @unchecked Sendable {
    private let lock = NSLock()
    private var now: UInt64

    init(_ start: UInt64 = 1_000_000) { now = start }

    var clock: HostClock { HostClock { [self] in self.value } }

    var value: UInt64 {
        lock.lock()
        defer { lock.unlock() }
        return now
    }

    func advance(ms: Double) {
        lock.lock()
        now += UInt64(ms * 1000)
        lock.unlock()
    }
}

extension Data {
    init(hex: String) {
        var out = Data(capacity: hex.count / 2)
        var it = hex.makeIterator()
        while let a = it.next(), let b = it.next() {
            out.append(UInt8(String([a, b]), radix: 16)!)
        }
        self = out
    }

    var hex: String { map { String(format: "%02x", $0) }.joined() }
}

/// Vetores partilhados (`protocol/test-vectors/v1.json`).
enum Vectors {
    static let url: URL = {
        var dir = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        while true {
            let candidate = dir.appendingPathComponent("protocol/test-vectors/v1.json")
            if FileManager.default.fileExists(atPath: candidate.path) { return candidate }
            let parent = dir.deletingLastPathComponent()
            if parent.path == dir.path { fatalError("protocol/test-vectors/v1.json não encontrado") }
            dir = parent
        }
    }()

    static let document: [String: Any] = {
        let data = try! Data(contentsOf: url)
        return try! JSONSerialization.jsonObject(with: data) as! [String: Any]
    }()

    static var messages: [[String: Any]] { document["messages"] as! [[String: Any]] }
    static var streams: [[String: Any]] { document["streams"] as! [[String: Any]] }
    static var invalid: [[String: Any]] { document["invalid"] as! [[String: Any]] }

    static func message(_ name: String) -> [String: Any] {
        messages.first { $0["name"] as? String == name }!
    }
}

/// Número de um valor JSON (Int, Double, Bool ou NSNumber), em qualquer plataforma.
func jsonNumber(_ v: Any) -> Double? {
    if let d = v as? Double { return d }
    if let i = v as? Int { return Double(i) }
    if let u = v as? UInt64 { return Double(u) }
    if let n = v as? NSNumber { return n.doubleValue }
    return nil
}

/// Igualdade de significado entre dois valores JSON (a ordem das chaves não conta).
func jsonEqual(_ a: Any, _ b: Any) -> Bool {
    switch (a, b) {
    case let (x as String, y as String):
        return x == y
    case let (x as [String: Any], y as [String: Any]):
        return x.count == y.count && x.allSatisfy { key, value in y[key].map { jsonEqual(value, $0) } ?? false }
    case let (x as [Any], y as [Any]):
        return x.count == y.count && zip(x, y).allSatisfy { jsonEqual($0, $1) }
    case (is NSNull, is NSNull):
        return true
    default:
        guard let x = jsonNumber(a), let y = jsonNumber(b) else { return false }
        return abs(x - y) <= 1e-9 * max(1, abs(x))
    }
}

func jsonObject(_ data: Data) -> Any {
    try! JSONSerialization.jsonObject(with: data, options: [.fragmentsAllowed])
}

func uint(_ v: Any?) -> UInt64 {
    guard let v, let d = jsonNumber(v) else { return UInt64.max }
    return UInt64(d)
}
