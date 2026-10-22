#!/usr/bin/env node
// Gera e verifica protocol/test-vectors/v1.json (vetores PSWP partilhados por Rust e Swift).
//
//   node protocol/reference/gen-vectors.mjs           → (re)gera o ficheiro e verifica-o
//   node protocol/reference/gen-vectors.mjs --check   → só verifica (falha se estiver desatualizado)

import assert from 'node:assert/strict';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import * as P from './pswp.mjs';

const T = P.Type;
const OUT = join(dirname(fileURLToPath(import.meta.url)), '..', 'test-vectors', 'v1.json');

const hex = (s) => Buffer.from(s.replace(/\s+/g, ''), 'hex');
const toHex = (b) => Buffer.from(b).toString('hex');

// ── Bytes de vídeo sintéticos (tipos de NAL corretos; só testam o enquadramento) ──
const SC = '00000001';
const H264_SPS = `${SC} 6764 0034 ac56 501e 005a 20`;
const H264_PPS = `${SC} 68ee 3cb0`;
const H264_IDR = `${SC} 6588 8400 33ff fe8a 9b12 7c`;
const H264_P = `${SC} 419a 020c 5f71 e3`;
const HEVC_VPS = `${SC} 4001 0c01 ffff 0160`;
const HEVC_SPS = `${SC} 4201 0101 6000 0003 00b0`; // contém 00 00 03 (emulation prevention): não é start code
const HEVC_PPS = `${SC} 4401 c172 b462 40`;

const PTS0 = 86_400_010_000; // ~1 dia de uptime do iPad, em µs

const CONFIG_USB = {
  codec: 'h264', max_fps: 120, min_fps: 10, bitrate_bps: 50_000_000, bitrate_mode: 'vbr',
  max_width: 1920, max_height: 1440, keyframe_interval_s: 0, audio_app: true, audio_mic: false,
};

// ── Payloads binários: campos (snake_case, como nos vetores) ⇄ bytes ──────────
const BINARY = {
  [T.PING]: {
    encode: (f) => P.encodePing({ t0: f.t0 }),
    decode: (p) => ({ t0: Number(P.decodePing(p).t0) }),
  },
  [T.PONG]: {
    encode: (f) => P.encodePong(f),
    decode: (p) => {
      const d = P.decodePong(p);
      return { t0: Number(d.t0), t1: Number(d.t1), t2: Number(d.t2) };
    },
  },
  [T.VIDEO_CONFIG]: {
    encode: (f) => P.encodeVideoConfig({
      codec: f.codec, bitDepth: f.bit_depth, fpsMax: f.fps_max, width: f.width, height: f.height,
      bitrateBps: f.bitrate_bps, colorPrimaries: f.color_primaries, transfer: f.transfer,
      matrix: f.matrix, fullRange: f.full_range, paramSets: hex(f.param_sets_hex),
    }),
    decode: (p) => {
      const d = P.decodeVideoConfig(p);
      return {
        codec: d.codec, bit_depth: d.bitDepth, fps_max: d.fpsMax, width: d.width, height: d.height,
        bitrate_bps: d.bitrateBps, color_primaries: d.colorPrimaries, transfer: d.transfer,
        matrix: d.matrix, full_range: d.fullRange, param_sets_hex: toHex(d.paramSets),
        nal_types: P.splitAnnexB(d.paramSets).map((n) => P.nalType(d.codec, n)),
      };
    },
  },
  [T.AUDIO_CONFIG]: {
    encode: (f) => P.encodeAudioConfig({ source: f.source, format: f.format, channels: f.channels, sampleRate: f.sample_rate }),
    decode: (p) => {
      const d = P.decodeAudioConfig(p);
      return { source: d.source, format: d.format, channels: d.channels, sample_rate: d.sampleRate };
    },
  },
  [T.SET_BITRATE]: {
    encode: (f) => P.encodeSetBitrate({ bitrateBps: f.bitrate_bps }),
    decode: (p) => ({ bitrate_bps: P.decodeSetBitrate(p).bitrateBps }),
  },
};

// ── Definição dos vetores ────────────────────────────────────────────────────
function buildDoc() {
  const messages = [];

  /** Cada vetor: cabeçalho + um de { json | fields (binário conhecido) | raw (bytes opacos) }. */
  function add(name, description, h, body = {}) {
    let payload;
    let payloadKind;
    if (body.json !== undefined) {
      payload = P.encodeJson(body.json);
      payloadKind = 'json';
    } else if (body.fields !== undefined) {
      payload = BINARY[h.type].encode(body.fields);
      payloadKind = 'binary';
    } else if (body.raw !== undefined) {
      payload = hex(body.raw);
      payloadKind = payload.length ? 'raw' : 'empty';
    } else {
      payload = Buffer.alloc(0);
      payloadKind = 'empty';
    }
    const flags = h.flags ?? 0;
    const bytes = P.encodeMessage({ type: h.type, flags, seq: h.seq ?? 0, aux: h.aux ?? 0, ptsUs: BigInt(h.pts_us ?? 0), payload });
    const v = {
      name,
      description,
      hex: toHex(bytes),
      header: {
        type: h.type,
        type_name: P.typeName(h.type),
        flags,
        payload_len: payload.length,
        seq: h.seq ?? 0,
        aux: h.aux ?? 0,
        pts_us: h.pts_us ?? 0,
      },
      payload_kind: payloadKind,
      payload_hex: toHex(payload),
    };
    if (payloadKind === 'json') v.payload_json = body.json;
    if (payloadKind === 'binary') v.fields = BINARY[h.type].decode(payload);
    if (h.type === T.VIDEO_FRAME) {
      const f = P.parseVideoFlags(flags);
      v.flags_decoded = { keyframe: f.keyframe, params_inband: f.paramsInband, discontinuity: f.discontinuity, rotation: f.rotation };
      v.context_codec = body.codec;
      v.nal_types = P.splitAnnexB(payload).map((n) => P.nalType(body.codec, n));
    }
    if (h.type === T.AUDIO_FRAME) v.flags_decoded = P.parseAudioFlags(flags);
    messages.push(v);
  }

  // Sessão
  add('hello_usb', 'HELLO do PC por USB com configuração inicial', { type: T.HELLO, pts_us: 1_250_000 }, {
    json: {
      protocol: 1, client: 'pc-studio-receiver', client_version: '0.1.0', transport: 'usb',
      want: { video: true, audio_app: true, audio_mic: false },
      config: { codec: 'h264', max_fps: 120, bitrate_bps: 50_000_000 },
    },
  });
  add('hello_lan', 'HELLO do PC pela rede local, com PIN', { type: T.HELLO, seq: 1, pts_us: 9_000_000 }, {
    json: { protocol: 1, client: 'pc-studio-receiver', client_version: '0.1.0', transport: 'lan', token: '482913' },
  });
  add('hello_ack', 'Resposta do iPad com dispositivo, codecs e configuração efetiva', { type: T.HELLO_ACK, pts_us: PTS0 - 9_000 }, {
    json: {
      protocol: 1, server: 'PadStudioBroadcast', server_version: '0.1.0',
      session_id: '6F1C2B0E-8C2D-4C9A-9D61-2B1E0A7F3C11', clock: 'host_us',
      device: { model: 'iPad16,6', system: 'iPadOS 26.0', screen_width: 2752, screen_height: 2064, max_fps: 120 },
      codecs: ['h264', 'hevc'],
      config: CONFIG_USB,
    },
  });
  add('ping', 'PING do PC (t0 no relógio do PC)', { type: T.PING, seq: 4 }, { fields: { t0: 5_000_123 } });
  add('pong', 'PONG do iPad (t1/t2 no relógio do iPad)', { type: T.PONG, seq: 4 }, {
    fields: { t0: 5_000_123, t1: 86_400_000_456, t2: 86_400_000_501 },
  });
  add('sender_state_paused', 'A transmissão foi pausada pelo sistema', { type: T.SENDER_STATE, pts_us: PTS0 + 60_000_000 }, {
    json: { state: 'paused' },
  });
  add('bye_replaced', 'Ligação antiga substituída por uma nova', { type: T.BYE, pts_us: PTS0 + 61_000_000 }, {
    json: { reason: 'replaced', message: 'Nova ligação USB aceite' },
  });
  add('error_unsupported', 'Erro não fatal em resposta a um CONFIGURE (ref_type 0x30)', { type: T.ERROR, pts_us: PTS0 + 2_000_000 }, {
    json: { code: 'unsupported_config', message: 'HEVC 10-bit não suportado', fatal: false, ref_type: 0x30, ref_seq: 3 },
  });

  // Vídeo
  add('video_config_h264', 'VIDEO_CONFIG H.264 1920x1440 @120, BT.709 video range, SPS+PPS', { type: T.VIDEO_CONFIG, seq: 1, pts_us: PTS0 }, {
    fields: {
      codec: P.Codec.H264, bit_depth: 8, fps_max: 120, width: 1920, height: 1440, bitrate_bps: 50_000_000,
      color_primaries: 1, transfer: 1, matrix: 1, full_range: false, param_sets_hex: toHex(hex(H264_SPS + H264_PPS)),
    },
  });
  add('video_config_hevc', 'VIDEO_CONFIG HEVC nativo 2752x2064 @120, Display P3, VPS+SPS+PPS', { type: T.VIDEO_CONFIG, seq: 2, pts_us: PTS0 + 5_000_000 }, {
    fields: {
      codec: P.Codec.HEVC, bit_depth: 8, fps_max: 120, width: 2752, height: 2064, bitrate_bps: 80_000_000,
      color_primaries: 12, transfer: 1, matrix: 1, full_range: false, param_sets_hex: toHex(hex(HEVC_VPS + HEVC_SPS + HEVC_PPS)),
    },
  });
  add('video_frame_keyframe', 'Keyframe H.264 com parâmetros in-band, rotação 90° horário', {
    type: T.VIDEO_FRAME, flags: P.videoFlags({ keyframe: true, paramsInband: true, rotation: 1 }), seq: 0, aux: 4_210, pts_us: PTS0,
  }, { raw: H264_SPS + H264_PPS + H264_IDR, codec: P.Codec.H264 });
  add('video_frame_p', 'P-frame H.264 seguinte (8,333 ms depois), rotação 90°', {
    type: T.VIDEO_FRAME, flags: P.videoFlags({ rotation: 1 }), seq: 1, aux: 3_980, pts_us: PTS0 + 8_333,
  }, { raw: H264_P, codec: P.Codec.H264 });
  add('video_frame_discontinuity', 'Keyframe após descarte no emissor: seq salta de 1 para 5 (3 frames perdidos)', {
    type: T.VIDEO_FRAME, flags: P.videoFlags({ keyframe: true, paramsInband: true, discontinuity: true }), seq: 5, aux: 5_120, pts_us: PTS0 + 41_666,
  }, { raw: H264_SPS + H264_PPS + H264_IDR, codec: P.Codec.H264 });
  add('video_frame_edge_values', 'Valores limite: seq e aux = u32 máximo, pts_us = 2^53 − 1', {
    type: T.VIDEO_FRAME, flags: 0, seq: 0xffffffff, aux: 0xffffffff, pts_us: Number.MAX_SAFE_INTEGER,
  }, { raw: H264_P, codec: P.Codec.H264 });

  // Áudio
  add('audio_config_app', 'Áudio da app: PCM s16le estéreo 48 kHz', { type: T.AUDIO_CONFIG }, {
    fields: { source: P.AudioSource.APP, format: P.AudioFormat.S16LE, channels: 2, sample_rate: 48_000 },
  });
  add('audio_config_mic', 'Microfone do iPad: PCM s16le mono 44,1 kHz', { type: T.AUDIO_CONFIG }, {
    fields: { source: P.AudioSource.MIC, format: P.AudioFormat.S16LE, channels: 1, sample_rate: 44_100 },
  });
  add('audio_frame_app', 'Áudio da app: 4 frames estéreo s16le', {
    type: T.AUDIO_FRAME, flags: P.audioFlags({ source: P.AudioSource.APP }), seq: 0, aux: 1_500, pts_us: PTS0 + 500,
  }, { raw: '0000 0000 ff7f 0180 0040 00c0 1234 edcb' });
  add('audio_frame_mic_discontinuity', 'Microfone com descontinuidade: 4 frames mono', {
    type: T.AUDIO_FRAME, flags: P.audioFlags({ source: P.AudioSource.MIC, discontinuity: true }), seq: 7, aux: 2_100, pts_us: PTS0 + 150_000,
  }, { raw: '0100 ffff 0010 00f0' });

  // Telemetria
  add('sender_stats', 'Estatísticas do iPad (1 Hz)', { type: T.SENDER_STATS, seq: 12, pts_us: PTS0 + 12_000_000 }, {
    json: {
      fps_capture: 119.6, fps_encoded: 119.6, bitrate_bps: 41_234_567, target_bitrate_bps: 50_000_000,
      frames_dropped: 3, keyframes: 2, send_queue_bytes: 84_211, send_queue_ms: 1.4, encode_ms_avg: 4.1,
      thermal: 'nominal', memory_mb: 21.7, low_power_mode: false,
    },
  });
  add('receiver_report', 'Relatório do PC (1 Hz)', { type: T.RECEIVER_REPORT, seq: 12, pts_us: 13_250_000 }, {
    json: {
      fps_received: 119.5, fps_decoded: 119.5, recv_bitrate_bps: 41_100_000, latency_ms_p50: 9.8,
      latency_ms_p95: 14.2, rtt_ms: 0.9, decode_errors: 0,
    },
  });

  // Comandos
  add('configure', 'Pedido do PC: HEVC, 60 fps, 20 Mbps', { type: T.CONFIGURE, seq: 3, pts_us: 20_000_000 }, {
    json: { codec: 'hevc', max_fps: 60, bitrate_bps: 20_000_000 },
  });
  add('configure_ack', 'Confirmação do iPad com a configuração efetiva', { type: T.CONFIGURE_ACK, seq: 3, pts_us: PTS0 + 20_000_000 }, {
    json: { request_seq: 3, applied: true, config: { ...CONFIG_USB, codec: 'hevc', max_fps: 60, bitrate_bps: 20_000_000 } },
  });
  add('request_keyframe', 'Pedido de keyframe (payload vazio)', { type: T.REQUEST_KEYFRAME, seq: 9 });
  add('set_bitrate', 'Mudar o bitrate para 35 Mbps em direto', { type: T.SET_BITRATE, seq: 2 }, { fields: { bitrate_bps: 35_000_000 } });

  // Compatibilidade futura
  add('unknown_type_0x9a', 'Tipo desconhecido (faixa experimental): o recetor TEM DE ignorar', { type: 0x9a, flags: 0x01, seq: 1 }, {
    raw: 'deadbeef',
  });

  const byName = new Map(messages.map((m) => [m.name, m]));
  const stream = (name, description, names) => ({
    name,
    description,
    hex: names.map((n) => byName.get(n).hex).join(''),
    messages: names,
    chunk_sizes: [1, 2, 3, 5, 7, 11, 23, 24, 25, 64, 4096],
  });

  const streams = [
    stream('session_start_usb', 'Início de sessão visto pelo PC, com um tipo desconhecido pelo meio', [
      'hello_ack', 'audio_config_app', 'video_config_h264', 'video_frame_keyframe', 'pong',
      'audio_frame_app', 'unknown_type_0x9a', 'video_frame_p', 'sender_stats',
    ]),
    stream('pc_to_ipad', 'Mensagens do PC para o iPad', ['hello_usb', 'ping', 'configure', 'request_keyframe', 'set_bitrate', 'receiver_report']),
  ];

  const header = (type, payloadLen, magic = '5053') =>
    magic + toHex(Buffer.from([type, 0])) + toHex(u32(payloadLen)) + '00'.repeat(16);
  const u32 = (n) => {
    const b = Buffer.alloc(4);
    b.writeUInt32LE(n, 0);
    return b;
  };

  const invalid = [
    { name: 'bad_magic', description: 'Magic "PX" em vez de "PS"', stage: 'header', error: 'bad_magic', hex: header(T.PING, 0, '5058') },
    { name: 'bad_magic_early', description: 'Só 2 bytes errados: o parser incremental deve falhar logo', stage: 'header', error: 'bad_magic', hex: '4745' },
    { name: 'payload_too_large', description: 'payload_len = 16 MiB + 1', stage: 'header', error: 'payload_too_large', hex: header(T.VIDEO_FRAME, P.MAX_PAYLOAD + 1) },
    { name: 'video_config_short', description: 'VIDEO_CONFIG com 10 bytes (mínimo 16)', stage: 'payload', error: 'bad_payload', hex: header(T.VIDEO_CONFIG, 10) + '01'.repeat(10) },
    { name: 'pong_short', description: 'PONG com 16 bytes (mínimo 24)', stage: 'payload', error: 'bad_payload', hex: header(T.PONG, 16) + '02'.repeat(16) },
  ];

  return {
    protocol: 'PSWP',
    version: 1,
    header_len: P.HEADER_LEN,
    max_payload: P.MAX_PAYLOAD,
    generated_by: 'protocol/reference/gen-vectors.mjs',
    notes: [
      'Todos os inteiros do cabeçalho e dos payloads binários são little-endian.',
      'Tipos binários (payload_kind = binary): o encoder TEM DE produzir exatamente payload_hex a partir de fields.',
      'Tipos JSON: compara-se payload_json semanticamente, não os bytes.',
      'payload_kind = raw/empty: bytes opacos (frames de media ou tipos desconhecidos).',
      'streams: concatenar e partir em pedaços de cada chunk_size; o parser incremental tem de devolver as mensagens na ordem de messages e ficar sem bytes pendentes.',
      'invalid.stage = header: o parser incremental tem de falhar com o código indicado; stage = payload: o cabeçalho é válido e a falha ocorre ao descodificar o payload.',
      'Bytes H.264/HEVC sintéticos: tipos de NAL corretos, conteúdo não descodificável.',
    ],
    messages,
    streams,
    invalid,
  };
}

// ── Verificação independente (descodifica a partir do hex) ───────────────────
function decodeBinaryStrict(type, payload) {
  if (BINARY[type]) return BINARY[type].decode(payload);
  return null;
}

function verify(doc) {
  assert.equal(doc.header_len, P.HEADER_LEN);
  let checks = 0;

  for (const v of doc.messages) {
    const ctx = `vetor ${v.name}`;
    const bytes = hex(v.hex);
    const h = P.decodeHeader(bytes);
    assert.equal(h.type, v.header.type, ctx);
    assert.equal(P.typeName(h.type), v.header.type_name, ctx);
    assert.equal(h.flags, v.header.flags, ctx);
    assert.equal(h.payloadLen, v.header.payload_len, ctx);
    assert.equal(h.seq, v.header.seq, ctx);
    assert.equal(h.aux, v.header.aux, ctx);
    assert.equal(h.ptsUs, BigInt(v.header.pts_us), ctx);
    assert.equal(bytes.length, P.HEADER_LEN + h.payloadLen, ctx);

    const payload = bytes.subarray(P.HEADER_LEN);
    assert.equal(toHex(payload), v.payload_hex, ctx);

    switch (v.payload_kind) {
      case 'json':
        assert.ok(P.JSON_TYPES.has(h.type), `${ctx}: tipo JSON`);
        assert.deepEqual(P.decodeJson(payload), v.payload_json, ctx);
        break;
      case 'binary':
        assert.deepEqual(decodeBinaryStrict(h.type, payload), v.fields, ctx);
        assert.equal(toHex(BINARY[h.type].encode(v.fields)), v.payload_hex, `${ctx}: re-encode`);
        break;
      case 'empty':
        assert.equal(payload.length, 0, ctx);
        break;
      case 'raw':
        break;
      default:
        assert.fail(`${ctx}: payload_kind desconhecido ${v.payload_kind}`);
    }

    if (h.type === T.VIDEO_FRAME) {
      const f = P.parseVideoFlags(h.flags);
      assert.deepEqual(
        { keyframe: f.keyframe, params_inband: f.paramsInband, discontinuity: f.discontinuity, rotation: f.rotation },
        v.flags_decoded, ctx,
      );
      assert.deepEqual(P.splitAnnexB(payload).map((n) => P.nalType(v.context_codec, n)), v.nal_types, ctx);
      if (f.discontinuity) assert.ok(f.keyframe, `${ctx}: DISCONTINUITY só em keyframes`);
      if (f.keyframe) assert.ok(f.paramsInband, `${ctx}: keyframes têm de ter PARAMS_INBAND`);
    }
    if (h.type === T.AUDIO_FRAME) assert.deepEqual(P.parseAudioFlags(h.flags), v.flags_decoded, ctx);

    const again = P.encodeMessage({ type: h.type, flags: h.flags, seq: h.seq, aux: h.aux, ptsUs: h.ptsUs, payload });
    assert.equal(toHex(again), v.hex, `${ctx}: re-encode da mensagem`);
    checks++;
  }

  const byName = new Map(doc.messages.map((m) => [m.name, m]));
  for (const s of doc.streams) {
    const bytes = hex(s.hex);
    assert.equal(s.hex, s.messages.map((n) => byName.get(n).hex).join(''), `stream ${s.name}`);
    for (const size of s.chunk_sizes) {
      const parser = new P.StreamParser();
      const got = [];
      for (let i = 0; i < bytes.length; i += size) got.push(...parser.push(bytes.subarray(i, i + size)));
      assert.equal(parser.pending, 0, `stream ${s.name} chunk ${size}: bytes pendentes`);
      const gotHex = got.map((m) => toHex(P.encodeMessage({ ...m })));
      assert.deepEqual(gotHex, s.messages.map((n) => byName.get(n).hex), `stream ${s.name} chunk ${size}`);
      checks++;
    }
  }

  for (const inv of doc.invalid) {
    const bytes = hex(inv.hex);
    const run = inv.stage === 'header'
      ? () => new P.StreamParser().push(bytes)
      : () => {
        const h = P.decodeHeader(bytes);
        decodeBinaryStrict(h.type, bytes.subarray(P.HEADER_LEN, P.HEADER_LEN + h.payloadLen));
      };
    assert.throws(run, (e) => e instanceof P.ProtocolError && e.code === inv.error, `inválido ${inv.name}`);
    checks++;
  }
  return checks;
}

// ── Main ─────────────────────────────────────────────────────────────────────
const checkOnly = process.argv.includes('--check');
const doc = buildDoc();
const text = `${JSON.stringify(doc, null, 2)}\n`;

if (checkOnly) {
  const current = readFileSync(OUT, 'utf8').replace(/\r\n/g, '\n');
  assert.equal(current, text, 'v1.json está desatualizado: corre `node protocol/reference/gen-vectors.mjs`');
} else {
  mkdirSync(dirname(OUT), { recursive: true });
  writeFileSync(OUT, text);
}

const checks = verify(JSON.parse(readFileSync(OUT, 'utf8')));
console.log(
  `PSWP v1: ${doc.messages.length} mensagens, ${doc.streams.length} streams, ${doc.invalid.length} inválidos — ${checks} verificações OK` +
  (checkOnly ? ' (só verificação)' : ` → ${OUT}`),
);
