#if canImport(Darwin)
import Darwin
import Foundation
#if os(iOS)
import os
#endif

/// Memória (S6), CPU, estado térmico e modo de poupança de energia do processo.
public final class SystemMetrics: StatsSource, @unchecked Sendable {
    private let lock = NSLock()
    private var peakBytes: UInt64 = 0
    private var lastCPUUs: UInt64?
    private var lastWallUs: UInt64?
    private let clock: HostClock

    public init(clock: HostClock = .system) {
        self.clock = clock
    }

    /// `phys_footprint`: o valor que o sistema compara com o limite da extensão (jetsam).
    public static func footprintBytes() -> UInt64? {
        var info = task_vm_info_data_t()
        var count = mach_msg_type_number_t(MemoryLayout<task_vm_info_data_t>.size / MemoryLayout<natural_t>.size)
        let kr = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: integer_t.self, capacity: Int(count)) {
                task_info(mach_task_self_, task_flavor_t(TASK_VM_INFO), $0, &count)
            }
        }
        return kr == KERN_SUCCESS ? info.phys_footprint : nil
    }

    /// Margem até ao limite de memória (só iOS; 0/nil quando não há limite).
    public static func availableBytes() -> UInt64? {
        #if os(iOS)
        let n = os_proc_available_memory()
        return n > 0 ? UInt64(n) : nil
        #else
        return nil
        #endif
    }

    /// Tempo de CPU (utilizador + sistema) do processo, em µs.
    static func cpuTimeUs() -> UInt64 {
        var u = rusage()
        getrusage(RUSAGE_SELF, &u)
        func us(_ t: timeval) -> UInt64 { UInt64(t.tv_sec) * 1_000_000 + UInt64(t.tv_usec) }
        return us(u.ru_utime) + us(u.ru_stime)
    }

    /// Modelo do aparelho (ex.: "iPad8,1").
    public static func machineModel() -> String {
        var s = utsname()
        uname(&s)
        return withUnsafeBytes(of: &s.machine) { raw in
            String(decoding: raw.prefix { $0 != 0 }, as: UTF8.self)
        }
    }

    public func contribute(to stats: inout SenderStats) {
        let footprint = Self.footprintBytes()
        let available = Self.availableBytes()
        let cpu = Self.cpuTimeUs()
        let wall = clock.nowUs()

        lock.lock()
        if let footprint { peakBytes = max(peakBytes, footprint) }
        let peak = peakBytes
        var cpuPercent: Double?
        if let lastCPU = lastCPUUs, let lastWall = lastWallUs, wall > lastWall {
            cpuPercent = Double(cpu &- lastCPU) / Double(wall - lastWall) * 100
        }
        lastCPUUs = cpu
        lastWallUs = wall
        lock.unlock()

        let mb = { (b: UInt64) in Double(b) / 1_048_576 }
        if let footprint { stats.memoryMb = rounded(mb(footprint), 1) }
        stats.thermal = switch ProcessInfo.processInfo.thermalState {
        case .nominal: .nominal
        case .fair: .fair
        case .serious: .serious
        case .critical: .critical
        @unknown default: .unknown
        }
        stats.lowPowerMode = ProcessInfo.processInfo.isLowPowerModeEnabled
        stats.withDiag { d in
            var m = MemoryDiagnostics()
            m.footprintMb = footprint.flatMap { rounded(mb($0), 1) }
            m.peakMb = rounded(mb(peak), 1)
            m.availableMb = available.flatMap { rounded(mb($0), 1) }
            if let footprint, let available { m.limitMb = rounded(mb(footprint + available), 1) }
            d.memory = m
            d.cpuPercent = rounded(cpuPercent, 1)
        }
    }
}
#endif
