# PSWP — PadStudio Wire Protocol · v1

Protocolo binário entre o **iPad** (`ipad-sender`, servidor) e o **PC** (`pc-studio-receiver`,
cliente). O mesmo protocolo corre sobre USB (túnel `usbmuxd`) e sobre a rede local.

- Implementação de referência (JavaScript): [`protocol/reference/pswp.mjs`](../protocol/reference/pswp.mjs)
- Vetores de teste partilhados (Rust e Swift testam contra eles): [`protocol/test-vectors/v1.json`](../protocol/test-vectors/v1.json)
- Regenerar e verificar os vetores: `node protocol/reference/gen-vectors.mjs`

As palavras **TEM DE**, **DEVE** e **PODE** seguem o sentido da RFC 2119 (MUST, SHOULD, MAY).

---

## 1. Transporte

| | USB | Rede local (alternativa) |
|---|---|---|
| Socket no iPad | TCP `127.0.0.1:23456` (só loopback) | TCP `0.0.0.0:23457` |
| Como o PC chega lá | `usbmuxd` → `Connect(DeviceID, 23456)` (ver §9) | Bonjour `_padstudio._tcp` → IP:porta |
| Autenticação | Não precisa (cabo físico + "Confiar neste computador") | `token` (PIN de 6 dígitos mostrado na app do iPad) |
| Perfil por omissão | H.264 · até 120 fps · 50 Mbps | HEVC · 60 fps · 20 Mbps |

Regras comuns:

- **Uma única ligação TCP bidirecional** por sessão. Ordem garantida, sem perdas.
- Ambos os lados **TÊM DE** ativar `TCP_NODELAY`. O iPad **DEVE** usar um `SO_SNDBUF` moderado
  (~512 KiB) para que a pressão se note na fila da aplicação e não fique escondida no kernel.
- **Little-endian** em todos os inteiros (iPad ARM64 e PCs x86-64/ARM64 são LE nativamente).
- **A ligação mais recente ganha.** Se chegar uma nova ligação, o iPad envia `BYE{reason:"replaced"}` à
  antiga e fecha-a.
- O servidor só existe enquanto a transmissão do ReplayKit está ativa. Se a ligação for recusada,
  o PC repete a tentativa a cada 1 s enquanto o dispositivo estiver ligado.

---

## 2. Enquadramento (framing)

Cada mensagem tem um cabeçalho fixo de **24 bytes** seguido de `payload_len` bytes.

```
 0                   1                   2                   3
 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1 2 3 4 5 6 7 8 9 0 1
+---------------+---------------+---------------+---------------+
|   'P' (0x50)  |   'S' (0x53)  |     type      |     flags     |  0
+---------------+---------------+---------------+---------------+
|                       payload_len (u32)                       |  4
+---------------------------------------------------------------+
|                           seq (u32)                           |  8
+---------------------------------------------------------------+
|                           aux (u32)                           | 12
+---------------------------------------------------------------+
|                                                               | 16
+                         pts_us (u64)                          +
|                                                               | 20
+---------------------------------------------------------------+
|                    payload (payload_len bytes)  ...
```

| Offset | Tamanho | Campo | Descrição |
|-------:|--------:|-------|-----------|
| 0 | 2 | `magic` | Bytes `0x50 0x53` ("PS"). Serve para detetar dessincronização. |
| 2 | 1 | `type` | Tipo de mensagem (§3). |
| 3 | 1 | `flags` | Bits específicos do tipo. **TEM DE** ser 0 quando o tipo não define flags. |
| 4 | 4 | `payload_len` | Tamanho do payload em bytes. Máximo **16 MiB** (16 777 216). |
| 8 | 4 | `seq` | Contador por fluxo lógico (§2.1), dá a volta em 2³². |
| 12 | 4 | `aux` | Campo específico do tipo; 0 quando não é usado. |
| 16 | 8 | `pts_us` | Timestamp em µs no relógio monotónico **do emissor** (§6); 0 quando não se aplica. |

### 2.1 Contadores `seq`

- `VIDEO_FRAME`: +1 por cada frame **produzido pelo encoder**, mesmo que depois seja descartado
  na fila. Assim, um salto em `seq` = frames perdidos no iPad (telemetria "drop frames").
- `AUDIO_FRAME`: um contador independente por fonte de áudio (`source`).
- `VIDEO_CONFIG` / `AUDIO_CONFIG`: "geração" da configuração (+1 a cada mudança; por fonte no áudio).
- Restantes tipos: um contador por tipo, em cada direção.

### 2.2 Regras do recetor

1. `magic` inválido → erro de protocolo (`bad_magic`): fechar a ligação.
2. `payload_len` > 16 MiB → erro (`payload_too_large`): fechar a ligação.
3. **Tipo desconhecido → ignorar** (ler e descartar `payload_len` bytes). É o que garante
   compatibilidade com versões futuras.
4. Campos JSON desconhecidos → ignorar.
5. Payload binário maior do que o esperado para o tipo → ignorar os bytes extra. Menor → erro (`bad_payload`).

---

## 3. Tipos de mensagem

Codificação do payload: **B** = binário (§4), **J** = objeto JSON UTF-8 compacto, sem BOM (§5), **∅** = vazio.

| Tipo | Nome | Direção | Payload | Flags | `aux` | `pts_us` |
|-----:|------|---------|:-------:|-------|-------|----------|
| `0x01` | `HELLO` | PC → iPad | J | — | 0 | relógio do emissor |
| `0x02` | `HELLO_ACK` | iPad → PC | J | — | 0 | relógio do emissor |
| `0x03` | `PING` | ambos | B (8) | — | 0 | 0 |
| `0x04` | `PONG` | ambos | B (24) | — | 0 | 0 |
| `0x05` | `BYE` | ambos | J | — | 0 | relógio do emissor |
| `0x06` | `ERROR` | ambos | J | — | 0 | relógio do emissor |
| `0x07` | `SENDER_STATE` | iPad → PC | J | — | 0 | relógio do emissor |
| `0x10` | `VIDEO_CONFIG` | iPad → PC | B (16 + N) | — | 0 | PTS do 1.º frame com esta config (ou 0) |
| `0x11` | `VIDEO_FRAME` | iPad → PC | B (Anexo-B) | §4.4 | latência no emissor (µs) | **PTS de captura** |
| `0x12` | `AUDIO_CONFIG` | iPad → PC | B (8) | — | 0 | 0 |
| `0x13` | `AUDIO_FRAME` | iPad → PC | B (PCM) | §4.6 | latência no emissor (µs) | **PTS da 1.ª amostra** |
| `0x20` | `SENDER_STATS` | iPad → PC | J | — | 0 | relógio do emissor |
| `0x21` | `RECEIVER_REPORT` | PC → iPad | J | — | 0 | relógio do emissor |
| `0x30` | `CONFIGURE` | PC → iPad | J | — | 0 | relógio do emissor |
| `0x31` | `CONFIGURE_ACK` | iPad → PC | J | — | 0 | relógio do emissor |
| `0x32` | `REQUEST_KEYFRAME` | PC → iPad | ∅ | — | 0 | 0 |
| `0x33` | `SET_BITRATE` | PC → iPad | B (4) | — | 0 | 0 |

Faixas: `0x00–0x0F` sessão · `0x10–0x1F` media · `0x20–0x2F` telemetria · `0x30–0x3F` comandos ·
`0x80–0xFF` reservado para experiências (os recetores ignoram, pela regra 3).

**Latência no emissor (`aux`)** = (relógio do iPad no momento em que o pacote entra na fila de envio) − `pts_us`,
em µs, saturada a `0xFFFFFFFF`. Inclui a entrega do ReplayKit e a codificação.

---

## 4. Payloads binários

### 4.1 `PING` (8 bytes)

| Offset | Tipo | Campo |
|---:|---|---|
| 0 | u64 | `t0`: relógio de quem envia o PING, em µs |

### 4.2 `PONG` (24 bytes)

| Offset | Tipo | Campo |
|---:|---|---|
| 0 | u64 | `t0`: copiado do PING |
| 8 | u64 | `t1`: relógio de quem responde, no momento em que recebeu o PING |
| 16 | u64 | `t2`: relógio de quem responde, no momento em que envia o PONG |

Com `t3` = relógio de quem originou o PING ao receber o PONG:

```
rtt    = (t3 − t0) − (t2 − t1)
offset = ((t1 − t0) + (t2 − t3)) / 2      // relógio_respondente ≈ relógio_originador + offset
```

O PC envia PING a cada 500 ms (e 5 seguidos ao ligar) e usa a amostra de **menor RTT** numa janela
de 16 amostras. O PONG **DEVE** ser enviado com prioridade máxima na fila (à frente do vídeo).

### 4.3 `VIDEO_CONFIG` (16 bytes + parâmetros)

Enviado antes do primeiro `VIDEO_FRAME` e sempre que a configuração do encoder muda (codec,
resolução, bitrate alvo, cor).

| Offset | Tipo | Campo | Valores |
|---:|---|---|---|
| 0 | u8 | `codec` | `1` = H.264, `2` = HEVC |
| 1 | u8 | `bit_depth` | `8` (`10` reservado para HEVC Main10) |
| 2 | u16 | `fps_max` | Frame rate nominal máximo (ex.: 120). O fluxo é VFR. |
| 4 | u16 | `width` | Largura da imagem codificada **sem rotação** (após o *cropping* do SPS) |
| 6 | u16 | `height` | Altura, idem |
| 8 | u32 | `bitrate_bps` | Bitrate alvo atual |
| 12 | u8 | `color_primaries` | Código ITU-T H.273 (`1` = BT.709, `12` = Display P3) |
| 13 | u8 | `transfer` | H.273 (`1` = BT.709, `13` = sRGB) |
| 14 | u8 | `matrix` | H.273 (`1` = BT.709, `6` = BT.601) |
| 15 | u8 | `full_range` | `0` = video range, `1` = full range |
| 16 | … | `param_sets` | VPS (só HEVC) + SPS + PPS em **Anexo-B**, até ao fim do payload |

O recetor **TEM DE** ignorar `VIDEO_FRAME` até ter recebido um `VIDEO_CONFIG` **e** um keyframe.

### 4.4 `VIDEO_FRAME`

Payload: **um access unit completo** em Anexo-B (start codes de 4 bytes `00 00 00 01`), sem AUD.

| Bit de `flags` | Nome | Significado |
|---:|---|---|
| 0 | `KEYFRAME` | IDR (H.264) / IRAP (HEVC) |
| 1 | `PARAMS_INBAND` | O AU começa com VPS/SPS/PPS. **TEM DE** estar ativo em todos os keyframes. |
| 2 | `DISCONTINUITY` | Foram descartados frames antes deste no emissor. **Só aparece em keyframes** (o emissor descarta sempre até ao keyframe seguinte). |
| 3 | — | reservado (0) |
| 4–5 | `ROTATION` | Rotação que o recetor tem de aplicar para mostrar a imagem direita: `0` = 0°, `1` = 90° horário, `2` = 180°, `3` = 270° horário |
| 6–7 | — | reservado (0) |

Correspondência com `RPVideoSampleOrientationKey` (`CGImagePropertyOrientation`, semântica EXIF;
*a validar no iPad real*, spike S1 em [`SPIKES.md`](SPIKES.md); o valor recebido aparece em
`SENDER_STATS.diag.capture.orientation`):

| CGImagePropertyOrientation | `ROTATION` |
|---|---|
| `.up` (1) | 0 |
| `.down` (3) | 2 |
| `.right` (6) | 1 (90° horário) |
| `.left` (8) | 3 (270° horário) |

### 4.5 `AUDIO_CONFIG` (8 bytes)

| Offset | Tipo | Campo | Valores |
|---:|---|---|---|
| 0 | u8 | `source` | `0` = áudio da app/jogo, `1` = microfone do iPad |
| 1 | u8 | `format` | `1` = PCM s16le intercalado, `2` = PCM f32le intercalado |
| 2 | u8 | `channels` | `1` ou `2` |
| 3 | u8 | — | reservado (0) |
| 4 | u32 | `sample_rate` | Hz (ex.: 44100, 48000) |

O iPad **TEM DE** normalizar o áudio antes de enviar: o ReplayKit pode entregar big-endian ou
não intercalado, e o PC só aceita os formatos acima. É enviado antes do primeiro `AUDIO_FRAME` de
cada fonte e sempre que o formato muda (por exemplo, ao ligar auscultadores Bluetooth).

### 4.6 `AUDIO_FRAME`

Payload: amostras PCM intercaladas no formato do último `AUDIO_CONFIG` dessa fonte.
Tipicamente ~1024 frames (~21–23 ms).

| Bits de `flags` | Nome | Significado |
|---:|---|---|
| 0–3 | `SOURCE` | Igual a `AUDIO_CONFIG.source` |
| 4 | `DISCONTINUITY` | Houve pacotes de áudio descartados antes deste |
| 5–7 | — | reservado (0) |

### 4.7 `SET_BITRATE` (4 bytes)

| Offset | Tipo | Campo |
|---:|---|---|
| 0 | u32 | `bitrate_bps` |

Aplicado em direto, sem reiniciar o encoder nem forçar keyframe. Não tem resposta; o valor efetivo
aparece em `SENDER_STATS.target_bitrate_bps`.

---

## 5. Payloads JSON

Todos os campos são opcionais, salvo indicação em contrário. Campos desconhecidos são ignorados.

### 5.1 `HELLO` (PC → iPad)

Tem de ser a primeira mensagem do cliente, até **3 s** depois de abrir o socket. O iPad não envia
nada antes do `HELLO`.

```json
{
  "protocol": 1,
  "client": "pc-studio-receiver",
  "client_version": "0.1.0",
  "transport": "usb",
  "token": "482913",
  "want": { "video": true, "audio_app": true, "audio_mic": false },
  "config": { "codec": "h264", "max_fps": 120, "bitrate_bps": 50000000 }
}
```

| Campo | Obrig. | Descrição |
|---|:---:|---|
| `protocol` | ✔ | Versão mais alta que o cliente suporta |
| `transport` | ✔ | `"usb"` ou `"lan"` |
| `token` | LAN | PIN de emparelhamento. Errado → `BYE{reason:"unauthorized"}`. 3 falhas → bloqueio de 30 s. |
| `want` | | Fluxos pretendidos |
| `config` | | Configuração inicial (mesmo esquema de `CONFIGURE`) |

### 5.2 `HELLO_ACK` (iPad → PC)

```json
{
  "protocol": 1,
  "server": "PadStudioBroadcast",
  "server_version": "0.1.0",
  "session_id": "6F1C2B0E-8C2D-4C9A-9D61-2B1E0A7F3C11",
  "clock": "host_us",
  "device": { "model": "iPad16,6", "system": "iPadOS 26.0", "screen_width": 2752, "screen_height": 2064, "max_fps": 120 },
  "codecs": ["h264", "hevc"],
  "config": { "codec": "h264", "max_fps": 120, "min_fps": 10, "bitrate_bps": 50000000, "bitrate_mode": "vbr", "max_width": 1920, "max_height": 1440, "keyframe_interval_s": 0, "audio_app": true, "audio_mic": false }
}
```

Se `protocol` não for suportado → `BYE{reason:"protocol_version"}`. Depois do `HELLO_ACK`, a
sequência normal é `AUDIO_CONFIG` (por fonte) → `VIDEO_CONFIG` → `VIDEO_FRAME` (keyframe) → …

### 5.3 `CONFIGURE` (PC → iPad) e objeto de configuração

Os campos em falta mantêm o valor atual.

| Campo | Tipo | Por omissão (USB) | Descrição |
|---|---|---|---|
| `codec` | `"h264"` \| `"hevc"` | `"h264"` | Mudar de codec recria a sessão VT |
| `max_fps` | int | `120` | `ExpectedFrameRate` e limite superior |
| `min_fps` | int | `10` | Repetição do último frame em ecrã estático (0 = desligado). Ver ADR-08. |
| `bitrate_bps` | int | `50000000` | Alvo |
| `bitrate_mode` | `"vbr"` \| `"cbr"` | `"vbr"` | VBR com teto de 1,5× o alvo, ou CBR |
| `max_width` / `max_height` | int | `1920` / `1440` | Caixa máxima, mantendo o aspeto e sem aumentar (0 = nativo). Aplica-se **sem olhar à orientação**: o lado maior da imagem cabe no maior dos dois valores (2388×1668 e 1668×2388 dão 1920×1340 e 1340×1920) |
| `keyframe_interval_s` | number | `0` | 0 = só a pedido (recomendado em USB) |
| `audio_app` / `audio_mic` | bool | `true` / `false` | Fontes de áudio |

### 5.4 `CONFIGURE_ACK` (iPad → PC)

```json
{ "request_seq": 3, "applied": true, "config": { "...": "configuração efetiva completa" }, "message": "max_width ajustado a 1920" }
```

Se mudar alguma coisa no encoder, o iPad envia a seguir um novo `VIDEO_CONFIG` e um keyframe.

### 5.5 `SENDER_STATE` (iPad → PC)

```json
{ "state": "paused" }
```

`state` ∈ `"active"`, `"paused"` (o sistema pausou a transmissão), `"finishing"` (a transmissão vai terminar).

### 5.6 `SENDER_STATS` (iPad → PC, ~1 Hz)

```json
{
  "fps_capture": 119.6, "fps_encoded": 119.6, "bitrate_bps": 41234567, "target_bitrate_bps": 50000000,
  "frames_dropped": 0, "keyframes": 1, "send_queue_bytes": 84211, "send_queue_ms": 1.4,
  "encode_ms_avg": 4.1, "thermal": "nominal", "memory_mb": 21.7, "low_power_mode": false
}
```

`thermal` ∈ `"nominal"`, `"fair"`, `"serious"`, `"critical"` (`ProcessInfo.thermalState`). É o
indicador direto do "impacto zero" no iPad.

#### 5.6.1 `SENDER_STATS.diag` (opcional)

Diagnóstico da última janela (~1 s), usado nos spikes S1/S2/S6 ([`SPIKES.md`](SPIKES.md)). É um
campo novo e opcional: recetores que não o conhecem ignoram-no (regra 4 de §2.2). Todos os campos
internos são opcionais.

```json
"diag": {
  "capture": { "fps": 119.6, "interval_ms_p50": 8.33, "interval_ms_p95": 9.1, "interval_ms_max": 25.0,
               "intervals": { "lt6": 0, "6to10": 117, "10to14": 1, "14to20": 0, "20to40": 1, "ge40": 0 },
               "lag_ms_p50": 3.2, "lag_ms_max": 6.0, "repeats": 0,
               "width": 1668, "height": 2388, "pixel_format": "420f", "orientation": 6, "rotation": 90 },
  "encode":  { "codec": "h264", "input_width": 1668, "input_height": 2388, "output_width": 1340, "output_height": 1920,
               "scaled": true, "ms_p50": 3.9, "ms_p95": 5.0, "ms_max": 6.1, "in_flight_max": 1,
               "errors": 0, "encoder_drops": 0, "sessions": 1 },
  "memory":  { "footprint_mb": 21.7, "peak_mb": 24.0, "available_mb": 28.3, "limit_mb": 50.0 },
  "cpu_percent": 12.5,
  "notes": ["VT MaxFrameDelayCount recusado (-12900)"]
}
```

| Bloco | Conteúdo |
|---|---|
| `capture` | Frames entregues pelo ReplayKit: ritmo, percentis e histograma dos intervalos (ms) entre chegadas (`6to10` ≈ 120 Hz, `14to20` ≈ 60 Hz), atraso entre o PTS e a chegada ao callback (`lag_ms_*`), frames repetidos por ecrã parado, dimensões e formato do buffer, último `RPVideoSampleOrientationKey` (EXIF) e a rotação enviada |
| `encode` | Sessão VideoToolbox: codec, resolução de entrada e de saída, tempo por frame (entrega → resultado), máximo de frames em voo, erros, frames descartados pelo encoder, sessões criadas |
| `memory` | `phys_footprint` (o valor que o sistema compara com o limite), pico, margem (`os_proc_available_memory`) e limite efetivo (= footprint + margem) |
| `cpu_percent` | CPU do processo da extensão na janela (pode passar de 100 % com vários núcleos) |
| `notes` | Avisos (ex.: propriedades do VideoToolbox recusadas pelo aparelho) |

### 5.7 `RECEIVER_REPORT` (PC → iPad, ~1 Hz)

```json
{ "fps_received": 119.5, "fps_decoded": 119.5, "recv_bitrate_bps": 41100000, "latency_ms_p50": 9.8, "latency_ms_p95": 14.2, "rtt_ms": 0.9, "decode_errors": 0 }
```

Na rede local, o iPad usa este relatório para adaptar o bitrate (ABR).

### 5.8 `BYE` e `ERROR`

```json
{ "reason": "user_stopped", "message": "Transmissão terminada no iPad" }
```

`reason` ∈ `"user_stopped"`, `"replaced"`, `"unauthorized"`, `"protocol_version"`, `"timeout"`,
`"shutdown"`, `"error"`. Quem envia `BYE` fecha o socket a seguir.

```json
{ "code": "unsupported_config", "message": "HEVC 10-bit não suportado", "fatal": false, "ref_type": 48, "ref_seq": 3 }
```

---

## 6. Relógios e medição de latência

- **iPad:** `pts_us` e os timestamps de PING/PONG usam **o mesmo relógio**: o host time
  (`CMClockGetHostTimeClock()`) em µs. Os PTS das amostras do ReplayKit já estão nesse domínio.
  `HELLO_ACK.clock = "host_us"`.
- **PC:** relógio monotónico do processo (`Instant`) em µs.
- Com o `offset` do PING/PONG (§4.2), em que o PC é o originador e o iPad o respondente,
  `offset_ipad→pc = −offset` (o valor a somar a um instante do iPad para obter o instante no PC).
  A latência de um frame no momento em que chega ao PC é:

```
latência_rede         = t_rececao_pc − (pts_us + aux + offset_ipad→pc)
latência_total_ate_pc = t_rececao_pc − (pts_us + offset_ipad→pc)
```

A UI mostra o desdobramento: **iPad** (`aux`) · **transporte** · **decode** · **apresentação**.

---

## 7. Comportamento do emissor (iPad)

### 7.1 Fila de envio e pressão

- Fila limitada (**8 MiB** ou **100 ms** de vídeo na USB / **250 ms** na rede local, o que vier primeiro).
- Prioridade: `PONG`/controlo > áudio > vídeo.
- Se um frame de vídeo não couber, ou se o mais antigo na fila ultrapassar o limite de idade:
  1. descartar todo o vídeo da fila e os P-frames seguintes;
  2. forçar keyframe (`kVTEncodeFrameOptionKey_ForceKeyFrame`);
  3. enviar esse keyframe com `DISCONTINUITY`.
- Áudio a mais: descartar os pacotes mais antigos e marcar o seguinte com `DISCONTINUITY`.

### 7.2 Keyframes

- Em USB não há keyframes periódicos (`keyframe_interval_s = 0`); só a pedido ou por descontinuidade.
- Os `REQUEST_KEYFRAME` são agrupados: no máximo 1 keyframe forçado a cada 250 ms.

### 7.3 Ecrã estático (`min_fps`)

Se não chegar nenhum frame do ReplayKit durante `1000 / min_fps` ms, o emissor volta a codificar
o último `CVPixelBuffer` com o PTS atual.

### 7.4 Keepalive

- O PC envia PING a cada 500 ms; o iPad envia `SENDER_STATS` a cada 1 s.
- Sem receber nada durante **3 s** (PC) ou **5 s** (iPad) → a ligação é dada como morta e fechada.

---

## 8. Sequências

**Ligação USB normal**

```
PC                                   usbmuxd                    iPad (extensão)
 │ Listen ───────────────────────────►│                              │
 │◄─────────────── Attached(DeviceID) │                              │
 │ Connect(DeviceID, 23456) ─────────►│ ───── TCP 127.0.0.1:23456 ──►│
 │◄────────────────────── Result(0)   │                              │
 │══════════════ túnel TCP transparente a partir daqui ══════════════│
 │ HELLO ───────────────────────────────────────────────────────────►│
 │◄─────────────────────────────────────────────────────── HELLO_ACK │
 │◄──────────────────────────────────────────── AUDIO_CONFIG (app)   │
 │◄──────────────────────────────────────────────────── VIDEO_CONFIG │
 │◄────────────────────────────────── VIDEO_FRAME (KEYFRAME|PARAMS)  │
 │ PING ────────────────────────────────────────────────────────────►│
 │◄──────────────────────────────────────────────────────────── PONG │
 │◄────────────────────────────── VIDEO_FRAME / AUDIO_FRAME …        │
```

`Result(3)` (ligação recusada) = a transmissão não está ativa no iPad → tentar de novo em 1 s.

**Recuperação de um erro de descodificação no PC**

```
PC: erro no decoder → descarta até ao próximo keyframe → REQUEST_KEYFRAME ──► iPad
iPad: ForceKeyFrame → VIDEO_FRAME (KEYFRAME|PARAMS_INBAND) ──► PC: retoma
```

---

## 9. Anexo — protocolo `usbmuxd` (lado PC)

| SO | Endpoint |
|---|---|
| Windows | TCP `127.0.0.1:27015` (serviço *Apple Mobile Device*, instalado com o iTunes ou a app *Dispositivos Apple*) |
| macOS | Unix socket `/var/run/usbmuxd` |

Cabeçalho de cada mensagem usbmuxd (16 bytes, little-endian):

| Offset | Tipo | Campo |
|---:|---|---|
| 0 | u32 | `length` (inclui o cabeçalho) |
| 4 | u32 | `version` = `1` (plist) |
| 8 | u32 | `message` = `8` (plist) |
| 12 | u32 | `tag` (eco na resposta) |

Seguido de um plist XML. Mensagens usadas:

- `ListDevices` → `{ DeviceList: [ { DeviceID, Properties: { ConnectionType, SerialNumber, ProductID, … } } ] }`
- `Listen` → fluxo contínuo de `{ MessageType: "Attached", DeviceID, Properties }` / `{ MessageType: "Detached", DeviceID }`
- `Connect` com `{ DeviceID, PortNumber }` → `{ MessageType: "Result", Number }`. **`PortNumber` é a porta em
  ordem de rede lida como inteiro**: 23456 (`0x5BA0`) → `0xA05B` = **41051**. Com `Number = 0`, o mesmo socket passa
  a ser o túnel TCP para a porta do iPad.

Todas as mensagens levam também `ClientVersionString`, `ProgName`, `BundleID` e `kLibUSBMuxVersion = 3`.
Códigos de `Result`: `0` OK · `1` comando inválido · `2` dispositivo inválido · `3` ligação recusada · `6` versão inválida.
Se o iPad aparecer com `ConnectionType = "USB"` e `"Network"`, usa-se o USB.

Notas de implementação (`crates/usbmux`):

- Os bytes que cheguem na mesma leitura, **a seguir** ao `Result` de um `Connect`, já pertencem
  ao túnel e têm de ser entregues ao leitor do túnel.
- `Result(3)` em `Connect` à porta 23456 = o iPad está ligado, mas a transmissão não está ativa.
- `DeviceID` muda a cada ligação do cabo; o identificador estável é o `SerialNumber` (UDID).
- Diagnóstico sem a app: a porta **62078** (`lockdownd`) existe em qualquer iPad. Um `QueryType`
  (4 bytes big-endian de tamanho + plist) responde sem emparelhamento, o que prova o túnel nos
  dois sentidos. `cargo run -p padstudio-usbmux --example devices -- info`.

---

## 10. Versões

- `HELLO.protocol` = versão mais alta do cliente; `HELLO_ACK.protocol` = versão escolhida (≤).
- O cabeçalho de 24 bytes está congelado na v1. Evoluir = novos tipos, novos campos JSON ou bits de
  flags reservados. Uma mudança no cabeçalho implica v2 e um novo `magic`.

## 11. Vetores de teste

[`protocol/test-vectors/v1.json`](../protocol/test-vectors/v1.json) contém:

- `messages`: cada mensagem em hex, com o cabeçalho e o payload descodificados (`payload_json` para
  tipos JSON, `fields` para tipos binários). Os encoders binários têm de produzir **exatamente** os
  mesmos bytes. Nos tipos JSON compara-se o significado, não os bytes.
- `streams`: várias mensagens concatenadas, para testar o parser incremental com cortes arbitrários
  (incluindo byte a byte).
- `invalid`: entradas que têm de falhar com o código de erro indicado.

Os bytes H.264/HEVC dos vetores são **sintéticos**: têm os tipos de NAL corretos, mas só servem para
testar o enquadramento, não um decoder.
