// swift-tools-version: 5.10
import PackageDescription

// Código partilhado entre a app anfitriã e a Broadcast Extension.
//
// O núcleo (protocolo PSWP, fila de envio, sessão, servidor TCP, diagnóstico) é Swift portável:
// compila e é testado em macOS, Linux e Windows. O código que depende de frameworks da Apple
// (VideoToolbox, métricas do sistema) fica atrás de `#if canImport(...)`.
//
// `padstudio-sim` é um iPad simulado em Swift: serve um ficheiro H.264/HEVC com o mesmo código
// de sessão da extensão, para testar o núcleo contra o PC real sem iPad nem Mac.
let package = Package(
    name: "PadStudioKit",
    platforms: [.iOS(.v17), .macOS(.v14)],
    products: [
        .library(name: "PadStudioKit", targets: ["PadStudioKit"]),
        .executable(name: "padstudio-sim", targets: ["padstudio-sim"]),
    ],
    targets: [
        .target(name: "PadStudioKit"),
        .executableTarget(
            name: "padstudio-sim",
            dependencies: ["PadStudioKit"],
            // timeBeginPeriod: temporizadores de 1 ms no Windows (sem isto o ritmo sai em rajadas de ~15,6 ms).
            linkerSettings: [.linkedLibrary("winmm", .when(platforms: [.windows]))]
        ),
        .testTarget(name: "PadStudioKitTests", dependencies: ["PadStudioKit"]),
    ]
)
