// iPad simulado em Swift: serve um ficheiro Anexo-B (.h264/.h265) em PSWP com o MESMO código de
// sessão da Broadcast Extension (`Sender`, fila, descarte, keyframes, SENDER_STATS com `diag`).
// Serve para testar o núcleo Swift contra o PC real (probe, motor, app) sem iPad nem Mac.
//
//   ffmpeg -f lavfi -i testsrc2=size=1920x1340:rate=120 -t 5 -c:v libx264 -bf 0 -g 120 teste.h264
//   swift run padstudio-sim --input teste.h264 --fps 120
//   cargo run -p padstudio-probe -- --duration 10          (noutro terminal, em pc-studio-receiver)

import Foundation
import PadStudioKit
#if os(Windows)
import WinSDK

// Por omissão o Windows acorda as threads a cada ~15,6 ms: os frames sairiam em rajadas.
// O Windows 11 ignora o pedido de 1 ms em processos sem janela visível, a não ser que se desligue o
// "power throttling" do temporizador para este processo.
var throttling = PROCESS_POWER_THROTTLING_STATE(
    Version: ULONG(PROCESS_POWER_THROTTLING_CURRENT_VERSION),
    ControlMask: ULONG(PROCESS_POWER_THROTTLING_IGNORE_TIMER_RESOLUTION | PROCESS_POWER_THROTTLING_EXECUTION_SPEED),
    StateMask: 0
)
_ = SetProcessInformation(GetCurrentProcess(), ProcessPowerThrottling, &throttling,
                          DWORD(MemoryLayout<PROCESS_POWER_THROTTLING_STATE>.size))
_ = timeBeginPeriod(1)
#endif

struct Options {
    var input = ""
    var codec: VideoCodec?
    var width = 1920
    var height = 1340
    var fps = 120
    var port: UInt16 = PadStudioKit.usbPort
    var lan = false
    var token: String?
    var rotateEvery = 0
    var dropEvery = 0
    var seconds = 0
}

func usage() -> Never {
    print("""
    uso: padstudio-sim --input FICHEIRO [opções]
      --input F         vídeo Anexo-B (.h264 / .h265)
      --codec C         h264 | hevc (por omissão, pela extensão do ficheiro)
      --size LxA        dimensões do vídeo (por omissão 1920x1340)
      --fps N           ritmo de envio (por omissão 120)
      --port N          porta (por omissão 23456; 0 = efémera)
      --lan --token PIN modo rede local (0.0.0.0, exige PIN)
      --rotate-every N  roda mais 90° a cada N segundos
      --drop-every N    simula uma fila cheia a cada N segundos
      --seconds N       termina ao fim de N segundos
    """)
    exit(2)
}

func parseOptions() -> Options {
    var o = Options()
    var args = CommandLine.arguments.dropFirst().makeIterator()
    func value(_ name: String) -> String {
        guard let v = args.next() else {
            print("falta o valor de \(name)")
            usage()
        }
        return v
    }
    while let a = args.next() {
        switch a {
        case "--input": o.input = value(a)
        case "--codec":
            guard let c = VideoCodec(name: value(a)) else { usage() }
            o.codec = c
        case "--size":
            let parts = value(a).lowercased().split(separator: "x").compactMap { Int($0) }
            guard parts.count == 2, parts[0] > 0, parts[1] > 0 else { usage() }
            (o.width, o.height) = (parts[0], parts[1])
        case "--fps": o.fps = Int(value(a)) ?? 0
        case "--port": o.port = UInt16(value(a)) ?? 0
        case "--lan": o.lan = true
        case "--token": o.token = value(a)
        case "--rotate-every": o.rotateEvery = Int(value(a)) ?? 0
        case "--drop-every": o.dropEvery = Int(value(a)) ?? 0
        case "--seconds": o.seconds = Int(value(a)) ?? 0
        case "-h", "--help": usage()
        default:
            print("opção desconhecida: \(a)")
            usage()
        }
    }
    guard !o.input.isEmpty, (1 ... 240).contains(o.fps) else { usage() }
    return o
}

let startUs = HostClock.system.nowUs()

func log(_ s: String) {
    let t = Double(HostClock.system.nowUs() - startUs) / 1_000_000
    print(String(format: "[swift-sim %7.2fs] ", t) + s)
    fflush(stdout)
}

/// Atualiza o ritmo quando o PC muda `max_fps`.
final class Pacer: SenderDelegate, @unchecked Sendable {
    private let lock = NSLock()
    private var fps: Int

    init(fps: Int) { self.fps = fps }

    var intervalUs: UInt64 {
        lock.lock()
        defer { lock.unlock() }
        return UInt64(1_000_000 / max(fps, 1))
    }

    func sender(_: Sender, didChangeSettings settings: EncoderSettings, from old: EncoderSettings) {
        log("configuração do PC: \(settings.codec.name) \(settings.maxFps) fps \(Double(settings.bitrateBps) / 1e6) Mbps "
            + "caixa \(settings.maxWidth)x\(settings.maxHeight) (o simulador só aplica o fps)")
        if settings.maxFps != old.maxFps {
            lock.lock()
            fps = settings.maxFps
            lock.unlock()
        }
    }

    func sender(_: Sender, clientConnected peer: String) { log("PC ligado: \(peer)") }
    func sender(_: Sender, clientDisconnected peer: String) { log("PC desligado: \(peer)") }
}

let opts = parseOptions()
let codec = opts.codec ?? (opts.input.lowercased().hasSuffix(".h265") || opts.input.lowercased().hasSuffix(".hevc") ? .hevc : .h264)
guard let file = FileManager.default.contents(atPath: opts.input) else {
    print("não foi possível ler \(opts.input)")
    exit(1)
}
let splitter = AccessUnitSplitter(codec: codec, annexB: file)
let units = splitter.units
let paramSets = splitter.orderedParamSets
guard !units.isEmpty, units.contains(where: \.keyframe), !paramSets.isEmpty else {
    print("\(opts.input): sem access units, keyframes ou parâmetros \(codec.name)")
    exit(1)
}
log("\(opts.input): \(units.count) frames \(codec.name), \(units.filter(\.keyframe).count) keyframes, \(paramSets.count) parâmetros")

var senderOptions = Sender.Options()
senderOptions.transport = opts.lan ? .lan : .usb
senderOptions.token = opts.lan ? (opts.token ?? "000000") : opts.token
senderOptions.serverName = "padstudio-sim (Swift)"
senderOptions.device = DeviceInfo(model: "SwiftSim", system: "simulado em Swift", screenWidth: 2388, screenHeight: 1668, maxFps: 120)
senderOptions.log = { log($0) }
var settings = opts.lan ? EncoderSettings.lan : EncoderSettings.usb
settings.maxFps = opts.fps
let sender = Sender(options: senderOptions, settings: settings)
let pacer = Pacer(fps: opts.fps)
sender.delegate = pacer
let capture = CaptureMonitor()
let encode = EncodeMonitor()
sender.addStatsSource(capture)
sender.addStatsSource(encode)
encode.sessionCreated(codec: codec, inputWidth: 2388, inputHeight: 1668, outputWidth: opts.width, outputHeight: opts.height)

let server: TCPServer
do {
    server = try TCPServer(interface: opts.lan ? .any : .loopback, port: opts.lan && opts.port == PadStudioKit.usbPort ? PadStudioKit.lanPort : opts.port)
} catch {
    print("\(error)")
    exit(1)
}
server.start { stream in sender.serve(stream, peer: stream.peer) }
sender.start()
log("à escuta em \(opts.lan ? "0.0.0.0" : "127.0.0.1"):\(server.port)\(opts.lan ? " (PIN \(senderOptions.token!))" : "")")

// Ciclo de "captura + codificação": um access unit por intervalo, em ciclo.
let config = VideoConfig(
    codec: codec, fpsMax: UInt16(opts.fps), width: UInt16(opts.width), height: UInt16(opts.height),
    bitrateBps: UInt32(settings.bitrateBps), paramSets: NAL.join(paramSets)
)
var index = 0
var rotation = Rotation.none
var nextUs = HostClock.system.nowUs()
var nextSecond = nextUs + 1_000_000
var tick = 0
let endUs: UInt64? = opts.seconds > 0 ? nextUs + UInt64(opts.seconds) * 1_000_000 : nil

while endUs.map({ HostClock.system.nowUs() < $0 }) ?? true {
    var now = HostClock.system.nowUs()
    if now < nextUs {
        Thread.sleep(forTimeInterval: Double(nextUs - now) / 1_000_000)
        now = HostClock.system.nowUs()
    }
    nextUs += pacer.intervalUs
    if now > nextUs + 100_000 { nextUs = now } // atrasado (ex.: computador suspenso): não recuperar em rajada

    if sender.keyframes.shouldForce(nowUs: now), !units[index].keyframe {
        // "Forçar keyframe" num ficheiro = saltar para o próximo keyframe.
        index = (index ..< units.count).first { units[$0].keyframe } ?? units.firstIndex(where: \.keyframe)!
    }
    let au = units[index]
    index = (index + 1) % units.count

    capture.frameArrived(ptsUs: now, width: 2388, height: 1668, pixelFormat: "420f", orientation: nil, rotation: rotation)
    encode.submitted()
    let t0 = HostClock.system.nowUs()
    let packet = FramePacket.fromNALs(au.nals, codec: codec, paramSets: au.keyframe ? paramSets : nil)
    encode.completed(ms: Double(HostClock.system.nowUs() - t0) / 1000)
    sender.submitVideo(EncodedFrame(packet: packet, keyframe: au.keyframe, ptsUs: now, rotation: rotation, configId: 1, config: config))

    if now >= nextSecond {
        nextSecond += 1_000_000
        tick += 1
        if opts.rotateEvery > 0, tick % opts.rotateEvery == 0 {
            rotation = Rotation(rawValue: (rotation.rawValue + 1) % 4) ?? .none
            log("rotação → \(rotation.degrees)°")
        }
        if opts.dropEvery > 0, tick % opts.dropEvery == 0, sender.hasClient {
            log("descarte simulado: \(sender.simulateDrop()) frames na fila, a descartar até ao próximo keyframe")
        }
    }
}
sender.stop()
server.stop()
log("fim")
