// Preview de baixa latência: WebSocket local → WebCodecs (descodificação por hardware) → canvas.
//
// - Guarda só o frame descodificado mais recente ("o último ganha") e desenha-o a cada
//   requestAnimationFrame: no máximo 1 frame de buffer, sem tearing (o compositor sincroniza).
// - A rotação vem por frame e é aplicada no canvas (GPU), não no iPad.
// - Se o descodificador ficar para trás ou falhar, descarta até ao próximo keyframe e volta a
//   ligar (o backend pede um keyframe ao iPad a cada nova ligação).

export interface PreviewStats {
  /// Frames desenhados no último segundo.
  fps: number;
  /// Tempo médio de descodificação (ms).
  decodeMs: number;
  width: number;
  height: number;
  rotation: number;
  codec: string | null;
  /// Falta de suporte do WebView para o codec, ou outro problema.
  error: string | null;
  connected: boolean;
  hasVideo: boolean;
}

type Listener = (s: PreviewStats) => void;

const HEADER = 10;
const MAX_DECODE_QUEUE = 3;

export class PreviewPlayer {
  private ws: WebSocket | null = null;
  private decoder: VideoDecoder | null = null;
  private latest: VideoFrame | null = null;
  private latestRotation = 0;
  private dirty = false;
  private waitKey = true;
  private raf = 0;
  private closed = false;
  private reconnectTimer = 0;
  private config: { codec: string; width: number; height: number } | null = null;
  private pending = new Map<number, number>();
  private decodeTimes: number[] = [];
  private drawn = 0;
  private statsTimer = 0;
  private stats: PreviewStats = {
    fps: 0,
    decodeMs: 0,
    width: 0,
    height: 0,
    rotation: 0,
    codec: null,
    error: null,
    connected: false,
    hasVideo: false,
  };

  constructor(
    private canvas: HTMLCanvasElement,
    private url: string,
    private onStats: Listener,
  ) {
    this.connect();
    this.raf = requestAnimationFrame(this.draw);
    this.statsTimer = window.setInterval(this.publishStats, 1000);
  }

  destroy() {
    this.closed = true;
    cancelAnimationFrame(this.raf);
    clearInterval(this.statsTimer);
    clearTimeout(this.reconnectTimer);
    this.ws?.close();
    this.resetDecoder();
  }

  private connect = () => {
    if (this.closed) return;
    const ws = new WebSocket(this.url);
    ws.binaryType = "arraybuffer";
    ws.onopen = () => this.update({ connected: true });
    ws.onclose = () => {
      this.update({ connected: false });
      if (!this.closed) this.reconnectTimer = window.setTimeout(this.connect, 500);
    };
    ws.onmessage = (ev) => (typeof ev.data === "string" ? this.onText(ev.data) : this.onFrame(ev.data as ArrayBuffer));
    this.ws = ws;
    this.waitKey = true;
  };

  private reconnect() {
    this.resetDecoder();
    this.ws?.close(); // onclose volta a ligar → o backend pede um keyframe
  }

  private update(p: Partial<PreviewStats>) {
    this.stats = { ...this.stats, ...p };
    this.onStats(this.stats);
  }

  private publishStats = () => {
    const avg = this.decodeTimes.length ? this.decodeTimes.reduce((a, b) => a + b, 0) / this.decodeTimes.length : 0;
    this.decodeTimes = [];
    this.update({ fps: this.drawn, decodeMs: avg });
    this.drawn = 0;
  };

  private onText(text: string) {
    const msg = JSON.parse(text) as { type: string; codec?: string | null; width?: number; height?: number };
    if (msg.type === "config") {
      if (!msg.codec) {
        this.update({ error: "Não foi possível identificar o codec do vídeo." });
        return;
      }
      this.config = { codec: msg.codec, width: msg.width ?? 0, height: msg.height ?? 0 };
      this.configure();
    } else if (msg.type === "ended") {
      this.waitKey = true;
      this.update({ hasVideo: false });
    }
  }

  private async configure() {
    const cfg = this.config;
    if (!cfg) return;
    const config: VideoDecoderConfig = {
      codec: cfg.codec,
      codedWidth: cfg.width,
      codedHeight: cfg.height,
      hardwareAcceleration: "prefer-hardware",
      optimizeForLatency: true,
    };
    const support = await VideoDecoder.isConfigSupported(config).catch(() => ({ supported: false }));
    if (!support.supported) {
      this.update({ error: `Este computador não descodifica ${cfg.codec} no WebView. Muda o codec do iPad para H.264.` });
      return;
    }
    this.resetDecoder();
    this.decoder = new VideoDecoder({
      output: (frame) => {
        const t0 = this.pending.get(frame.timestamp);
        if (t0 !== undefined) {
          this.decodeTimes.push(performance.now() - t0);
          this.pending.delete(frame.timestamp);
        }
        this.latest?.close();
        this.latest = frame;
        this.dirty = true;
      },
      error: (e) => {
        this.update({ error: `Erro do descodificador: ${e.message}` });
        this.reconnect();
      },
    });
    this.decoder.configure(config);
    this.waitKey = true;
    this.update({ codec: cfg.codec, width: cfg.width, height: cfg.height, error: null });
  }

  private onFrame(buf: ArrayBuffer) {
    const d = this.decoder;
    if (!d || d.state !== "configured") return;
    const bytes = new Uint8Array(buf);
    const key = (bytes[0] & 1) === 1;
    const rotation = (bytes[1] & 3) * 90;
    const timestamp = Number(new DataView(buf).getBigUint64(2, true));
    if (this.waitKey && !key) return;
    if (d.decodeQueueSize > MAX_DECODE_QUEUE) {
      // O descodificador não está a acompanhar: salta para o próximo keyframe.
      this.waitKey = true;
      if (!key) return;
    }
    this.waitKey = false;
    this.latestRotation = rotation;
    this.pending.set(timestamp, performance.now());
    if (this.pending.size > 64) this.pending.clear();
    try {
      d.decode(new EncodedVideoChunk({ type: key ? "key" : "delta", timestamp, data: bytes.subarray(HEADER) }));
    } catch {
      this.reconnect();
    }
  }

  private resetDecoder() {
    if (this.decoder && this.decoder.state !== "closed") this.decoder.close();
    this.decoder = null;
    this.latest?.close();
    this.latest = null;
    this.pending.clear();
  }

  private draw = () => {
    this.raf = requestAnimationFrame(this.draw);
    const frame = this.latest;
    if (!frame || !this.dirty) return;
    this.dirty = false;
    this.drawn++;

    const canvas = this.canvas;
    const dpr = window.devicePixelRatio || 1;
    const cw = Math.round(canvas.clientWidth * dpr);
    const ch = Math.round(canvas.clientHeight * dpr);
    if (canvas.width !== cw || canvas.height !== ch) {
      canvas.width = cw;
      canvas.height = ch;
    }
    const ctx = canvas.getContext("2d");
    if (!ctx) return;
    const rot = this.latestRotation;
    const fw = frame.displayWidth;
    const fh = frame.displayHeight;
    const swap = rot === 90 || rot === 270;
    const [rw, rh] = swap ? [fh, fw] : [fw, fh];
    const scale = Math.min(cw / rw, ch / rh);
    ctx.setTransform(1, 0, 0, 1, 0, 0);
    ctx.fillStyle = "#000";
    ctx.fillRect(0, 0, cw, ch);
    ctx.translate(cw / 2, ch / 2);
    ctx.rotate((rot * Math.PI) / 180);
    ctx.drawImage(frame, (-fw * scale) / 2, (-fh * scale) / 2, fw * scale, fh * scale);
    if (!this.stats.hasVideo || this.stats.rotation !== rot) this.update({ hasVideo: true, rotation: rot });
  };
}
