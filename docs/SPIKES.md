# Spikes do passo 4 — S1, S2, S6

Três dúvidas que só se resolvem no iPad real, antes de fechar o desenho do lado iPad
(ARCHITECTURE.md §6). A extensão mede tudo sozinha e envia os resultados ao PC a cada segundo,
dentro de `SENDER_STATS.diag` (PROTOCOL.md §5.6.1).

| Spike | Pergunta | Onde se vê |
|---|---|---|
| **S1** | O ReplayKit entrega mesmo 120 fps a uma Broadcast Extension? | `diag.capture` |
| **S2** | Escalar dentro do VideoToolbox (ex.: 2388×1668 → 1920×1340) pesa no iPad? | `diag.encode` + `diag.cpu_percent` |
| **S6** | A extensão fica folgada abaixo do limite de memória (~50 MB)? | `diag.memory` |

Os resultados aparecem:

- na app do PC, separador **iPad → Diagnóstico do iPad**;
- no `probe`, uma linha por segundo (exemplos abaixo);
- no Mac, em Console.app (subsistema `PadStudio`), se o iPad estiver ligado ao Mac.

---

## 1. O que já está validado sem iPad

O núcleo Swift (`ipad-sender/Packages/PadStudioKit`) compila e é testado em Windows:

- **Testes:** 51 testes passam, incluindo os vetores partilhados do protocolo (bytes iguais aos do Rust e da referência JS), o Anexo-B, a fila de envio, os keyframes, a sessão completa e o TCP real.
- **iPad simulado em Swift (`padstudio-sim`):** usa exatamente o mesmo código de sessão da extensão.
  - Contra o `probe` e a app do PC: 120 fps, latência p50 de 0,2 ms em localhost e zero erros de descodificação em 1513 frames.
  - O teste incluiu um pedido de keyframe, um `CONFIGURE` e 1,5 s com o PC parado (168 frames descartados, recuperação com `DISCONTINUITY`).

**Falta a parte que só existe num iPad:** o encoder VideoToolbox, as métricas do sistema e o `SampleHandler`. Estão escritos, mas só compilam com o SDK da Apple.

## 2. Preparar

### 2.1 Sem Mac: GitHub Actions + Sideloadly (Windows)

O workflow [`.github/workflows/build.yml`](../.github/workflows/build.yml) compila a app num
runner macOS do GitHub e produz um `.ipa` **sem assinatura**. A assinatura acontece na instalação,
no Windows, com o teu Apple ID (gratuito serve).

**1. Pôr o projeto no GitHub** (uma vez):

1. Instalar o Git (`winget install Git.Git`) e identificar-se uma vez:
   `git config --global user.name "O teu nome"` e `git config --global user.email "o-teu@email"`.
2. Criar um repositório **público** no GitHub. Num repositório privado o macOS gasta 10× os minutos gratuitos.
3. Enviar o projeto a partir da pasta raiz:

```bash
git init -b main
```

```bash
git add . && git commit -m "PadStudio"
```

```bash
git remote add origin https://github.com/<utilizador>/<repositório>.git && git push -u origin main
```

As chaves de stream nunca estão no projeto (ficam no cofre do Windows), por isso não há segredos a publicar.

**2. Compilar:**

- Cada envio que mexa em `ipad-sender/` ou `protocol/` compila sozinho.
- Para escolher o prefixo dos bundle IDs: separador **Actions** → **iPad · build** → **Run workflow**, por exemplo com `com.<o-teu-nome>.padstudio`.

**3. Descarregar:** abrir a execução e, em **Artifacts**, descarregar `PadStudio-ipa-N`. É um
`.zip` com `PadStudio-unsigned.ipa` e `BUILD_INFO.txt`. Com uma tag `ipad-v0.1.0`, o `.ipa` fica
também numa Release.

**4. Instalar no iPad com o [Sideloadly](https://sideloadly.io):**

1. Instalar o iTunes e o iCloud da versão do site da Apple. Segundo os guias do Sideloadly, as versões da Microsoft Store não servem.
2. No iPad, ativar o modo de programador: Definições → Privacidade e segurança → Modo de programador. O iPad reinicia.
3. Ligar o iPad por cabo e abrir o Sideloadly. Arrastar o `.ipa`, escrever o Apple ID e carregar em **Start**.
   - **Mantém a extensão:** não removas os *App Extensions / PlugIns* nas opções avançadas, porque é lá que está a extensão de transmissão.
   - **Bundle ID ocupado:** se o Sideloadly disser que o bundle ID já existe, volta a compilar com outro prefixo (passo 2).
4. No iPad, a primeira vez: Definições → Geral → VPN e gestão de dispositivos → confiar no teu Apple ID.
5. Com um Apple ID gratuito a app dura **7 dias**; depois reinstala-se da mesma forma. O ID conta 2 por instalação (app + extensão), num máximo de 10 a cada 7 dias.

**Erros de login no Sideloadly** (aparecem como «Guru Meditation … Login failed»):

- **`-22406`:** palavra-passe errada ou demasiadas tentativas. Confirma-a em [account.apple.com](https://account.apple.com) e espera pelo menos 10 minutos antes de tentar de novo.
- **`-80046` («This action cannot be completed at this time»):** a Apple recusou o login.
  - Costuma vir depois de várias tentativas falhadas. Espera algumas horas, sem tentar.
  - Em Advanced options → *Anisette Authentication*, troca de modo. O modo local precisa do iCloud para Windows na versão do site da Apple.
  - Se o Apple ID tiver autenticação de dois fatores, escreve o código quando o Sideloadly o pedir.
  - Alternativa: usar um Apple ID secundário só para instalar apps.
- **«bundle ID não disponível»:** compila de novo com outro prefixo (Run workflow → `bundle_prefix`).

**Se o build falhar:** o código só-Apple (encoder, métricas, extensão) é compilado pela primeira
vez no runner. Os erros aparecem no passo «Testes do PadStudioKit (macOS)» ou «Compilar para iOS»;
copia o log da execução para se corrigir.

### 2.2 Com um Mac

1. Mac com Xcode 16 ou mais recente e `brew install xcodegen`.
2. Em `ipad-sender/Config/Shared.xcconfig`, preencher `DEVELOPMENT_TEAM` e `PADSTUDIO_BUNDLE_PREFIX`. Um Apple ID gratuito serve: o projeto já não usa App Groups.
3. No Mac, confirmar que o pacote compila e passa os testes:

```bash
cd ipad-sender/Packages/PadStudioKit && swift test
```

4. Gerar o projeto Xcode:

```bash
cd ipad-sender && xcodegen generate && open PadStudio.xcodeproj
```

5. No iPad, ativar o modo de programador: Definições → Privacidade e segurança.
6. Correr o esquema `PadStudioHost` no iPad.
7. Na primeira vez, confiar no programador: Definições → Geral → VPN e gestão de dispositivos.
8. Ligar o iPad ao PC Windows por cabo e tocar no botão da app → **Iniciar transmissão**.

## 3. Medir

No PC (pasta `pc-studio-receiver`), o `probe` liga-se ao iPad por USB e espera pela transmissão:

```bash
cargo run -p padstudio-probe -- --usb --wait --duration 60
```

Linha por segundo (S1 e S2 vêm do iPad simulado; S6 e CPU só existem no iPad, os valores são ilustrativos):

```
      │ S1 captura 120.0 fps · intervalo p50 8.4 / p95 9.1 / máx 9.3 ms [<6:0 6-10:119 10-14:0 14-20:0 20-40:0 ≥40:0] · atraso PTS→callback 0.0 ms · 2388x1668 420f
      │ S2 encoder h264 1920x1340 (escalado de 2388x1668) · 0.0 / p95 0.1 / máx 0.2 ms · em voo 1 · erros 0 · descartes VT 0
      │ S6 memória 21.7 MB · pico 24.0 · limite 50.0
      │ CPU 12 %
```

### S1 — ritmo de captura

1. Abrir um jogo a 120 Hz (ou fazer scroll rápido no Safari, que também usa ProMotion).
2. Observar `S1 captura … fps` e o histograma: a 120 Hz os intervalos caem em `6-10`; a 60 Hz caem em `14-20`.
3. Deixar o ecrã parado alguns segundos. A captura deve cair para perto de 0 e aparecerem
   `repetidos` (com `min_fps` = 10, o PC continua a receber 10 fps).
4. Rodar o iPad nas quatro orientações e confirmar que a imagem no PC fica direita. Anotar o valor
   `orientação EXIF n → graus` de cada uma. Isto valida a tabela de PROTOCOL.md §4.4.
5. `atraso PTS→callback` deve ficar entre 0 e ~20 ms. Valores enormes significariam que o PTS do
   ReplayKit não está no host time, e a latência medida no PC estaria errada.

**Decisão:**

- **≥ 110 fps em jogo:** mantém-se 120 fps por omissão.
- **ReplayKit limitado a 60:** passa a 60 fps por omissão, e o PC deixa de anunciar 120.

### S2 — custo da escala no VideoToolbox

Começar escalado (caixa 1920×1440, a predefinição) e mudar para nativo aos 30 s, com o mesmo jogo
aberto:

```bash
cargo run -p padstudio-probe -- --usb --wait --duration 60 --switch-at 30 --switch-max-width 0 --switch-max-height 0
```

Comparar as duas metades:

- **Tempo do encoder:** `encoder … p50 / p95 / máx`.
- **CPU da extensão:** `CPU`.
- **Fps do próprio jogo:** a captura do S1 é o ritmo a que o jogo desenha. Se cair com a escala, a escala está a roubar GPU ao jogo.
- **Estado térmico:** depois de 10 minutos.

Opcional: com o iPad ligado ao Mac, Xcode → Debug → Attach to Process → `PadStudioBroadcast`,
e Instruments → *Metal System Trace* para ver se aparece trabalho de GPU da extensão.

**Decisão:**

- **Escala mais cara (> 1 ms por frame ou fps do jogo mais baixos):** nativo por omissão. No iPad de referência, H.264 2388×1668 a 120 fps cabe no Level 5.2. A escala fica só para iPads maiores ou para a rede local.
- **Escala barata:** mantém-se a caixa 1920×1440, que poupa bitrate.

### S6 — memória

1. Medir em regime normal (`S6 memória … pico … limite`).
2. Pressão máxima: HEVC a 80 Mbps, e o PC para de ler durante 3 s (a fila de 8 MiB enche e a política de descarte entra em ação):

```bash
cargo run -p padstudio-probe -- --usb --wait --duration 40 --codec hevc --bitrate 80000000 --stall-at 15 --stall-for 3
```

3. Custo de guardar o último frame para o ecrã parado: repetir sem repetição e comparar o pico:

```bash
cargo run -p padstudio-probe -- --usb --wait --duration 30 --min-fps 0
```

**Decisão:**

- **Pico abaixo de ~70 % do limite:** fica como está.
- **Acima disso:** baixar o limite da fila (8 MiB → 4 MiB) e/ou desligar `min_fps` por omissão.
- **Se a extensão morrer** (a transmissão para sozinha): procurar `jetsam` em Console.app.

### Outras coisas a confirmar de caminho

- **`diag.notes`:** lista as propriedades do VideoToolbox que o iPad recusou (ex.: `MaxFrameDelayCount`). Se houver alguma, anotar.
- **Teste longo:** 10 minutos de jogo sem `drops` e com estado térmico `nominal`/`fair`.

## 4. O que enviar de volta

| | Valor |
|---|---|
| Captura em jogo (fps, histograma) | |
| Captura com ecrã parado (repetidos) | |
| Orientações: EXIF → imagem direita? | |
| Atraso PTS → callback (ms) | |
| Encoder escalado vs. nativo (p50/p95 ms, CPU %) | |
| fps do jogo escalado vs. nativo | |
| Memória: normal / pico com pressão / com `--min-fps 0` / limite | |
| `diag.notes` | |
| Estado térmico após 10 min | |

## 5. Outras formas de compilar

A app iOS precisa do SDK da Apple. O caminho recomendado sem Mac é o do §2.1 (GitHub Actions +
Sideloadly). Alternativas:

1. **Um Mac emprestado**, uma vez (§2.2). Com um Apple ID gratuito a app fica instalada 7 dias.
2. **Mac na nuvem** (ex.: MacinCloud, AWS EC2 Mac). O iPad tem de estar ligado a esse Mac para instalar, o que é difícil. Serve sobretudo para compilar.
3. **[xtool](https://github.com/xtool-org/xtool)** (experimental; instável com extensões): compila apps iOS a partir de pacotes SwiftPM em Linux/WSL e instala-as com um Apple ID.
   - Já aceita extensões em `xtool.yml`, com o ponto de entrada `_NSExtensionMain` que uma Broadcast Upload Extension usa. Há um [problema conhecido com extensões de widget](https://github.com/xtool-org/xtool/issues/313).
   - Precisa do SDK extraído do `Xcode.xip`, que se descarrega com uma conta Apple.
   - Exigiria um manifesto próprio além do `project.yml`. Ainda não foi testado com este projeto.
