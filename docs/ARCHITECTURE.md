# PadStudio — Arquitetura do Sistema

> Documento vivo. Estado atual: **passo 4a em validação**.
>
> - **Feito:** backend Rust e app do PC completos (passos 1–3).
> - **Passo 4a (código feito):** o lado iPad, pronto para os spikes S1/S2/S6. O núcleo Swift foi testado no Windows contra o PC real; falta medir no iPad (ver [`SPIKES.md`](SPIKES.md)).
> - **Por fazer:** áudio, rede local e app anfitriã final (resto do passo 4), e o guia de compilação (passo 5).
>
> **iPad de referência:** iPad Pro 11" (2018, `iPad8,1`, A12X), ecrã 2388×1668 ProMotion 120 Hz,
> USB-C, iPadOS 26.6.2. Em H.264 nativo a 120 fps fica em 1,89 M MB/s, dentro do Level 5.2
> (limite 2,07 M), por isso não precisa de escala nem de HEVC (ver R3).
> Especificação do fio: [`PROTOCOL.md`](PROTOCOL.md).

PadStudio transmite o ecrã de um iPad para um PC (Windows/macOS) por cabo USB (ou Wi-Fi como
alternativa) com latência mínima, e a partir do PC faz multi-streaming para Twitch, YouTube e
TikTok usando a GPU do PC.

---

## 1. Visão geral

```
iPad ─────────────────────────────────────────────────────────────────────────
  PadStudioBroadcast   (ReplayKit Broadcast Upload Extension, limite ~50 MB RAM)
    vídeo   CMSampleBuffer  (CVPixelBuffer IOSurface 420f, VFR até 120 fps)
              └─► VTCompressionSession   [hardware · RealTime · sem B-frames]
                    └─► AVCC → Anexo-B  (feito durante a cópia para o buffer de envio)
    áudio   app / microfone ─► PCM s16le intercalado
    orientação ─► flag de rotação no pacote  (rodar é trabalho do PC)
                    └─► SendQueue  (limitada · prioridade: controlo > áudio > vídeo)
                          ├─► 127.0.0.1:23456   USB, via usbmuxd
                          └─► 0.0.0.0:23457     Wi-Fi, Bonjour + PIN

PC ───────────────────────────────────────────────────────────────────────────
  pc-studio-receiver   (Tauri 2 · Rust · React)
    cliente usbmuxd  /  TCP na rede local
      └─► Session   (parser PSWP · sincronização de relógio · telemetria)
            └─► Bus   (fan-out sem cópias: Arc<Packet>)
                  ├─► Preview         WebSocket local → WebCodecs (GPU) → canvas a cada vsync
                  ├─► Gravador MKV    cópia do bitstream original · 120 fps · 0 % GPU
                  ├─► Mixer de áudio  iPad + microfone do PC · ganhos em tempo real
                  └─► StreamManager   1 processo FFmpeg por destino
                        decode GPU → fps=60 → layout 16:9 / 9:16 → NVENC/AMF/QSV → RTMP(S)
                        ├─► Twitch      ├─► YouTube      └─► TikTok
    UI React/Tailwind ◄── comandos + eventos Tauri (telemetria a 4 Hz)
```

O iPad só captura e comprime (no chip dedicado). Tudo o resto — descodificar, rodar, compor,
misturar áudio, recodificar e enviar — acontece no PC.

---

## 2. Análise dos requisitos

| #   | Requisito | Veredicto | Decisão |
|-----|-----------|-----------|---------|
| R1  | Codificação 100 % hardware no iPad, zero-copy | ✅ Viável | O `CVPixelBuffer` do ReplayKit (IOSurface) entra direto no `VTCompressionSession`. Sem CIImage, Metal, vImage nem cópias de píxeis. A rotação passa para o PC (ver ADR-03). |
| R2  | 120 fps (ProMotion) | ⚠️ A validar | O ReplayKit entrega frames a ritmo variável (só quando o ecrã muda) e a Apple não garante 120 fps numa Broadcast Extension. O pipeline é VFR de raiz e a telemetria mostra o fps real. É o primeiro spike do passo 4 (S1). |
| R3  | H.264 High AutoLevel / HEVC, RealTime, sem reordenação | ✅ c/ nota | Além de `RealTime` e `AllowFrameReordering=false`: `MaxFrameDelayCount=0`, `EnableLowLatencyRateControl` (H.264) e `PrioritizeEncodingSpeedOverQuality`. **Atenção:** H.264 na resolução nativa (ex.: 2732×2048) a 120 fps excede o Level 5.2, e muitos descodificadores de PC recusam-no. Regra: HEVC para nativo@120, ou H.264 até 1920×1440@120. |
| R4  | 30–80 Mbps via USB, CBR ou dinâmico | ✅ | VBR com teto (`DataRateLimits`) por omissão; CBR opcional (`ConstantBitRate`). O bitrate muda em direto sem reiniciar o encoder. |
| R5  | Latência < 15 ms até ao PC | ⚠️ Parcial | Meta realista: **p50 captura → frame pronto a desenhar no PC < 15 ms** via USB-C. O *glass-to-glass* (ecrã do iPad → ecrã do PC) ronda 25–40 ms por causa do vsync e do painel, que não se controlam por software. Ver §5. |
| R6  | Túnel `usbmuxd` | ✅ | Windows: serviço Apple Mobile Device em `127.0.0.1:27015` (**confirmado a correr neste PC**). macOS: socket `/var/run/usbmuxd`. Cliente próprio em Rust (o protocolo é simples). |
| R7  | Alternativa Wi-Fi (WebRTC/UDP ou AirPlay) | 🔁 Alterado | **v1: o mesmo protocolo sobre TCP + Bonjour + PIN.** WebRTC dentro da extensão foi rejeitado (limite de ~50 MB, binário pesado). Recetor AirPlay também (protocolo proprietário com FairPlay, sem controlo do encoder, risco legal). v2: UDP com FEC/NACK, só se as medições o justificarem. |
| R8  | Trabalho pesado 100 % no PC (GPU) | ✅ c/ nota | Decode por GPU (D3D11VA / NVDEC / VideoToolbox), encode por NVENC / AMF / QSV / VideoToolbox. A composição (pad, crop, fundo desfocado 9:16) usa filtros de GPU quando o fabricante os tem; caso contrário, usa CPU (custo baixo a 1080p60). |
| R9  | Preview 120 fps, ≤ 1 frame de buffer, sem tearing | ✅ c/ nota | WebCodecs na WebView (decode por hardware) + desenho a cada vsync, onde "o último frame ganha". 120 fps visíveis só com monitor ≥ 120 Hz. Um renderer nativo (wgpu) fica como plano B se as medições o pedirem. |
| R10 | 120 → 60 fps para RTMP | ✅ | `fps=60` sobre entrada VFR com timestamps reais: a 120 fps descarta exatamente 1 em cada 2 frames. GOP de 2 s, CBR. |
| R11 | Multi-RTMP com botões independentes | ✅ | **1 processo FFmpeg por destino**: ligar ou desligar um não afeta os outros, as falhas ficam isoladas e cada um reinicia com backoff. |
| R12 | TikTok 16:9 ou 9:16 | ✅ c/ nota | Modos `crop` (centro), `fit-blur` (fundo desfocado) e `fit-black`. A chave RTMP do TikTok exige uma conta com acesso LIVE elegível. |
| R13 | Gravação local a 120 fps | ✅ | Remux (cópia) do bitstream do iPad para MKV, escrito pelo próprio Rust: 0 % GPU e qualidade idêntica à recebida. |
| R14 | Áudio do jogo + microfone do PC | ✅ | Mixer em Rust (`cpal` + resampling para 48 kHz) com ganhos e mute em tempo real, sem reiniciar o FFmpeg. |

---

## 3. Decisões de arquitetura (ADR)

**ADR-01 · Protocolo próprio (PSWP) sobre TCP.**
O modelo é o do scrcpy: cabeçalho fixo de 24 bytes e payload. Media em binário, controlo em JSON
(raro e extensível). Um único socket bidirecional. Detalhes em `PROTOCOL.md`.

**ADR-02 · Anexo-B no fio.**
O VideoToolbox produz AVCC (prefixos de tamanho de 4 bytes). A conversão para Anexo-B
(`00 00 00 01`) é feita *durante* a cópia para o buffer de envio, que já teria de existir, por
isso custa zero. Vantagem: o vídeo pode ser despejado para `.h264`/`.h265` e aberto com `ffplay`,
o que ajuda muito a depurar. VPS/SPS/PPS seguem in-band em cada keyframe.

**ADR-03 · A rotação é feita no PC.**
Na Broadcast Extension os buffers chegam sempre na orientação nativa do painel; a orientação real
vem em `RPVideoSampleOrientationKey`. Rodar no iPad exigiria GPU/CPU. Por isso enviamos uma flag
de 2 bits por frame e o PC roda: no preview com uma transformação no canvas (GPU, custo zero), no
FFmpeg com `transpose`. Mudar de orientação a meio de uma live reinicia de forma controlada só os
ramos RTMP (1–2 s de corte nas plataformas); preview e gravação não são afetados.

**ADR-04 · O servidor vive na extensão: o iPad escuta, o PC liga-se.**
A ligação só existe enquanto a transmissão do ReplayKit está ativa. Enquanto o iPad estiver ligado
por USB, o PC tenta ligar-se a cada 1 s. "A ligação mais recente ganha": uma ligação USB antiga e
presa nunca bloqueia uma nova.

**ADR-05 · Preview via WebSocket local + WebCodecs, e não via IPC do Tauri.**
O IPC do Tauri serializa as mensagens e tem latência variável com ~10 MB/s de vídeo. Um WebSocket
binário em `127.0.0.1` é direto. Os pacotes PSWP são reencaminhados tal como chegam (mesmo
cabeçalho), e o WebSocket usa token aleatório por sessão e escuta só em loopback.

**ADR-06 · Duas descodificações no PC (preview e FFmpeg).**
Isolamento: um servidor RTMP lento nunca atrasa o preview. O NVDEC tem capacidade de sobra para
vários streams de 1080p/1440p.

**ADR-07 · FFmpeg como processo externo (sidecar), não libav ligada ao binário.**
Distribuição simples (o `ffmpeg.exe` vai ao lado da app), falhas isoladas e nenhum toolchain
C/bindgen no Windows. A entrada é um **Matroska "live" escrito pelo Rust no stdin**, com os PTS
originais do iPad e o áudio já misturado. Um H.264 cru no stdin perderia os timestamps e
partiria a sincronização A/V com VFR. O mesmo escritor Matroska serve o gravador.

**ADR-08 · Repetição em ecrã estático (`min_fps`).**
O ReplayKit deixa de entregar frames quando o ecrã não muda. O `fps=60` do FFmpeg ficaria
parado e as plataformas cortariam a live. O emissor repete o último frame a `min_fps` (10 por
omissão), o que gera P-frames quase vazios e custo desprezável. Só acontece com o ecrã parado,
ou seja, nunca durante o jogo.

**ADR-09 · Chaves de stream no cofre do sistema operativo.**
Usamos `keyring` (Credential Manager no Windows, Keychain no macOS), nunca ficheiros de
configuração. Os URLs RTMP são redigidos nos logs.

**ADR-10 · Monorepo com XcodeGen.**
O lado Swift é descrito em `project.yml` e o `.xcodeproj` é gerado e não versionado (um
`project.pbxproj` editado à mão é fonte constante de conflitos). O código partilhado entre a app e
a extensão vive no Swift Package `PadStudioKit`.

---

## 4. Componentes

### 4.1 iPad — `ipad-sender`

| Peça | Função |
|------|--------|
| `PadStudioHost` (app SwiftUI) | Instruções e botão `RPSystemBroadcastPickerView` (aponta à extensão). A configuração vem do PC. O App Group (para o PIN do modo rede local) só entra no passo 4c; até lá a app assina com um Apple ID gratuito. |
| `PadStudioBroadcast` (extensão) | `SampleHandler`: extrai pixel buffer, PTS e orientação e entrega-os ao `BroadcastPipeline`. É aqui que tudo corre durante a transmissão. |
| `PadStudioKit` (Swift Package) | **Núcleo portável**, que compila e é testado em macOS, Linux e Windows: protocolo PSWP, `Sender` (handshake, ligação mais recente ganha, comandos, `SENDER_STATS`), `SendQueue` com a política de descarte, `KeyframeGate`, Anexo-B, `TCPServer` (sockets BSD/Winsock) e medidores de diagnóstico. **Só Apple:** `VideoEncoder` (VideoToolbox), `SystemMetrics` (memória, CPU, térmico) e `BroadcastPipeline`. **`padstudio-sim`:** iPad simulado com o mesmo código de sessão. |

**ADR-11 · Núcleo Swift portável e sockets BSD em vez de Network.framework.**
Este PC é Windows e a app iOS só compila num Mac (S8). Separar o que depende da Apple (encoder,
métricas, ReplayKit) do resto permite compilar e testar aqui ~80 % do código Swift, contra os
vetores partilhados e contra o PC real (`padstudio-sim` + `probe`/app). Os sockets BSD dão
`TCP_NODELAY`, `SO_SNDBUF` de 512 KiB (§1 do protocolo) e `SO_NOSIGPIPE`, e o mesmo código corre
no Windows (Winsock). O Network.framework não expõe o `SO_SNDBUF` e não existe fora da Apple.

**Configuração do encoder (VideoToolbox)**

| Propriedade | Valor |
|-------------|-------|
| `kVTCompressionPropertyKey_RealTime` | `true` |
| `kVTCompressionPropertyKey_AllowFrameReordering` | `false` (zero B-frames) |
| `kVTCompressionPropertyKey_ProfileLevel` | `H264_ConstrainedHigh_AutoLevel` (High sem B-frames, o perfil que o modo de baixa latência aceita; recua para `H264_High_AutoLevel` se for recusado) / `HEVC_Main_AutoLevel` |
| `kVTCompressionPropertyKey_MaxFrameDelayCount` | `0` (cada frame sai assim que fica pronto) |
| `kVTCompressionPropertyKey_ExpectedFrameRate` | `max_fps` (ex.: 120) |
| `AverageBitRate` + `DataRateLimits` | alvo + teto (VBR, por omissão) |
| `ConstantBitRate` | quando `bitrate_mode = "cbr"` |
| `MaxKeyFrameInterval(Duration)` | 0 (sem limite): os keyframes são decididos pelo `KeyframeGate` (pedidos do PC agrupados a 1 por 250 ms, descontinuidades, `keyframe_interval_s`) |
| `PrioritizeEncodingSpeedOverQuality` | `true` |
| Encoder spec `EnableLowLatencyRateControl` | `true` em H.264 (em HEVC, verificar suporte em runtime) |
| Entrada | o `CVPixelBuffer` do ReplayKit, sem cópia |

**Escala:** quando `max_width/max_height` é menor que o nativo, a sessão VT é criada com as
dimensões de saída e o VideoToolbox faz a escala internamente. A caixa aplica-se sem olhar à
orientação. Falta medir se isso usa o escalador de hardware sem tocar na GPU de renderização
(spike S2). Plano B: resolução nativa (no iPad de referência, H.264 nativo a 120 fps cabe no
Level 5.2).

**Sem PC ligado não se codifica:** a sessão VT só existe enquanto houver um PC ligado, por isso a
transmissão ativa sem PC quase não gasta bateria. Ao ligar, o primeiro frame é um keyframe.

**Memória (limite ~50 MB):** runtime + código ≈ 10–15 MB; fila de envio ≤ 8 MB
(drop-até-keyframe quando enche); buffers de áudio < 1 MB. Os buffers internos do VT são
monitorizados no spike S6.

### 4.2 PC — `pc-studio-receiver`

| Crate / pasta | Responsabilidade |
|---------------|------------------|
| `crates/protocol` | Codec PSWP: parser incremental sem cópias e encoder, validados contra os vetores partilhados. |
| `crates/usbmux` | Cliente usbmuxd: `ListDevices` / `Listen` / `Connect`. Windows via TCP 27015, macOS via unix socket. |
| `crates/engine` | Sessão, sincronização de relógio, telemetria, bus, gravador MKV, `StreamManager` (FFmpeg). Ver §4.4. |
| `crates/audio` | Mixer: áudio do iPad + microfone do PC (`cpal`), 48 kHz estéreo (`rubato`), ganhos, medidores, correção de deriva. Ver §4.5. |
| `crates/streaming` | `StreamManager`: um FFmpeg por destino (Twitch / YouTube / TikTok / RTMP), deteção de encoder, layouts 16:9 e 9:16, modo de espera, religação. Ver §4.6. |
| `src-tauri` | Shell da app: comandos e eventos Tauri, servidor WebSocket do preview, cofre de chaves. |
| `tools/fake-sender` | iPad simulado: serve PSWP em `localhost:23456` com vídeo de teste gerado pelo FFmpeg (encoder de hardware detetado automaticamente). Simula rotação, descartes e reconfiguração. Permite desenvolver todo o lado PC sem iPad nem Mac. |
| `tools/probe` | Cliente de teste: handshake, relógio sincronizado, estatísticas por segundo, `CONFIGURE`/`REQUEST_KEYFRAME` e gravação do vídeo recebido (`.h264`/`.hevc`). |
| `src/` (React) | Preview, telemetria, destinos RTMP, mixer de áudio. |

**Ramo de live (linha de comandos real, Twitch com iPad rodado 90°):**

```
ffmpeg -nostdin -nostats -loglevel warning -progress pipe:2 -stats_period 1 -hwaccel d3d11va \
  -f matroska -i pipe:0 \
  -filter_complex "[0:v]fps=60,split=2[bg][fg];
     [bg]scale=134:240:force_original_aspect_ratio=increase,crop=134:240,transpose=clock,boxblur=6:2,scale=1920:1080[bgb];
     [fg]scale=1080:1920:force_original_aspect_ratio=decrease:force_divisible_by=2,transpose=clock[fgs];
     [bgb][fgs]overlay=(W-w)/2:(H-h)/2,format=yuv420p[v]" -map [v] -map 0:a:0 \
  -c:v h264_nvenc -preset p5 -tune hq -rc cbr -b:v 6000k -maxrate 6000k -bufsize 12000k -bf 2 \
  -g 120 -keyint_min 120 -force_key_frames "expr:gte(t,n_forced*2)" \
  -af aresample=async=1 -c:a aac -b:a 160k -ar 48000 -ac 2 \
  -f flv -flvflags no_duration_filesize rtmp://live.twitch.tv/app/<CHAVE>
```

| Destino | Por omissão | Notas |
|---------|-------------|-------|
| Twitch | 1920×1080 · 60 fps · H.264 CBR 6000 kbps · GOP 2 s | `fit-blur` / `fit-black` / `crop` |
| YouTube | 1920×1080 · 60 fps · H.264 CBR 12000 kbps · GOP 2 s | RTMPS |
| TikTok | 1080×1920 (9:16) ou 1920×1080 · 60 fps · 6000 kbps · GOP 2 s | servidor + chave do TikTok LIVE Producer |
| Gravação | resolução recebida · 120 fps · cópia | MKV escrito pelo Rust; novo segmento quando a orientação muda |

**Deteção de encoders:** no arranque, tenta codificar 1 frame com `h264_nvenc`, `h264_amf`,
`h264_qsv` e `h264_videotoolbox`; `libx264` só como último recurso, com aviso.

**Nota sobre este PC:** tem GPU híbrida (RTX 4070 Laptop + Radeon integrada). A app e o FFmpeg
devem correr na NVIDIA ("Alto desempenho" nas definições gráficas do Windows, ou seleção explícita
do dispositivo). O painel interno pode estar ligado à iGPU, o que afeta o preview a 120 Hz
(spike S3). **O NVENC não funciona com o driver atual** (592.82): o FFmpeg 9 exige o driver
NVIDIA ≥ 610 (API NVENC 13.1). O AMF da Radeon funciona e é o que o `fake-sender` usa até lá
(risco S10).

### 4.3 Pipeline do preview

1. O Rust reencaminha `VIDEO_CONFIG` / `VIDEO_FRAME` pelo WebSocket local (mesmo formato PSWP).
2. O TS faz parse do cabeçalho → `VideoDecoder.configure({ codec, optimizeForLatency: true, hardwareAcceleration: "prefer-hardware" })` em modo Anexo-B.
3. `output(frame)`: guarda só o mais recente e faz `close()` ao anterior.
4. `requestAnimationFrame`: desenha o frame mais recente (WebGL `texImage2D(VideoFrame)`) com a rotação da flag.
5. Se `decodeQueueSize > 2`: descarta até ao próximo keyframe e envia `REQUEST_KEYFRAME`.

### 4.4 Motor (`crates/engine`)

```
Engine::start ──► supervisor (thread)
                    ├─ liga: USB (usbmuxd → porta 23456) ou TCP; nome do modelo via lockdownd
                    ├─ HELLO → espera HELLO_ACK (3 s) → estado Streaming
                    ├─ PING 5× a 50 ms, depois a cada 500 ms · RECEIVER_REPORT a cada 1 s
                    ├─ comandos: configure / request_keyframe (agrupados a 100 ms) / set_bitrate / set_endpoint
                    └─ sem bytes 3 s, BYE ou erro → SessionEnded → volta a tentar (1 s)
                  thread de leitura ──► Bus ──► subscrições (fila própria; cheia → Gap)
                                    └─► estatísticas → Telemetry (1 Hz)
```

- **Estado para a UI** (`EngineStatus`): `Waiting` traz o motivo e uma frase pronta a mostrar
  ("iPad Pro 11" (2018) ligado por USB. Inicia a transmissão do PadStudio no iPad.").
- **Subscrever a meio de uma sessão**: `Controller::subscribe` devolve as configurações já
  conhecidas e pede um keyframe, para o novo consumidor começar de imediato.
- **Gravador**: grava o bitstream tal como chega (0 % GPU) em MKV com faixas PCM do iPad.
  Muda de ficheiro quando mudam a configuração de vídeo, a rotação ou o áudio, sempre num
  keyframe (pedido na hora). Timestamps estritamente crescentes por faixa. A rotação vai em
  `ProjectionPoseRoll`, com sinal validado contra o `-display_rotation` do FFmpeg.
- **Escritor MKV** (`mkv.rs`): modo `File` (clusters com tamanho, Cues, Duration, SeekHead) e modo
  `Live` (tamanhos desconhecidos, sem atraso), que será a entrada do FFmpeg no passo 2f.

### 4.5 Mixer de áudio (`crates/audio`)

```
Bus: AUDIO_FRAME (iPad) ── pts do iPad → relógio local ──┐
Microfone do PC (cpal) ── instante de captura ───────────┼─► linha temporal 48 kHz ─► Bus: MixedAudio (10 ms)
                                                         │   latência fixa 60 ms
Ganho / mute por entrada (UI, rampa por bloco) ──────────┘   limitador suave
```

- **Sincronia por timestamp, não por ordem de chegada:** cada pacote é colocado na linha temporal
  pelo seu instante de captura. Com entradas a 44,1 e 48 kHz, o mesmo instante sai alinhado
  (erro medido: 0 e 21 µs). O `pts_local_us` da mistura está no mesmo relógio que
  `Controller::remote_to_local(pts_do_vídeo)`.
- **Deriva de relógio:** cada entrada compara onde os pacotes devem ficar com onde ficam e ajusta
  o rácio do resampler (até ±0,3 %, inaudível). Num teste com 300 ppm de deriva, a correção
  convergiu para −300 ppm sem realinhamentos nem falhas.
- **Descontinuidades** (pausa no iPad, salto > 40 ms, pacote antigo) realinham de imediato; o que
  chega depois de a sua janela ter sido misturada é contado como atrasado e descartado.
- **Microfone:** o predefinido ou um escolhido por nome. Falhas pontuais de buffer (xrun) são
  contadas à parte. Se o dispositivo desaparecer, o mixer tenta reabri-lo a cada 2 s.
- **Gravação:** o gravador MKV continua a guardar o áudio original do iPad; a mistura serve a live
  (passo 2f).

### 4.6 Multi-streaming (`crates/streaming`)

```
Bus ─► destino (thread) ─► Interleaver ─► Matroska live ─► stdin FFmpeg ─► RTMP(S)
        vídeo do iPad (pts → relógio local)        stderr ◄─ -progress (fps, kbps, speed)
        + MixedAudio (relógio local)
```

- **Um processo por destino:** ligar/desligar/falhar um não mexe nos outros. O encoder é
  detetado uma vez (NVENC → AMF → Quick Sync → VideoToolbox → x264).
- **Entrada do FFmpeg:** o bitstream original do iPad (sem recodificar) e a mistura em f32, num
  Matroska com timestamps no relógio local. O interleaver segura o vídeo até o áudio desse
  instante chegar e só mete silêncio se a mistura deixar mesmo de chegar.
- **Filtros:** `fps=60` primeiro (descarta metade dos frames a 120 fps), escala, e só depois a
  rotação (`transpose` é o filtro mais caro em CPU; rodar depois de reduzir custa até 4× menos).
- **iPad desligado a meio:** modo de espera com o último keyframe repetido a 30 fps e o áudio da
  mistura (o microfone continua), na mesma linha temporal. A ligação RTMP não cai. Retoma no
  keyframe seguinte; ao fim de 5 min sem iPad, pára. Enquanto em direto, pede um keyframe ao
  iPad a cada 2 s, para a imagem de espera ser recente.
- **Falhas:** o FFmpeg terminou (rede, chave errada) → nova tentativa após 2, 4, 8, 16 e 30 s. A
  espera volta a 2 s depois de 1 min estável.
- **Atraso limitado:** fila de 600 eventos por destino (~2 s). Um FFmpeg abaixo do tempo real
  perde pacotes e recomeça num keyframe, em vez de acumular atraso.
- **Chaves:** nunca aparecem no estado nem nos logs (URL e linhas do FFmpeg redigidas).

---

## 5. Orçamento de latência (USB-C · 1920×1440 · 120 fps · H.264 low-latency)

| Etapa | Estimativa | Como se mede |
|-------|-----------|--------------|
| Composição → callback do ReplayKit | ~1 frame (≈ 8 ms) | parte do `aux` (latência no emissor) |
| Encode VideoToolbox | 3–6 ms | `aux` = enfileiramento − PTS |
| Fila + socket + usbmuxd | 0,5–2 ms (P-frame) · 5–30 ms (keyframe em USB 2) | relógios sincronizados (PING/PONG) |
| Parse + WebSocket local | < 1 ms | `performance.now()` |
| Decode por hardware (WebCodecs) | 1–4 ms | callback `output` |
| Espera pelo vsync (120 Hz) | 0–8,3 ms (média ~4) | `requestAnimationFrame` |
| Scan-out + painel | 4–10 ms | não mensurável por software |
| **PTS → frame pronto no PC (meta p50)** | **< 15 ms** | telemetria da UI |
| **Glass-to-glass** | **~25–40 ms** | câmara de alta velocidade |

Os keyframes são o pico: um IDR de ~1 MB em USB 2 (iPads Lightning) pode demorar 30 ms ou mais.
Por isso, em USB os keyframes só são gerados a pedido.

---

## 6. Riscos e spikes de validação

| ID | Risco | Impacto | Mitigação | Quando |
|----|-------|---------|-----------|--------|
| S1 | O ReplayKit pode não entregar 120 fps na extensão | Alto | Pipeline VFR. Medição pronta (`diag.capture`: ritmo, histograma, atraso do PTS, orientação); falta correr no iPad ([`SPIKES.md`](SPIKES.md)) | Passo 4a (no iPad) |
| S2 | A escala dentro do VT pode usar GPU | Médio | Medição pronta (`diag.encode`, CPU, A/B com `probe --switch-max-width 0`); Instruments opcional. Plano B: nativo | Passo 4a (no iPad) |
| S3 | WebView2 + WebCodecs a 120 fps numa GPU híbrida | Baixo (medido: descodificação de 0,6–0,9 ms por frame; o preview segue a frequência do ecrã, 60 fps no painel de 60 Hz deste PC) | Confirmar num monitor de 120 Hz; plano B: renderer nativo wgpu | Passo 4 (com o iPad real) |
| S4 | Débito do usbmuxd em USB 2 (Lightning) | Baixo (o iPad de referência é USB-C) | Keyframes só a pedido; bitrate ≤ 40 Mbps em Lightning. O usbmuxd do Windows não indica a velocidade do barramento | Passo 4 |
| S5 | Rotação a meio da live | Baixo | Reinício controlado só dos ramos RTMP | Passo 2f |
| S6 | Limite de ~50 MB da extensão | Alto | Fila limitada (8 MiB), sem frameworks pesados. Medição pronta (`diag.memory`: footprint, pico, limite real; teste de pressão com `probe --stall-at`) | Passo 4a (no iPad) |
| S7 | Acesso RTMP do TikTok depende da elegibilidade da conta | Externo | Documentar; o modo 9:16 também serve para gravar | Passo 3 |
| S8 | **A app iOS só compila num Mac com Xcode** (este PC é Windows) | Alto | Núcleo Swift portável testado no Windows (ADR-11). Para o iPad: GitHub Actions compila um `.ipa` sem assinatura num runner macOS (`.github/workflows/build.yml`), e o Sideloadly assina-o no Windows com um Apple ID gratuito (por isso não há App Group). Ver [`SPIKES.md`](SPIKES.md) §2.1 | Passo 4a |
| S9 | Wi-Fi v1 sem cifra (só PIN) | Médio | Aviso na UI; v2: canal cifrado (TLS-PSK ou Noise) | v2 |
| S10 | Driver NVIDIA antigo para o FFmpeg 9 (NVENC exige driver ≥ 610) | Médio | Atualizar o driver; ou distribuir um FFmpeg 8.x (API 13.0); a deteção de encoders cai para AMF/QSV/x264 | Antes da 1.ª live real |
| S11 | Três destinos 1080p60 em simultâneo no AMF (GPU integrada) ficam no limite: o YouTube (12 Mbps) fica ~1–2 % abaixo do tempo real | Médio | NVENC (S10); filtros na GPU (`scale_cuda`/`vpp_amf`) se ainda faltar margem | Antes da 1.ª live real |
| S12 | O WebView ainda não tem CSP (`"csp": null` em `tauri.conf.json`) | Baixo (a UI só carrega código local e o preview exige token) | Definir uma CSP restrita (`connect-src` só para IPC e `ws://127.0.0.1`) e testá-la no build final | Passo 5 |

---

## 7. Plano de execução (revisto)

- **Passo 1 — Fundações** ✅ Estrutura do monorepo, esta análise, `PROTOCOL.md`, vetores de teste
  (gerados e verificados pela implementação de referência em `protocol/reference/`) e esqueletos
  das apps.
- **Passo 2 — Backend Rust**
  - 2a · ✅ Toolchain (Rust 1.99, FFmpeg 9.0.2) + crate `protocol`: parser incremental sem cópias,
    payloads binários e JSON, Anexo-B, sincronização de relógio, contagem de drops. Testado contra
    os vetores partilhados.
  - 2b · ✅ `fake-sender` (iPad simulado) + `probe` (cliente de teste). Validado: 1920×1440 a
    119,8 fps, latência p50 de 0,15 ms em localhost, mudança H.264 → HEVC em direto, rotação,
    descartes com `DISCONTINUITY`, substituição de ligação; vídeo gravado descodifica sem erros.
  - 2c · ✅ Crate `usbmux`: `ListDevices`, `Listen`, `Connect` e túnel (Windows TCP 27015 /
    macOS unix socket), testado com um usbmuxd falso e com o iPad real: deteção imediata, túnel
    até ao `lockdownd` em 60 ms (modelo e versão lidos sem emparelhamento), porta 23456 recusada
    sem a app. `probe --usb [--wait]` liga o cliente de teste por cabo.
  - 2d · ✅ `engine`: sessão, relógio, telemetria, bus e gravador MKV. Testado com um iPad
    simulado em memória (relógio desviado 50 000 s, comandos, BYE + religação, gravação a meio da
    sessão) e ponta a ponta com o `fake-sender`: MKV H.264 e HEVC a ~120 fps com áudio PCM,
    rotação correta, seek e zero erros de descodificação no `ffprobe`. Com o iPad real por USB, o
    motor identifica o modelo e explica que falta iniciar a transmissão.
  - 2e · ✅ Mixer de áudio (`crates/audio`): testes de sincronia, deriva, ganhos e
    descontinuidades; autoteste real com o `fake-sender` e o microfone do PC: 0 falhas em 252
    blocos, nível −20,0 dBFS exato, ganho de −12 dB medido em −12,0 dB, microfone a entregar áudio.
  - 2f · ✅ `StreamManager` (`crates/streaming`): validado com três servidores RTMP locais em
    simultâneo (Twitch 16:9, YouTube 16:9, TikTok 9:16) e o iPad simulado rodado 90°: H.264
    1080p a 60/1 fps exatos, keyframes a cada 2,000 s, AAC 48 kHz, bitrate CBR no alvo, zero
    erros de descodificação. Com o iPad desligado 4 s a meio, a live continuou em modo de espera
    e retomou sozinha; áudio e vídeo pararam e retomaram juntos (~0,1 s).
- **Passo 3 — UI** ✅ App Tauri 2 + React/Tailwind. Preview por WebSocket local (127.0.0.1,
  porta e token aleatórios) com descodificação por hardware no WebCodecs, sem recodificar e com
  "frame mais recente ganha"; telemetria a 10 Hz; destinos com interruptores independentes e
  chaves só no cofre do sistema (Gestor de Credenciais do Windows / Keychain do macOS), nunca no
  `settings.json` nem nos registos; mixer com medidores, ganhos e microfone desligado por omissão;
  gravação local. Validado na app real com o iPad simulado: preview H.264 1920×1342 a ~57 fps no
  painel de 60 Hz (descodificação 0,6–0,9 ms), telemetria a 119–122 fps / ~34 Mbps, e uma live
  "Twitch" iniciada pela UI para um servidor RTMP local: 1080p a 60 fps, ~5,95 Mbps, 1,04× tempo real.
- **Passo 4 — Swift**
  - 4a · 🔶 Pipeline de vídeo com os spikes S1/S2/S6 instrumentados (`PadStudioKit`, `SampleHandler`,
    `diag` no `SENDER_STATS`, painel "Diagnóstico do iPad" na app e no `probe`).
    - **Validado no Windows:** Swift 6.4.1, 51 testes, incluindo os vetores partilhados byte a byte.
    - **Simulador contra o PC real:** o `padstudio-sim` com o `probe` e a app deu 120 fps, latência p50 de 0,2 ms e zero erros de descodificação. Também passou o pedido de keyframe, o `CONFIGURE`, 1,5 s de PC parado (168 descartes e recuperação com `DISCONTINUITY`) e a rotação.
    - **Falta:** compilar o código Apple e medir no iPad ([`SPIKES.md`](SPIKES.md)).
  - 4b · Áudio da app (e microfone do iPad) normalizado para s16le, `AUDIO_CONFIG`/`AUDIO_FRAME`.
  - 4c · Rede local: escuta em 0.0.0.0:23457, Bonjour, PIN mostrado na app (App Group).
  - 4d · App anfitriã final e ajustes ditados pelos spikes.
- **Passo 5 — `BUILD.md`**: dependências e compilação em Windows e macOS, assinatura iOS.
