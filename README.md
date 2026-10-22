# PadStudio

Transmite o ecrã do iPad para o PC (Windows/macOS) por **cabo USB** com latência mínima (ou por
Wi-Fi, como alternativa) e faz **multi-streaming** a partir do PC para **Twitch, YouTube e TikTok**,
com todo o trabalho pesado na GPU do PC.

- O iPad só captura (ReplayKit) e comprime no chip dedicado (VideoToolbox, sem B-frames, até 120 fps).
- O PC recebe pelo túnel `usbmuxd`, mostra o preview, grava a 120 fps e envia para as plataformas a 60 fps (NVENC/AMF/QSV).

## Documentação

| Documento | Conteúdo |
|-----------|----------|
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Análise dos requisitos, decisões (ADR), orçamento de latência, riscos, plano |
| [docs/PROTOCOL.md](docs/PROTOCOL.md) | Protocolo binário PSWP v1 (iPad ⇄ PC) e anexo `usbmuxd` |
| [docs/SPIKES.md](docs/SPIKES.md) | Como medir no iPad real: ritmo do ReplayKit (S1), custo da escala (S2), memória (S6) |
| `docs/BUILD.md` | *(passo 5)* Dependências e compilação em Windows/macOS |

## Estrutura

```
.
├── .github/workflows/build.yml CI: testes Swift + .ipa sem assinatura (runner macOS)
├── docs/                       Arquitetura, protocolo e spikes
├── protocol/
│   ├── reference/              Implementação de referência PSWP (JS) + gerador de vetores
│   └── test-vectors/v1.json    Vetores partilhados: Rust e Swift testam contra eles
├── pc-studio-receiver/         App do PC — Tauri 2 + Rust + React/Tailwind
│   ├── Cargo.toml              Workspace Rust
│   ├── crates/
│   │   ├── protocol/           Codec PSWP
│   │   ├── usbmux/             Cliente usbmuxd (Windows: TCP 27015 · macOS: /var/run/usbmuxd)
│   │   ├── engine/             Sessão, relógio, telemetria, bus, gravador MKV
│   │   ├── audio/              Mixer: iPad + microfone do PC, 48 kHz, ganhos, deriva
│   │   └── streaming/          Um FFmpeg por destino: Twitch, YouTube, TikTok (16:9 / 9:16)
│   ├── tools/
│   │   ├── fake-sender/        iPad simulado (vídeo de teste do FFmpeg servido em PSWP)
│   │   └── probe/              Cliente de teste (estatísticas, latência, gravação)
│   ├── src-tauri/              Shell Tauri (comandos, eventos, WebSocket do preview)
│   └── src/                    UI React
└── ipad-sender/                App do iPad — Swift (gerada com XcodeGen)
    ├── project.yml             Definição do projeto Xcode
    ├── Config/                 Team ID e bundle IDs
    ├── Packages/PadStudioKit/  Código partilhado (compila e testa também em Windows/Linux)
    │   ├── Sources/PadStudioKit/   Protocolo, sessão, fila, servidor TCP, diagnóstico; Apple/: encoder VT
    │   └── Sources/padstudio-sim/  iPad simulado em Swift (mesmo código de sessão da extensão)
    ├── PadStudioHost/          App anfitriã (SwiftUI)
    └── PadStudioBroadcast/     ReplayKit Broadcast Upload Extension
```

## Comandos úteis

Verificar os vetores de teste e testar o crate do protocolo (a partir de `pc-studio-receiver/`):

```bash
node ../protocol/reference/gen-vectors.mjs --check
```

```bash
cargo test -p padstudio-protocol
```

Simular o iPad sem iPad nem Mac: num terminal, o iPad simulado (vídeo de teste a 120 fps, encoder de hardware detetado automaticamente)…

```bash
cargo run -p padstudio-fake-sender -- --fps 120
```

…e noutro, o cliente de teste (10 s de estatísticas, grava o vídeo recebido):

```bash
cargo run -p padstudio-probe -- --duration 10 --dump captura.h264
```

Opções úteis: `fake-sender --codec hevc --rotate-every 3 --simulate-drops 4` e
`probe --switch-at 3 --switch-codec hevc --switch-fps 60 --keyframe-at 2`. Para pressionar a fila
do emissor: `probe --stall-at 5 --stall-for 2` (deixa de ler durante 2 s).

iPad real por cabo (usbmuxd): listar dispositivos, ver eventos, testar o túnel e ligar o cliente de teste:

```bash
cargo run -p padstudio-usbmux --example devices
```

```bash
cargo run -p padstudio-usbmux --example devices -- info
```

```bash
cargo run -p padstudio-probe -- --usb --wait
```

Motor completo (religação automática, telemetria a cada segundo, gravação em MKV), contra o
`fake-sender` ou o iPad por cabo:

```bash
cargo run -p padstudio-engine --example record -- --seconds 10 --out gravacoes
```

```bash
cargo run -p padstudio-engine --example record -- --usb --seconds 30 --out gravacoes
```

Mixer de áudio (iPad + microfone do PC): listar microfones, ver medidores, autoteste:

```bash
cargo run -p padstudio-audio --example mixer -- --list-mics
```

```bash
cargo run -p padstudio-audio --example mixer -- --seconds 10 --wav mistura.wav
```

```bash
cargo run -p padstudio-audio --example mixer -- --selftest
```

Multi-streaming (as chaves nunca são impressas; `--mic default` junta o microfone do PC):

```bash
cargo run -p padstudio-streaming --example stream -- --usb --twitch-key <CHAVE> --youtube-key <CHAVE> --mic default
```

Teste sem contas: um servidor RTMP local feito com o FFmpeg…

```bash
ffmpeg -listen 1 -f flv -i rtmp://127.0.0.1:19351/live/teste -c copy teste.flv
```

…e o destino apontado para ele:

```bash
cargo run -p padstudio-streaming --example stream -- --twitch-url rtmp://127.0.0.1:19351/live/teste --seconds 20
```

App do PC (a partir de `pc-studio-receiver/`). As chaves de transmissão introduzidas na UI ficam
no cofre do sistema (Gestor de Credenciais do Windows / Keychain do macOS), nunca em ficheiros:

```bash
npm install
```

```bash
npm run tauri dev
```

Para testar a app sem iPad, deixe o `fake-sender` a correr e, no separador **iPad**, escolha
**Rede / endereço** com `127.0.0.1:23456`.

### Lado iPad (Swift)

Testes do `PadStudioKit` (num Mac, ou no Windows com o Swift 6.4 para Windows, a partir de uma
«x64 Native Tools Command Prompt for VS 2022»):

```bash
cd ipad-sender/Packages/PadStudioKit && swift test
```

iPad simulado em Swift, com o mesmo código de sessão da extensão e as medições dos spikes. Serve
um ficheiro H.264/HEVC; liga-se com o `probe` ou com a app, como o `fake-sender`:

```bash
ffmpeg -f lavfi -i testsrc2=size=1920x1340:rate=120 -t 5 -c:v libx264 -bf 0 -g 120 teste.h264
```

```bash
swift run -c release padstudio-sim --input teste.h264 --fps 120
```

**Sem Mac:** o workflow [.github/workflows/build.yml](.github/workflows/build.yml) compila a app
num runner macOS do GitHub e deixa um `.ipa` sem assinatura para download (Actions → Artifacts).
Instala-se a partir do Windows com o Sideloadly e um Apple ID gratuito; passos em
[docs/SPIKES.md](docs/SPIKES.md) §2.1.

**Com um Mac:** o mesmo `.ipa` com `bash ipad-sender/scripts/build-ipa.sh`, ou pelo Xcode:

```bash
cd ipad-sender && xcodegen generate && open PadStudio.xcodeproj
```

Medições no iPad real (spikes S1/S2/S6): ver [docs/SPIKES.md](docs/SPIKES.md).

## Estado

| Passo | Descrição | Estado |
|------:|-----------|--------|
| 1 | Estrutura, arquitetura, protocolo, vetores de teste | ✅ |
| 2 | Backend Rust (protocolo, iPad simulado, usbmux, motor + gravador, mixer de áudio, multi-RTMP) | ✅ |
| 3 | UI React (preview WebCodecs, telemetria, destinos, áudio) | ✅ |
| 4 | iPad: Broadcast Extension + VideoToolbox + servidor | 🔶 4a: código e spikes prontos, núcleo testado no Windows; falta medir no iPad |
| 5 | Guia de compilação e dependências | ⏳ |
