import Dispatch

/// Relógio monotónico em µs.
///
/// No iPad, `DispatchTime` assenta em `mach_absolute_time`, a mesma base do
/// `CMClockGetHostTimeClock()` e dos PTS do ReplayKit, como exige PROTOCOL.md §6
/// (`HELLO_ACK.clock = "host_us"`). Nos testes usa-se um relógio manual.
public struct HostClock: @unchecked Sendable {
    private let source: () -> UInt64

    public init(_ source: @escaping () -> UInt64) {
        self.source = source
    }

    public static let system = HostClock { DispatchTime.now().uptimeNanoseconds / 1_000 }

    public func nowUs() -> UInt64 { source() }
}
