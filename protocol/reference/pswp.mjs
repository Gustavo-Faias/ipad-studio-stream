// PSWP v1 — implementação de referência (JavaScript, sem dependências).
//
// Especificação: docs/PROTOCOL.md. Este módulo gera e verifica os vetores de teste
// (gen-vectors.mjs) e serve de modelo às implementações em Rust, Swift e TypeScript.
// É deliberadamente simples, não otimizado.

export const MAGIC0 = 0x50; // 'P'
export const MAGIC1 = 0x53; // 'S'
export const HEADER_LEN = 24;
export const MAX_PAYLOAD = 16 * 1024 * 1024;

export const Type = Object.freeze({
  HELLO: 0x01,
  HELLO_ACK: 0x02,
  PING: 0x03,
  PONG: 0x04,
  BYE: 0x05,
  ERROR: 0x06,
  SENDER_STATE: 0x07,
  VIDEO_CONFIG: 0x10,
  VIDEO_FRAME: 0x11,
  AUDIO_CONFIG: 0x12,
  AUDIO_FRAME: 0x13,
  SENDER_STATS: 0x20,
  RECEIVER_REPORT: 0x21,
  CONFIGURE: 0x30,
  CONFIGURE_ACK: 0x31,
  REQUEST_KEYFRAME: 0x32,
  SET_BITRATE: 0x33,
});

const TYPE_NAMES = new Map(Object.entries(Type).map(([name, value]) => [value, name]));

/** Nome do tipo, ou null se for desconhecido (e, portanto, para ignorar). */
export const typeName = (type) => TYPE_NAMES.get(type) ?? null;

export const JSON_TYPES = new Set([
  Type.HELLO, Type.HELLO_ACK, Type.BYE, Type.ERROR, Type.SENDER_STATE,
  Type.SENDER_STATS, Type.RECEIVER_REPORT, Type.CONFIGURE, Type.CONFIGURE_ACK,
]);

export const VideoFlag = Object.freeze({ KEYFRAME: 0x01, PARAMS_INBAND: 0x02, DISCONTINUITY: 0x04 });
export const ROTATION_SHIFT = 4;
export const ROTATION_MASK = 0x30;
export const AudioFlag = Object.freeze({ SOURCE_MASK: 0x0f, DISCONTINUITY: 0x10 });

export const Codec = Object.freeze({ H264: 1, HEVC: 2 });
export const AudioSource = Object.freeze({ APP: 0, MIC: 1 });
export const AudioFormat = Object.freeze({ S16LE: 1, F32LE: 2 });

export class ProtocolError extends Error {
  /** @param {'bad_magic'|'payload_too_large'|'bad_payload'} code */
  constructor(code, message) {
    super(message ?? code);
    this.name = 'ProtocolError';
    this.code = code;
  }
}

// ── Enquadramento ────────────────────────────────────────────────────────────

export function encodeMessage({ type, flags = 0, seq = 0, aux = 0, ptsUs = 0n, payload = Buffer.alloc(0) }) {
  if (payload.length > MAX_PAYLOAD) throw new ProtocolError('payload_too_large');
  const buf = Buffer.alloc(HEADER_LEN + payload.length);
  buf[0] = MAGIC0;
  buf[1] = MAGIC1;
  buf.writeUInt8(type, 2);
  buf.writeUInt8(flags, 3);
  buf.writeUInt32LE(payload.length, 4);
  buf.writeUInt32LE(seq >>> 0, 8);
  buf.writeUInt32LE(aux >>> 0, 12);
  buf.writeBigUInt64LE(BigInt(ptsUs), 16);
  payload.copy(buf, HEADER_LEN);
  return buf;
}

function checkMagic(buf, offset) {
  if (buf[offset] !== MAGIC0 || buf[offset + 1] !== MAGIC1) throw new ProtocolError('bad_magic');
}

export function decodeHeader(buf, offset = 0) {
  if (buf.length - offset < HEADER_LEN) throw new RangeError('cabeçalho incompleto');
  checkMagic(buf, offset);
  const payloadLen = buf.readUInt32LE(offset + 4);
  if (payloadLen > MAX_PAYLOAD) throw new ProtocolError('payload_too_large');
  return {
    type: buf[offset + 2],
    flags: buf[offset + 3],
    payloadLen,
    seq: buf.readUInt32LE(offset + 8),
    aux: buf.readUInt32LE(offset + 12),
    ptsUs: buf.readBigUInt64LE(offset + 16),
  };
}

/** Parser incremental: aceita pedaços de qualquer tamanho e devolve as mensagens completas. */
export class StreamParser {
  #buf = Buffer.alloc(0);

  push(chunk) {
    this.#buf = this.#buf.length ? Buffer.concat([this.#buf, chunk]) : Buffer.from(chunk);
    const out = [];
    for (;;) {
      if (this.#buf.length >= 2) checkMagic(this.#buf, 0); // falha cedo, sem esperar pelos 24 bytes
      if (this.#buf.length < HEADER_LEN) break;
      const header = decodeHeader(this.#buf, 0);
      const total = HEADER_LEN + header.payloadLen;
      if (this.#buf.length < total) break;
      out.push({ ...header, payload: this.#buf.subarray(HEADER_LEN, total) });
      this.#buf = this.#buf.subarray(total);
    }
    return out;
  }

  /** Bytes recebidos que ainda não formam uma mensagem completa. */
  get pending() {
    return this.#buf.length;
  }
}

// ── Flags ────────────────────────────────────────────────────────────────────

export function videoFlags({ keyframe = false, paramsInband = false, discontinuity = false, rotation = 0 }) {
  return (keyframe ? VideoFlag.KEYFRAME : 0)
    | (paramsInband ? VideoFlag.PARAMS_INBAND : 0)
    | (discontinuity ? VideoFlag.DISCONTINUITY : 0)
    | ((rotation & 0x3) << ROTATION_SHIFT);
}

export function parseVideoFlags(flags) {
  return {
    keyframe: (flags & VideoFlag.KEYFRAME) !== 0,
    paramsInband: (flags & VideoFlag.PARAMS_INBAND) !== 0,
    discontinuity: (flags & VideoFlag.DISCONTINUITY) !== 0,
    rotation: (flags & ROTATION_MASK) >> ROTATION_SHIFT,
  };
}

export const audioFlags = ({ source, discontinuity = false }) =>
  (source & AudioFlag.SOURCE_MASK) | (discontinuity ? AudioFlag.DISCONTINUITY : 0);

export const parseAudioFlags = (flags) => ({
  source: flags & AudioFlag.SOURCE_MASK,
  discontinuity: (flags & AudioFlag.DISCONTINUITY) !== 0,
});

// ── Payloads ─────────────────────────────────────────────────────────────────

function need(payload, len) {
  if (payload.length < len) throw new ProtocolError('bad_payload', `payload com ${payload.length} bytes; mínimo ${len}`);
}

export const encodeJson = (obj) => Buffer.from(JSON.stringify(obj), 'utf8');
export const decodeJson = (payload) => JSON.parse(payload.toString('utf8'));

export function encodePing({ t0 }) {
  const b = Buffer.alloc(8);
  b.writeBigUInt64LE(BigInt(t0), 0);
  return b;
}

export function decodePing(p) {
  need(p, 8);
  return { t0: p.readBigUInt64LE(0) };
}

export function encodePong({ t0, t1, t2 }) {
  const b = Buffer.alloc(24);
  b.writeBigUInt64LE(BigInt(t0), 0);
  b.writeBigUInt64LE(BigInt(t1), 8);
  b.writeBigUInt64LE(BigInt(t2), 16);
  return b;
}

export function decodePong(p) {
  need(p, 24);
  return { t0: p.readBigUInt64LE(0), t1: p.readBigUInt64LE(8), t2: p.readBigUInt64LE(16) };
}

export function encodeVideoConfig(c) {
  const b = Buffer.alloc(16 + c.paramSets.length);
  b.writeUInt8(c.codec, 0);
  b.writeUInt8(c.bitDepth, 1);
  b.writeUInt16LE(c.fpsMax, 2);
  b.writeUInt16LE(c.width, 4);
  b.writeUInt16LE(c.height, 6);
  b.writeUInt32LE(c.bitrateBps, 8);
  b.writeUInt8(c.colorPrimaries, 12);
  b.writeUInt8(c.transfer, 13);
  b.writeUInt8(c.matrix, 14);
  b.writeUInt8(c.fullRange ? 1 : 0, 15);
  c.paramSets.copy(b, 16);
  return b;
}

export function decodeVideoConfig(p) {
  need(p, 16);
  return {
    codec: p[0],
    bitDepth: p[1],
    fpsMax: p.readUInt16LE(2),
    width: p.readUInt16LE(4),
    height: p.readUInt16LE(6),
    bitrateBps: p.readUInt32LE(8),
    colorPrimaries: p[12],
    transfer: p[13],
    matrix: p[14],
    fullRange: p[15] === 1,
    paramSets: p.subarray(16),
  };
}

export function encodeAudioConfig({ source, format, channels, sampleRate }) {
  const b = Buffer.alloc(8);
  b.writeUInt8(source, 0);
  b.writeUInt8(format, 1);
  b.writeUInt8(channels, 2);
  b.writeUInt32LE(sampleRate, 4);
  return b;
}

export function decodeAudioConfig(p) {
  need(p, 8);
  return { source: p[0], format: p[1], channels: p[2], sampleRate: p.readUInt32LE(4) };
}

export function encodeSetBitrate({ bitrateBps }) {
  const b = Buffer.alloc(4);
  b.writeUInt32LE(bitrateBps, 0);
  return b;
}

export function decodeSetBitrate(p) {
  need(p, 4);
  return { bitrateBps: p.readUInt32LE(0) };
}

// ── Anexo-B ──────────────────────────────────────────────────────────────────

/** Divide um access unit Anexo-B em NAL units (sem start codes). */
export function splitAnnexB(au) {
  const nals = [];
  let start = -1;
  let i = 0;
  while (i + 2 < au.length) {
    if (au[i] === 0 && au[i + 1] === 0 && au[i + 2] === 1) {
      if (start >= 0) {
        let end = i;
        while (end > start && au[end - 1] === 0) end--; // zero inicial do start code de 4 bytes
        nals.push(au.subarray(start, end));
      }
      i += 3;
      start = i;
    } else {
      i++;
    }
  }
  if (start >= 0 && start < au.length) nals.push(au.subarray(start));
  return nals;
}

/** Tipo de NAL: H.264 → 5 bits baixos; HEVC → bits 1..6 do primeiro byte. */
export const nalType = (codec, nal) => (codec === Codec.HEVC ? (nal[0] >> 1) & 0x3f : nal[0] & 0x1f);
