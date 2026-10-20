#!/usr/bin/env node
/**
 * 阶段 4/C 合成素材夹具生成器（仅 Node 标准库，不联网、不读 daily、不清理既有夹具）。
 *
 * 生成内容（每个夹具一个目录包）：
 * - `packages/<name>/manifest.json`：按 GATEWAY C 契约冻结字段的纯数据清单；
 * - `packages/<name>/...`：微型 PNG 序列 / GIF 编码文件；
 * - `expected/<name>.json`：**由构造参数直接推导**的期望 RGBA、帧时长、循环提示，
 *   不经过任何实际解码器；用于和宿主解码结果逐像素比对；
 * - `index.json`：夹具清单（接受/拒绝、拒绝标签、缺陷位置），供 Rust 测试挂载。
 *
 * 拒绝夹具的 `rejectCode` 只是本夹具包的标签（便于人读与 Rust 侧映射），
 * 不是已冻结的 IPC/错误码契约，需领队在宿主接线时统一映射。
 *
 * 安全边界：
 * - 只写 `--out` 指定目录，绝不递归删除或清理其他目录；同路径文件按名覆盖；
 * - 预算级大样本必须显式 `--budget` 且 `--out` 落在 `target/` 下；
 * - 默认只生成 KB 级微观夹具，不产生大文件。
 */

import { createHash } from 'node:crypto';
import { existsSync, mkdirSync, writeFileSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { deflateSync } from 'node:zlib';

const HERE = path.dirname(fileURLToPath(import.meta.url));

// ---------------------------------------------------------------------------
// 通用工具
// ---------------------------------------------------------------------------

export function locateRepoRoot() {
  let dir = HERE;
  for (;;) {
    if (existsSync(path.join(dir, 'Cargo.toml')) && existsSync(path.join(dir, 'AGENTS.md'))) {
      return dir;
    }
    const parent = path.dirname(dir);
    if (parent === dir) throw new Error('无法定位仓库根（缺少 Cargo.toml/AGENTS.md）');
    dir = parent;
  }
}

const CRC_TABLE = (() => {
  const table = new Uint32Array(256);
  for (let n = 0; n < 256; n += 1) {
    let c = n;
    for (let k = 0; k < 8; k += 1) {
      c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    }
    table[n] = c >>> 0;
  }
  return table;
})();

export function crc32(buffer) {
  let c = 0xffffffff;
  for (let i = 0; i < buffer.length; i += 1) {
    c = CRC_TABLE[(c ^ buffer[i]) & 0xff] ^ (c >>> 8);
  }
  return (c ^ 0xffffffff) >>> 0;
}

function sha256Hex(buffer) {
  return createHash('sha256').update(buffer).digest('hex');
}

// ---------------------------------------------------------------------------
// PNG 编码器（8bit RGBA，支持 Adam7 交错与全部五种行过滤器）
// ---------------------------------------------------------------------------

function pngChunk(type, data) {
  const typeBytes = Buffer.from(type, 'ascii');
  const length = Buffer.alloc(4);
  length.writeUInt32BE(data.length, 0);
  const body = Buffer.concat([typeBytes, data]);
  const crc = Buffer.alloc(4);
  crc.writeUInt32BE(crc32(body), 0);
  return Buffer.concat([length, body, crc]);
}

function pngHeader(width, height, interlace) {
  const data = Buffer.alloc(13);
  data.writeUInt32BE(width, 0);
  data.writeUInt32BE(height, 4);
  data[8] = 8; // bit depth
  data[9] = 6; // color type: truecolour with alpha
  data[10] = 0; // compression
  data[11] = 0; // filter method
  data[12] = interlace ? 1 : 0;
  return pngChunk('IHDR', data);
}

const ADAM7_PASSES = [
  [0, 0, 8, 8],
  [4, 0, 8, 8],
  [0, 4, 4, 8],
  [2, 0, 4, 4],
  [0, 2, 2, 4],
  [1, 0, 2, 2],
  [0, 1, 1, 2],
];

function paeth(left, up, upperLeft) {
  const estimate = left + up - upperLeft;
  const dLeft = Math.abs(estimate - left);
  const dUp = Math.abs(estimate - up);
  const dUpperLeft = Math.abs(estimate - upperLeft);
  if (dLeft <= dUp && dLeft <= dUpperLeft) return left;
  if (dUp <= dUpperLeft) return up;
  return upperLeft;
}

/**
 * 对一条已重建扫描线做过滤。`filters` 可为 `none` 或 `cycle`（按行轮换 0..4）。
 * 编码器与解码侧共享同一定义，但期望像素来自场景构造，不依赖本函数。
 */
function filterRow(raw, previous, bpp, filterType) {
  const out = Buffer.alloc(raw.length);
  for (let i = 0; i < raw.length; i += 1) {
    const left = i >= bpp ? raw[i - bpp] : 0;
    const up = previous === null ? 0 : previous[i];
    const upperLeft = previous === null ? 0 : i >= bpp ? previous[i - bpp] : 0;
    let predictor = 0;
    if (filterType === 1) predictor = left;
    else if (filterType === 2) predictor = up;
    else if (filterType === 3) predictor = Math.floor((left + up) / 2);
    else if (filterType === 4) predictor = paeth(left, up, upperLeft);
    out[i] = (raw[i] - predictor) & 0xff;
  }
  return out;
}

function filterTypeForRow(filters, rowIndex) {
  if (filters !== 'cycle') return 0;
  return rowIndex % 5;
}

/**
 * 生成静态 PNG（颜色类型 6，8 位/通道）。`rgba` 为行主序、非预乘直通 alpha。
 */
export function encodePng({ width, height, rgba, interlace = false, filters = 'cycle', extraChunks = [] }) {
  if (rgba.length !== width * height * 4) {
    throw new Error(`RGBA 长度不匹配：${rgba.length} != ${width}x${height}x4`);
  }
  const scanlines = [];
  const passes = interlace ? ADAM7_PASSES : [[0, 0, 1, 1]];
  for (const [startX, startY, stepX, stepY] of passes) {
    const passWidth = Math.ceil((width - startX) / stepX);
    const passHeight = Math.ceil((height - startY) / stepY);
    if (passWidth <= 0 || passHeight <= 0) continue;
    let previous = null;
    for (let py = 0; py < passHeight; py += 1) {
      const raw = Buffer.alloc(passWidth * 4);
      for (let px = 0; px < passWidth; px += 1) {
        const sourceX = startX + px * stepX;
        const sourceY = startY + py * stepY;
        const offset = (sourceY * width + sourceX) * 4;
        rgba.copy(raw, px * 4, offset, offset + 4);
      }
      const type = filterTypeForRow(filters, scanlines.length);
      scanlines.push(Buffer.from([type]), filterRow(raw, previous, 4, type));
      previous = raw;
    }
  }
  const idat = deflateSync(Buffer.concat(scanlines), { level: 9 });
  const chunks = [Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), pngHeader(width, height, interlace)];
  for (const chunk of extraChunks) chunks.push(chunk);
  chunks.push(pngChunk('IDAT', idat), pngChunk('IEND', Buffer.alloc(0)));
  return Buffer.concat(chunks);
}

/** 拆分 PNG 为块序列；供坏流夹具精确改写单个块使用。 */
export function splitPngChunks(bytes) {
  const signature = bytes.subarray(0, 8);
  if (signature.toString('hex') !== '89504e470d0a1a0a') throw new Error('not a png');
  const chunks = [];
  let offset = 8;
  while (offset + 8 <= bytes.length) {
    const length = bytes.readUInt32BE(offset);
    const type = bytes.subarray(offset + 4, offset + 8).toString('ascii');
    const data = bytes.subarray(offset + 8, offset + 8 + length);
    const crc = bytes.subarray(offset + 8 + length, offset + 12 + length);
    chunks.push({ type, data: Buffer.from(data), crc: Buffer.from(crc) });
    offset += 12 + length;
  }
  return chunks;
}

export function joinPngChunks(chunks) {
  const parts = [Buffer.from([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])];
  for (const chunk of chunks) parts.push(pngChunk(chunk.type, chunk.data));
  return Buffer.concat(parts);
}

// ---------------------------------------------------------------------------
// GIF89a 编码器（全局/局部调色板、透明、局部矩形、交错、disposal 0/1/2/3、循环提示）
// ---------------------------------------------------------------------------

function paddedPalette(palette) {
  const size = Math.max(2, 2 ** Math.ceil(Math.log2(palette.length)));
  const table = Buffer.alloc(size * 3);
  palette.forEach(([r, g, b], index) => {
    table[index * 3] = r;
    table[index * 3 + 1] = g;
    table[index * 3 + 2] = b;
  });
  return { table, size, bits: Math.log2(size) };
}

function paletteBitsField(size) {
  return Math.log2(size) - 1;
}

/**
 * GIF LZW 压缩（位宽增长点与 GDI+/标准解码器一致）。
 * 编码器比解码器早一条表项登记，因此位宽提升判据是“已登记条数减一达到
 * 2^codeSize”，即 nextCode-1 === 1<<codeSize；直接用 nextCode 会早一码提升位宽，
 * 生成 GDI+ 无法解码的流（已用 System.Drawing 实际解码对照确认）。
 */
function lzwEncode(indices, minCodeSize) {
  const clearCode = 1 << minCodeSize;
  const endCode = clearCode + 1;
  let codeSize = minCodeSize + 1;
  let nextCode = endCode + 1;
  let dictionary = new Map();
  const bytes = [];
  let bitBuffer = 0;
  let bitCount = 0;

  const emit = (code) => {
    bitBuffer |= code << bitCount;
    bitCount += codeSize;
    while (bitCount >= 8) {
      bytes.push(bitBuffer & 0xff);
      bitBuffer >>>= 8;
      bitCount -= 8;
    }
  };
  const resetTable = () => {
    dictionary = new Map();
    codeSize = minCodeSize + 1;
    nextCode = endCode + 1;
  };

  emit(clearCode);
  let prefix = indices[0];
  for (let i = 1; i < indices.length; i += 1) {
    const next = indices[i];
    const key = prefix * 256 + next;
    const known = dictionary.get(key);
    if (known !== undefined) {
      prefix = known;
      continue;
    }
    emit(prefix);
    if (nextCode < 4096) {
      dictionary.set(key, nextCode);
      nextCode += 1;
      if (nextCode - 1 === 1 << codeSize && codeSize < 12) codeSize += 1;
    } else {
      emit(clearCode);
      resetTable();
    }
    prefix = next;
  }
  emit(prefix);
  emit(endCode);
  if (bitCount > 0) bytes.push(bitBuffer & 0xff);
  return Buffer.from(bytes);
}

function writeSubBlocks(data) {
  const parts = [];
  for (let offset = 0; offset < data.length; offset += 255) {
    const slice = data.subarray(offset, offset + 255);
    parts.push(Buffer.from([slice.length]), slice);
  }
  parts.push(Buffer.from([0x00]));
  return Buffer.concat(parts);
}

function interlaceRowOrder(height) {
  const rows = [];
  for (let y = 0; y < height; y += 8) rows.push(y);
  for (let y = 4; y < height; y += 8) rows.push(y);
  for (let y = 2; y < height; y += 4) rows.push(y);
  for (let y = 1; y < height; y += 2) rows.push(y);
  return rows;
}

/**
 * 编码 GIF。`frames[i]` 为 `{ left, top, width, height, indices, disposal, delayCs,
 * interlace?, transparent?, localPalette? }`；`indices` 为局部矩形内逐行调色板下标。
 */
export function encodeGif({ width, height, palette, transparentIndex = null, loopCount = null, frames }) {
  const globalPalette = paddedPalette(palette);
  const parts = [];
  parts.push(Buffer.from('GIF89a', 'ascii'));

  const lsd = Buffer.alloc(7);
  lsd.writeUInt16LE(width, 0);
  lsd.writeUInt16LE(height, 2);
  lsd[4] = 0x80 | (7 << 4) | paletteBitsField(globalPalette.size);
  lsd[5] = 0; // 背景色下标固定 0；不依赖“初始画布背景”语义
  lsd[6] = 0;
  parts.push(lsd, globalPalette.table);

  if (loopCount !== null) {
    // 应用扩展块：0x21 0xFF 0x0B "NETSCAPE2.0" 03 01 <loop> 00。
    const app = Buffer.from([0x21, 0xff, 0x0b]);
    const body = Buffer.alloc(11 + 4 + 1);
    body.write('NETSCAPE2.0', 0, 'ascii');
    body[11] = 0x03;
    body[12] = 0x01;
    body.writeUInt16LE(loopCount, 13);
    body[15] = 0x00;
    parts.push(app, body);
  }

  for (const frame of frames) {
    const useTransparency = frame.transparent === true && transparentIndex !== null;
    const gce = Buffer.from([
      0x21,
      0xf9,
      0x04,
      ((frame.disposal & 0x07) << 2) | (useTransparency ? 0x01 : 0x00),
      frame.delayCs & 0xff,
      (frame.delayCs >> 8) & 0xff,
      useTransparency ? transparentIndex & 0xff : 0x00,
      0x00,
    ]);
    parts.push(gce);

    const local = frame.localPalette === undefined ? null : paddedPalette(frame.localPalette);
    const descriptor = Buffer.alloc(10);
    descriptor[0] = 0x2c;
    descriptor.writeUInt16LE(frame.left, 1);
    descriptor.writeUInt16LE(frame.top, 3);
    descriptor.writeUInt16LE(frame.width, 5);
    descriptor.writeUInt16LE(frame.height, 7);
    descriptor[9] =
      (local === null ? 0 : 0x80) | (frame.interlace ? 0x40 : 0) | paletteBitsField((local ?? globalPalette).size);
    parts.push(descriptor);
    if (local !== null) parts.push(local.table);

    const used = local === null ? globalPalette : local;
    const minCodeSize = Math.max(2, Math.ceil(Math.log2(used.size)));
    const rows = frame.interlace ? interlaceRowOrder(frame.height) : [...Array(frame.height).keys()];
    const ordered = Buffer.alloc(frame.width * frame.height);
    rows.forEach((sourceRow, writeRow) => {
      frame.indices.copy(
        ordered,
        writeRow * frame.width,
        sourceRow * frame.width,
        sourceRow * frame.width + frame.width,
      );
    });
    parts.push(Buffer.from([minCodeSize]), writeSubBlocks(lzwEncode(ordered, minCodeSize)));
  }

  parts.push(Buffer.from([0x3b]));
  return Buffer.concat(parts);
}

/** 逐像素构造局部矩形调色板下标；`fn(x, y)` 返回调色板下标。 */
export function indexPattern(width, height, fn) {
  const buffer = Buffer.alloc(width * height);
  for (let y = 0; y < height; y += 1) {
    for (let x = 0; x < width; x += 1) {
      buffer[y * width + x] = fn(x, y);
    }
  }
  return buffer;
}

// ---------------------------------------------------------------------------
// 期望数据：从构造场景直接合成（不经过解码器）
// ---------------------------------------------------------------------------

function alphaHistogram(rgba) {
  let opaque = 0;
  let transparent = 0;
  let semi = 0;
  for (let offset = 3; offset < rgba.length; offset += 4) {
    const alpha = rgba[offset];
    if (alpha === 255) opaque += 1;
    else if (alpha === 0) transparent += 1;
    else semi += 1;
  }
  return { opaque, transparent, semi };
}

const INLINE_PIXEL_LIMIT = 64 * 64;

function frameExpectation({ index, durationMs, rgba, width, height, note }) {
  const histogram = alphaHistogram(rgba);
  const inlinePixels = width * height <= INLINE_PIXEL_LIMIT;
  const frame = {
    index,
    durationMs,
    width,
    height,
    opaquePixels: histogram.opaque,
    transparentPixels: histogram.transparent,
    semiTransparentPixels: histogram.semi,
    rgbaSha256: sha256Hex(rgba),
  };
  if (inlinePixels) frame.rgbaBase64 = rgba.toString('base64');
  if (note !== undefined) frame.note = note;
  return frame;
}

/**
 * 合成 GIF 显示帧。语义按 GIF89a：
 * - 画布初始为全透明（不绘制 LSD 背景色）；
 * - 帧内透明下标不改写画布；否则直通写入调色板颜色（alpha=255）；
 * - disposal 0/1 保留，2 清回透明，3 恢复本帧绘制前画布。
 */
export function composeGifFrames({ width, height, palette, transparentIndex, frames }, { disposalOverride } = {}) {
  let canvas = Buffer.alloc(width * height * 4, 0);
  const displayed = [];
  for (const frame of frames) {
    const disposal = frame.disposal;
    const saved = disposalOverride === 3 ? null : disposal === 3 ? Buffer.from(canvas) : null;
    for (let y = 0; y < frame.height; y += 1) {
      for (let x = 0; x < frame.width; x += 1) {
        const paletteIndex = frame.indices[y * frame.width + x];
        if (transparentIndex !== null && paletteIndex === transparentIndex) continue;
        const color = (frame.localPalette ?? palette)[paletteIndex];
        if (color === undefined) throw new Error(`帧 ${frame.index} 使用未定义调色板下标 ${paletteIndex}`);
        const target = ((frame.top + y) * width + (frame.left + x)) * 4;
        canvas[target] = color[0];
        canvas[target + 1] = color[1];
        canvas[target + 2] = color[2];
        canvas[target + 3] = 255;
      }
    }
    displayed.push(Buffer.from(canvas));
    const effective = disposalOverride !== undefined && disposal === disposalOverride ? 0 : disposal;
    if (effective === 2) {
      for (let y = 0; y < frame.height; y += 1) {
        for (let x = 0; x < frame.width; x += 1) {
          const target = ((frame.top + y) * width + (frame.left + x)) * 4;
          canvas[target] = 0;
          canvas[target + 1] = 0;
          canvas[target + 2] = 0;
          canvas[target + 3] = 0;
        }
      }
    } else if (effective === 3 && saved !== null) {
      canvas = saved;
    }
  }
  return displayed;
}

function differsAt(a, b) {
  if (a.length !== b.length) return true;
  for (let i = 0; i < a.length; i += 1) {
    if (a[i] !== b[i]) return true;
  }
  return false;
}

// ---------------------------------------------------------------------------
// 夹具定义
// ---------------------------------------------------------------------------

function rectOps(width, height, ops) {
  const buffer = Buffer.alloc(width * height * 4, 0);
  for (const op of ops) {
    if (op.op === 'rect') {
      const [x, y, w, h, rgba] = op.args;
      for (let py = y; py < y + h; py += 1) {
        for (let px = x; px < x + w; px += 1) {
          if (px < 0 || py < 0 || px >= width || py >= height) continue;
          const target = (py * width + px) * 4;
          buffer[target] = rgba[0];
          buffer[target + 1] = rgba[1];
          buffer[target + 2] = rgba[2];
          buffer[target + 3] = rgba[3];
        }
      }
    } else {
      throw new Error(`未知绘制操作 ${op.op}`);
    }
  }
  return buffer;
}

const GREEN = [40, 80, 60, 255];
const ORANGE = [230, 90, 60, 255];
const DARK = [30, 30, 30, 255];
const LIGHT = [200, 200, 200, 255];
const PAPER = [250, 247, 238, 255];
const BLUE = [90, 140, 210, 255];
const GHOST = [90, 140, 210, 128];

function fixtureSequenceBasic() {
  const width = 24;
  const height = 16;
  const durations = [40, 70, 120, 250];
  const scenes = [
    rectOps(width, height, [{ op: 'rect', args: [2, 2, 20, 12, GREEN] }]),
    rectOps(width, height, [
      { op: 'rect', args: [2, 2, 20, 12, GREEN] },
      { op: 'rect', args: [6, 6, 4, 4, ORANGE] },
    ]),
    rectOps(width, height, [{ op: 'rect', args: [2, 2, 20, 12, GHOST] }]),
    rectOps(width, height, [
      { op: 'rect', args: [4, 4, 16, 8, DARK] },
      { op: 'rect', args: [4, 4, 2, 2, [0, 0, 0, 0]] },
    ]),
  ];
  const files = new Map();
  const frames = scenes.map((rgba, index) => {
    const file = `frames/f${index}.png`;
    files.set(file, { png: { width, height, rgba } });
    return { path: file, durationMs: durations[index] };
  });
  const manifest = {
    formatVersion: 1,
    id: 'synthetic-seq-basic',
    version: 1,
    label: '合成序列基础',
    clips: [{ id: 'idle', type: 'sequence', frames }],
    actions: [
      { id: 'idle', label: '待机', clip: 'idle', mode: 'loop' },
      { id: 'wave', label: '招手', clip: 'idle', mode: 'once' },
    ],
    defaultAction: 'idle',
  };
  const expected = {
    canvas: { width, height },
    clips: {
      idle: {
        type: 'sequence',
        frameCount: scenes.length,
        durationsMs: durations,
        totalDurationMs: durations.reduce((sum, value) => sum + value, 0),
        frames: scenes.map((rgba, index) =>
          frameExpectation({
            index,
            durationMs: durations[index],
            rgba,
            width,
            height,
            note:
              index === 2
                ? '整块 alpha=128 半透明，未被预乘'
                : index === 3
                  ? '左上 2x2 为全透明洞，alpha=0 像素 RGB 保持 0'
                  : index === 0
                    ? '画布四周 2px 环带全透明'
                    : '不透明绿底 + 4x4 橙色块',
          }),
        ),
      },
    },
  };
  return {
    entry: {
      name: 'valid-sequence-basic',
      expect: 'accept',
      notes: [
        '4 帧静态 PNG 序列，非等时 40/70/120/250ms，总 480ms',
        '覆盖全透明、半透明与不透明像素；行过滤器按 0..4 轮换',
      ],
    },
    manifest,
    expected,
    files,
  };
}

/**
 * 首版只接受非交错静态 PNG。这里保留 Adam7 编码夹具，但明确列为拒绝项：
 * 契约不承诺完整 PNG，交错 PNG 必须报未支持，不能静默解出或只取首 pass。
 */
function rejectSequenceInterlaced() {
  const width = 17;
  const height = 11;
  const durations = [100, 200, 300];
  const scenes = [
    rectOps(width, height, [{ op: 'rect', args: [0, 0, width, height, PAPER] }]),
    rectOps(width, height, [
      { op: 'rect', args: [0, 0, width, height, GREEN] },
      { op: 'rect', args: [3, 2, 5, 4, ORANGE] },
    ]),
    rectOps(width, height, [
      { op: 'rect', args: [0, 0, 4, 3, BLUE] },
      { op: 'rect', args: [13, 8, 3, 2, DARK] },
    ]),
  ];
  const files = new Map();
  const frames = scenes.map((rgba, index) => {
    const file = `frames/interlaced-f${index}.png`;
    files.set(file, { png: { width, height, rgba, interlace: true } });
    return { path: file, durationMs: durations[index] };
  });
  const manifest = {
    formatVersion: 1,
    id: 'synthetic-seq-interlaced',
    version: 1,
    label: '合成序列交错',
    clips: [{ id: 'walk', type: 'sequence', frames }],
    actions: [{ id: 'walk', label: '走动', clip: 'walk', mode: 'loop' }],
    defaultAction: 'walk',
  };
  return rejectFixture(
    'reject-sequence-interlaced-adam7',
    'png_interlaced_unsupported',
    { where: 'IHDR.interlace', value: 1 },
    [
      '首版只接受非交错静态 PNG；Adam7 交错明确报未支持，不冒称完整 PNG 支持',
      '17x11 的通道宽度不能被 8 整除，7 个 pass 全部非空，仍须整包拒绝而不是只解首 pass',
    ],
    manifest,
    files,
  );
}

const GIF_PALETTE = [
  [0, 0, 0],
  [40, 80, 60],
  [230, 90, 60],
  [30, 30, 30],
  [200, 200, 200],
  [90, 140, 210],
  [250, 247, 238],
  [1, 1, 1],
];
const GIF_TRANSPARENT = 7;

/**
 * GIF 合成策略（写入 expected.json，供宿主解码器对齐）。
 * 直接采用 GATEWAY 首版冻结的透明呈现策略：初始画布全透明；disposal 0/1 保留，
 * 2 把本帧矩形清为 RGBA 全零，3 恢复本帧绘制前的画布；LSD 背景色不参与合成。
 * System.Drawing/GDI+ 对 disposal 2 会填充 LSD 背景色且首帧外区域不透明，
 * 因此**不得把 GDI+ 的不同背景结果当作像素验收通过**。
 */
const GIF_COMPOSITION_POLICY = {
  canvasOrigin: 'transparent：画布初始全透明，不绘制 LSD 背景色',
  disposal0: '保留本帧结果',
  disposal1: '保留本帧结果',
  disposal2: '把本帧矩形清为 RGBA 全零（不使用 LSD 背景色）',
  disposal3: '把本帧矩形恢复为本帧绘制前的画布',
  transparency: '透明下标不改写画布；非透明像素直通写入 alpha=255；无半透明',
  delay: 'raw GCE delay*10，不做浏览器 2cs 下限钳制',
  gdiPlus: 'GDI+ 对 disposal 2 填充 LSD 背景色且首帧外区域不透明，与冻结策略不同，不能据此判通过',
};

function gifDisposalSpec() {
  const width = 20;
  const height = 12;
  const frames = [
    {
      index: 0,
      left: 0,
      top: 0,
      width: 20,
      height: 12,
      disposal: 0,
      delayCs: 3,
      indices: indexPattern(20, 12, () => 1),
    },
    {
      index: 1,
      left: 4,
      top: 3,
      width: 8,
      height: 6,
      disposal: 2,
      delayCs: 4,
      transparent: true,
      indices: indexPattern(8, 6, (x, y) => ((x === 2 || x === 3) && (y === 2 || y === 3) ? GIF_TRANSPARENT : x === 5 && y === 1 ? GIF_TRANSPARENT : 2)),
    },
    {
      index: 2,
      left: 9,
      top: 2,
      width: 5,
      height: 5,
      disposal: 3,
      delayCs: 7,
      transparent: true,
      indices: indexPattern(5, 5, (x, y) => (x === 0 && y === 0 ? GIF_TRANSPARENT : 3)),
    },
    {
      index: 3,
      left: 14,
      top: 1,
      width: 5,
      height: 8,
      disposal: 1,
      delayCs: 11,
      indices: indexPattern(5, 8, () => 5),
    },
    {
      index: 4,
      left: 0,
      top: 0,
      width: 6,
      height: 6,
      disposal: 0,
      delayCs: 16,
      indices: indexPattern(6, 6, () => 6),
    },
  ];
  return { width, height, palette: GIF_PALETTE, transparentIndex: GIF_TRANSPARENT, loopCount: 0, frames };
}

function fixtureGifDisposal() {
  const spec = gifDisposalSpec();
  const gif = encodeGif(spec);
  const displayed = composeGifFrames(spec);
  const naiveZero = composeGifFrames(spec, { disposalOverride: 2 });
  const naiveThree = composeGifFrames(spec, { disposalOverride: 3 });
  const disposal2Visible = displayed.some((frame, index) => differsAt(frame, naiveZero[index]));
  const disposal3Visible = displayed.some((frame, index) => differsAt(frame, naiveThree[index]));
  if (!disposal2Visible || !disposal3Visible) {
    throw new Error('GIF disposal 夹具失去区分度：需要 disposal 2/3 在后续显示帧上可见');
  }
  const transparentVisible = displayed.some((frame) => {
    for (let offset = 3; offset < frame.length; offset += 4) if (frame[offset] === 255) return true;
    return false;
  });
  if (!transparentVisible) throw new Error('GIF 夹具缺少可见像素');
  const files = new Map([['clips/idle.gif', { gif: spec }]]);
  const manifest = {
    formatVersion: 1,
    id: 'synthetic-gif-disposal',
    version: 2,
    label: '合成GIF合成模式',
    clips: [{ id: 'idle', type: 'gif', path: 'clips/idle.gif' }],
    actions: [{ id: 'idle', label: '待机', clip: 'idle', mode: 'loop' }],
    defaultAction: 'idle',
  };
  const expected = {
    canvas: { width: spec.width, height: spec.height },
    clips: {
      idle: {
        type: 'gif',
        frameCount: displayed.length,
        durationsMs: spec.frames.map((frame) => frame.delayCs * 10),
        totalDurationMs: spec.frames.reduce((sum, frame) => sum + frame.delayCs * 10, 0),
        loopCount: spec.loopCount,
        disposalPolicy: GIF_COMPOSITION_POLICY,
        transport: 'raw GCE delay*10；不做浏览器 2cs 下限钳制',
        frames: displayed.map((rgba, index) =>
          frameExpectation({
            index,
            durationMs: spec.frames[index].delayCs * 10,
            rgba,
            width: spec.width,
            height: spec.height,
            note: `disposal=${spec.frames[index].disposal}${spec.frames[index].transparent ? '，含透明下标' : ''}`,
          }),
        ),
      },
    },
  };
  return {
    entry: {
      name: 'valid-gif-disposal',
      expect: 'accept',
      notes: [
        '单 GIF 内同时使用 disposal 0/1/2/3、局部矩形、调色板透明、非等时帧',
        'NETSCAPE2.0 循环提示 loopCount=0（无限）；不启动第二套循环',
        'disposal 2/3 均对后续显示帧产生可断言差异（生成时已校验）',
      ],
    },
    manifest,
    expected,
    files,
  };
}

function fixtureGifInterlaced() {
  const palette = [
    [0, 0, 0],
    [230, 90, 60],
    [40, 80, 60],
    [200, 200, 200],
  ];
  const localPalette = [
    [250, 247, 238],
    [30, 30, 30],
    [90, 140, 210],
    [235, 214, 201],
  ];
  const width = 16;
  const height = 9;
  const frames = [
    {
      index: 0,
      left: 0,
      top: 0,
      width: 16,
      height: 9,
      disposal: 1,
      delayCs: 5,
      interlace: true,
      indices: indexPattern(16, 9, (x, y) => ((x + y) % 2 === 0 ? 1 : 2)),
    },
    {
      index: 1,
      left: 2,
      top: 1,
      width: 12,
      height: 7,
      disposal: 0,
      delayCs: 9,
      interlace: true,
      transparent: false,
      localPalette,
      indices: indexPattern(12, 7, (x, y) => ((x * 3 + y) % 4 === 0 ? 1 : 2)),
    },
  ];
  const spec = { width, height, palette, transparentIndex: null, loopCount: null, frames };
  const displayed = composeGifFrames(spec);
  const files = new Map([['clips/interlaced.gif', { gif: spec }]]);
  const manifest = {
    formatVersion: 1,
    id: 'synthetic-gif-interlaced',
    version: 1,
    label: '合成GIF交错',
    clips: [{ id: 'hop', type: 'gif', path: 'clips/interlaced.gif' }],
    actions: [{ id: 'hop', label: '跳', clip: 'hop', mode: 'once' }],
    defaultAction: 'hop',
  };
  const expected = {
    canvas: { width, height },
    clips: {
      hop: {
        type: 'gif',
        frameCount: displayed.length,
        durationsMs: frames.map((frame) => frame.delayCs * 10),
        totalDurationMs: frames.reduce((sum, frame) => sum + frame.delayCs * 10, 0),
        loopCount: null,
        disposalPolicy: GIF_COMPOSITION_POLICY,
        transport: 'raw GCE delay*10',
        frames: displayed.map((rgba, index) =>
          frameExpectation({
            index,
            durationMs: frames[index].delayCs * 10,
            rgba,
            width,
            height,
            note: index === 0 ? '交错 + 全局调色板' : '交错 + 局部调色板（无透明下标）',
          }),
        ),
      },
    },
  };
  return {
    entry: {
      name: 'valid-gif-interlaced',
      expect: 'accept',
      notes: [
        '两帧均置交错标志，第 2 帧使用局部调色板',
        '无 NETSCAPE 扩展：loopCount 为 null，单次播放；宿主不得自行补循环',
      ],
    },
    manifest,
    expected,
    files,
  };
}

function fixtureCombo() {
  const width = 12;
  const height = 8;
  const durations = [60, 90];
  const scenes = [
    rectOps(width, height, [{ op: 'rect', args: [0, 0, 12, 8, PAPER] }]),
    rectOps(width, height, [{ op: 'rect', args: [2, 2, 8, 4, ORANGE] }]),
  ];
  const files = new Map();
  const seqFrames = scenes.map((rgba, index) => {
    const file = `frames/talk${index}.png`;
    files.set(file, { png: { width, height, rgba } });
    return { path: file, durationMs: durations[index] };
  });
  const gifFrames = [
    {
      index: 0,
      left: 0,
      top: 0,
      width: 8,
      height: 8,
      disposal: 1,
      delayCs: 8,
      indices: indexPattern(8, 8, () => 1),
    },
    {
      index: 1,
      left: 1,
      top: 1,
      width: 4,
      height: 4,
      disposal: 0,
      delayCs: 13,
      indices: indexPattern(4, 4, () => 2),
    },
    {
      index: 2,
      left: 3,
      top: 3,
      width: 3,
      height: 3,
      disposal: 2,
      delayCs: 21,
      indices: indexPattern(3, 3, () => 4),
    },
  ];
  const gifSpec = {
    width: 8,
    height: 8,
    palette: GIF_PALETTE,
    transparentIndex: GIF_TRANSPARENT,
    loopCount: 2,
    frames: gifFrames,
  };
  files.set('clips/cheer.gif', { gif: gifSpec });
  const displayed = composeGifFrames(gifSpec);
  const manifest = {
    formatVersion: 1,
    id: 'synthetic-combo',
    version: 7,
    label: '合成多片段',
    clips: [
      { id: 'talk', type: 'sequence', frames: seqFrames },
      { id: 'cheer', type: 'gif', path: 'clips/cheer.gif' },
    ],
    actions: [
      { id: 'talk', label: '说话', clip: 'talk', mode: 'loop' },
      { id: 'cheer', label: '欢呼', clip: 'cheer', mode: 'once' },
    ],
    defaultAction: 'talk',
  };
  const expected = {
    canvas: { width: 12, height: 8 },
    clips: {
      talk: {
        type: 'sequence',
        frameCount: scenes.length,
        durationsMs: durations,
        totalDurationMs: durations.reduce((sum, value) => sum + value, 0),
        frames: scenes.map((rgba, index) =>
          frameExpectation({ index, durationMs: durations[index], rgba, width, height }),
        ),
      },
      cheer: {
        type: 'gif',
        frameCount: displayed.length,
        durationsMs: gifFrames.map((frame) => frame.delayCs * 10),
        totalDurationMs: gifFrames.reduce((sum, frame) => sum + frame.delayCs * 10, 0),
        loopCount: 2,
        disposalPolicy: GIF_COMPOSITION_POLICY,
        transport: 'raw GCE delay*10',
        canvas: { width: 8, height: 8 },
        frames: displayed.map((rgba, index) =>
          frameExpectation({
            index,
            durationMs: gifFrames[index].delayCs * 10,
            rgba,
            width: 8,
            height: 8,
          }),
        ),
      },
    },
  };
  return {
    entry: {
      name: 'valid-combo',
      expect: 'accept',
      notes: [
        '同包内 PNG 序列 + GIF 两类 Clip 与 once/loop 两种动作',
        'GIF 循环提示 loopCount=2；动作 loop 仍由宿主 Animator 决定，不叠加第二套循环',
      ],
    },
    manifest,
    expected,
    files,
  };
}

function fixtureMinimal() {
  const rgba = Buffer.from([0, 0, 0, 0]);
  const files = new Map([['frames/1px.png', { png: { width: 1, height: 1, rgba, filters: 'none' } }]]);
  const manifest = {
    formatVersion: 1,
    id: 'synthetic-minimal',
    version: 1,
    label: '合成最小包',
    clips: [{ id: 'dot', type: 'sequence', frames: [{ path: 'frames/1px.png', durationMs: 1 }] }],
    actions: [{ id: 'dot', label: '点', clip: 'dot', mode: 'once' }],
    defaultAction: 'dot',
  };
  const expected = {
    canvas: { width: 1, height: 1 },
    clips: {
      dot: {
        type: 'sequence',
        frameCount: 1,
        durationsMs: [1],
        totalDurationMs: 1,
        frames: [frameExpectation({ index: 0, durationMs: 1, rgba, width: 1, height: 1, note: '全透明 1x1，最短合法时长 1ms' })],
      },
    },
  };
  return {
    entry: {
      name: 'valid-minimal',
      expect: 'accept',
      notes: ['下限边界：1x1 全透明 PNG、单帧、durationMs=1'],
    },
    manifest,
    expected,
    files,
  };
}

function fixtureMaxDuration() {
  const width = 4;
  const height = 4;
  const rgba = rectOps(width, height, [{ op: 'rect', args: [0, 0, 4, 4, PAPER] }]);
  const files = new Map([['frames/long.png', { png: { width, height, rgba } }]]);
  const manifest = {
    formatVersion: 1,
    id: 'synthetic-max-duration',
    version: 1,
    label: '合成最长帧',
    clips: [
      {
        id: 'long',
        type: 'sequence',
        frames: [{ path: 'frames/long.png', durationMs: 600000 }],
      },
    ],
    actions: [{ id: 'long', label: '长帧', clip: 'long', mode: 'once' }],
    defaultAction: 'long',
  };
  const expected = {
    canvas: { width, height },
    clips: {
      long: {
        type: 'sequence',
        frameCount: 1,
        durationsMs: [600000],
        totalDurationMs: 600000,
        frames: [frameExpectation({ index: 0, durationMs: 600000, rgba, width, height, note: '单帧最长 600000ms' })],
      },
    },
  };
  return {
    entry: {
      name: 'valid-max-duration',
      expect: 'accept',
      notes: ['上限边界：单帧 600000ms、单 Clip 总时长 600000ms ≤ 3600000ms'],
    },
    manifest,
    expected,
    files,
  };
}

function fixtureMaxFrames() {
  const width = 16;
  const height = 16;
  const rgba = rectOps(width, height, [{ op: 'rect', args: [0, 0, 16, 16, PAPER] }]);
  const files = new Map([['frames/tile.png', { png: { width, height, rgba } }]]);
  const makeFrames = (count) =>
    Array.from({ length: count }, () => ({ path: 'frames/tile.png', durationMs: 1 }));
  const clips = [
    { id: 'a', type: 'sequence', frames: makeFrames(256) },
    { id: 'b', type: 'sequence', frames: makeFrames(256) },
  ];
  const manifest = {
    formatVersion: 1,
    id: 'synthetic-max-frames',
    version: 1,
    label: '合成满帧数',
    clips,
    actions: [
      { id: 'a', label: 'A', clip: 'a', mode: 'loop' },
      { id: 'b', label: 'B', clip: 'b', mode: 'once' },
    ],
    defaultAction: 'a',
  };
  const expected = {
    canvas: { width, height },
    clips: {
      a: {
        type: 'sequence',
        frameCount: 256,
        durationsMs: Array(256).fill(1),
        totalDurationMs: 256,
        repeatedAsset: true,
        frames: Array.from({ length: 256 }, (_, index) =>
          frameExpectation({ index, durationMs: 1, rgba, width, height }),
        ),
      },
      b: {
        type: 'sequence',
        frameCount: 256,
        durationsMs: Array(256).fill(1),
        totalDurationMs: 256,
        repeatedAsset: true,
        frames: Array.from({ length: 256 }, (_, index) =>
          frameExpectation({ index, durationMs: 1, rgba, width, height }),
        ),
      },
    },
  };
  // 512 帧内容完全相同：只在首帧内联像素，其余仅保留 sha256 与直方图，
  // 避免 sidecar 膨胀；解码侧仍逐帧比对哈希。
  for (const clip of Object.values(expected.clips)) {
    clip.frames.forEach((frame, index) => {
      if (index > 0) delete frame.rgbaBase64;
    });
  }
  return {
    entry: {
      name: 'valid-max-frames',
      expect: 'accept',
      notes: [
        '上限边界：单 Clip 256 帧、全包 512 帧（256+256）',
        '同一文件重复引用仍计读取预算，但编码数据总量只有一份',
      ],
    },
    manifest,
    expected,
    files,
  };
}

// ---------------------------------------------------------------------------
// 拒绝夹具
// ---------------------------------------------------------------------------

function seedPng(width = 8, height = 8, color = PAPER) {
  const rgba = rectOps(width, height, [{ op: 'rect', args: [0, 0, width, height, color] }]);
  return encodePng({ width, height, rgba });
}

function rejectFixture(name, code, defect, notes, manifest, files = new Map()) {
  return {
    entry: { name, expect: 'reject', rejectCode: code, defect, notes },
    manifest,
    files,
  };
}

function baseManifest(clipId = 'idle', framePath = 'frames/a.png', durationMs = 100) {
  return {
    formatVersion: 1,
    id: 'synthetic-reject',
    version: 1,
    label: '合成拒绝夹具',
    clips: [{ id: clipId, type: 'sequence', frames: [{ path: framePath, durationMs }] }],
    actions: [{ id: clipId, label: '待机', clip: clipId, mode: 'loop' }],
    defaultAction: clipId,
  };
}

function buildRejectFixtures() {
  const fixtures = [rejectSequenceInterlaced()];
  const png = () => seedPng();

  fixtures.push(
    rejectFixture(
      'reject-manifest-unknown-field',
      'manifest_unknown_field',
      { where: '$.extraField', value: true },
      ['清单出现未冻结字段 extraField，严格解析必须拒绝而不是忽略'],
      { ...baseManifest(), extraField: true },
      new Map([['frames/a.png', { png: { width: 8, height: 8, rgba: rectOps(8, 8, [{ op: 'rect', args: [0, 0, 8, 8, PAPER] }]) } }]]),
    ),
  );

  // 重复键必须保留原文（JSON.parse 会静默取最后一个）。
  fixtures.push({
    entry: {
      name: 'reject-manifest-duplicate-field',
      expect: 'reject',
      rejectCode: 'manifest_duplicate_field',
      defect: { where: '$.formatVersion', value: '重复出现两次' },
      notes: ['同一对象内 formatVersion 出现两次，严格解析必须拒绝'],
    },
    rawManifest: `{"formatVersion":1,"formatVersion":1,"id":"synthetic-reject","version":1,"label":"合成拒绝夹具","clips":[{"id":"idle","type":"sequence","frames":[{"path":"frames/a.png","durationMs":100}]}],"actions":[{"id":"idle","label":"待机","clip":"idle","mode":"loop"}],"defaultAction":"idle"}`,
    files: new Map([['frames/a.png', { png: { width: 8, height: 8, rgba: rectOps(8, 8, [{ op: 'rect', args: [0, 0, 8, 8, PAPER] }]) } }]]),
  });

  fixtures.push(
    rejectFixture(
      'reject-manifest-unknown-version',
      'manifest_version_unsupported',
      { where: '$.formatVersion', value: 2 },
      ['未知 formatVersion=2；不猜测向后兼容'],
      { ...baseManifest(), formatVersion: 2 },
      new Map([['frames/a.png', { png: { width: 8, height: 8, rgba: rectOps(8, 8, [{ op: 'rect', args: [0, 0, 8, 8, PAPER] }]) } }]]),
    ),
  );

  const missingField = baseManifest();
  delete missingField.defaultAction;
  fixtures.push(
    rejectFixture(
      'reject-manifest-missing-field',
      'manifest_missing_field',
      { where: '$.defaultAction', value: null },
      ['缺少必填 defaultAction；不得回退到首个动作'],
      missingField,
      new Map([['frames/a.png', { png: { width: 8, height: 8, rgba: rectOps(8, 8, [{ op: 'rect', args: [0, 0, 8, 8, PAPER] }]) } }]]),
    ),
  );

  fixtures.push(
    rejectFixture(
      'reject-manifest-too-large',
      'manifest_too_large',
      { where: 'manifest.json 字节数', value: '> 65536' },
      ['清单 >64KiB（尾部合法性空白填充），须在读字段前按字节拒绝'],
      { ...baseManifest(), pad: ' '.repeat(70 * 1024) },
      new Map([['frames/a.png', { png: { width: 8, height: 8, rgba: rectOps(8, 8, [{ op: 'rect', args: [0, 0, 8, 8, PAPER] }]) } }]]),
    ),
  );

  const badId = baseManifest('bad id');
  fixtures.push(
    rejectFixture(
      'reject-id-charset',
      'id_charset',
      { where: '$.clips[0].id', value: 'bad id' },
      ['ID 含空格，非 [A-Za-z0-9_-]{1,64}'],
      badId,
      new Map([['frames/a.png', { png: { width: 8, height: 8, rgba: rectOps(8, 8, [{ op: 'rect', args: [0, 0, 8, 8, PAPER] }]) } }]]),
    ),
  );

  const longId = 'a'.repeat(65);
  fixtures.push(
    rejectFixture(
      'reject-id-too-long',
      'id_length',
      { where: '$.clips[0].id', value: '65 字节' },
      ['ID 65 字节超过 64 字节上限'],
      { ...baseManifest('idle'), clips: [{ id: longId, type: 'sequence', frames: [{ path: 'frames/a.png', durationMs: 100 }] }], actions: [{ id: 'idle', label: '待机', clip: longId, mode: 'loop' }], defaultAction: 'idle' },
      new Map([['frames/a.png', { png: { width: 8, height: 8, rgba: rectOps(8, 8, [{ op: 'rect', args: [0, 0, 8, 8, PAPER] }]) } }]]),
    ),
  );

  const dupId = baseManifest();
  dupId.clips.push({ id: 'idle', type: 'sequence', frames: [{ path: 'frames/a.png', durationMs: 100 }] });
  fixtures.push(
    rejectFixture(
      'reject-id-duplicate',
      'id_duplicate',
      { where: '$.clips[1].id', value: 'idle' },
      ['两个 Clip 使用同一 ID；目录 ID 必须唯一'],
      dupId,
      new Map([['frames/a.png', { png: { width: 8, height: 8, rgba: rectOps(8, 8, [{ op: 'rect', args: [0, 0, 8, 8, PAPER] }]) } }]]),
    ),
  );

  const missingClip = baseManifest();
  missingClip.actions[0].clip = 'ghost';
  fixtures.push(
    rejectFixture(
      'reject-action-missing-clip',
      'action_clip_missing',
      { where: '$.actions[0].clip', value: 'ghost' },
      ['动作引用不存在的 Clip'],
      missingClip,
      new Map([['frames/a.png', { png: { width: 8, height: 8, rgba: rectOps(8, 8, [{ op: 'rect', args: [0, 0, 8, 8, PAPER] }]) } }]]),
    ),
  );

  const missingDefault = baseManifest();
  missingDefault.defaultAction = 'ghost';
  fixtures.push(
    rejectFixture(
      'reject-default-action-missing',
      'default_action_missing',
      { where: '$.defaultAction', value: 'ghost' },
      ['defaultAction 指向不存在的动作'],
      missingDefault,
      new Map([['frames/a.png', { png: { width: 8, height: 8, rgba: rectOps(8, 8, [{ op: 'rect', args: [0, 0, 8, 8, PAPER] }]) } }]]),
    ),
  );

  const pathCases = [
    ['reject-path-escape', 'path_escape', '../escape.png', ['帧路径含 .. 组件，可逃出包根']],
    ['reject-path-absolute', 'path_absolute', 'C:/frames/a.png', ['Windows 盘符绝对路径']],
    ['reject-path-backslash', 'path_backslash', 'frames\\a.png', ['反斜线分隔符不在允许集']],
    ['reject-path-unc', 'path_unc', '//server/share/a.png', ['UNC/根外路径']],
    ['reject-path-dot-component', 'path_dot_component', 'frames/./a.png', ['含 . 组件']],
    ['reject-path-empty-component', 'path_empty_component', 'frames//a.png', ['空组件']],
    ['reject-path-device', 'path_device', 'CON', ['设备名路径']],
    ['reject-path-colon', 'path_colon', 'frames/a.png:stream', ['NTFS 备用数据流冒号']],
  ];
  for (const [name, code, value, notes] of pathCases) {
    fixtures.push(
      rejectFixture(name, code, { where: '$.clips[0].frames[0].path', value }, notes, baseManifest('idle', value), new Map()),
    );
  }

  fixtures.push(
    rejectFixture(
      'reject-duration-zero',
      'duration_zero',
      { where: '$.clips[0].frames[0].durationMs', value: 0 },
      ['零时长明确拒绝，不猜 FPS、不静默钳制'],
      baseManifest('idle', 'frames/a.png', 0),
      new Map([['frames/a.png', { png: { width: 8, height: 8, rgba: rectOps(8, 8, [{ op: 'rect', args: [0, 0, 8, 8, PAPER] }]) } }]]),
    ),
  );

  fixtures.push(
    rejectFixture(
      'reject-duration-over-max',
      'duration_over_max',
      { where: '$.clips[0].frames[0].durationMs', value: 600001 },
      ['单帧时长 600001ms 超过 600000ms 上限'],
      baseManifest('idle', 'frames/a.png', 600001),
      new Map([['frames/a.png', { png: { width: 8, height: 8, rgba: rectOps(8, 8, [{ op: 'rect', args: [0, 0, 8, 8, PAPER] }]) } }]]),
    ),
  );

  fixtures.push(
    rejectFixture(
      'reject-duration-clip-total',
      'duration_clip_total',
      { where: '$.clips[0].frames[*].durationMs 之和', value: 4200000 },
      ['7 帧 × 600000ms 超过单 Clip 3600000ms 上限（引用同一文件）'],
      {
        ...baseManifest(),
        clips: [
          {
            id: 'idle',
            type: 'sequence',
            frames: Array.from({ length: 7 }, () => ({ path: 'frames/a.png', durationMs: 600000 })),
          },
        ],
      },
      new Map([['frames/a.png', { png: { width: 8, height: 8, rgba: rectOps(8, 8, [{ op: 'rect', args: [0, 0, 8, 8, PAPER] }]) } }]]),
    ),
  );

  fixtures.push(
    rejectFixture(
      'reject-frames-per-clip',
      'frames_per_clip',
      { where: '$.clips[0].frames.length', value: 257 },
      ['单 Clip 257 帧超过 256 上限（重复引用同一文件）'],
      {
        ...baseManifest(),
        clips: [
          {
            id: 'idle',
            type: 'sequence',
            frames: Array.from({ length: 257 }, () => ({ path: 'frames/a.png', durationMs: 10 })),
          },
        ],
      },
      new Map([['frames/a.png', { png: { width: 8, height: 8, rgba: rectOps(8, 8, [{ op: 'rect', args: [0, 0, 8, 8, PAPER] }]) } }]]),
    ),
  );

  fixtures.push(
    rejectFixture(
      'reject-frames-total',
      'frames_total',
      { where: '$.clips[*].frames 总数', value: 600 },
      ['3 个 Clip × 200 帧 = 600 帧，超过全包 512 帧上限'],
      {
        ...baseManifest(),
        clips: ['a', 'b', 'c'].map((id) => ({
          id,
          type: 'sequence',
          frames: Array.from({ length: 200 }, () => ({ path: 'frames/a.png', durationMs: 10 })),
        })),
        actions: ['a', 'b', 'c'].map((id) => ({ id, label: `动作${id}`, clip: id, mode: 'loop' })),
        defaultAction: 'a',
      },
      new Map([['frames/a.png', { png: { width: 8, height: 8, rgba: rectOps(8, 8, [{ op: 'rect', args: [0, 0, 8, 8, PAPER] }]) } }]]),
    ),
  );

  const bigPng = (() => {
    const width = 513;
    const height = 513;
    const rgba = rectOps(width, height, []);
    return encodePng({ width, height, rgba, filters: 'none' });
  })();
  fixtures.push(
    rejectFixture(
      'reject-frame-dimension',
      'frame_dimension',
      { where: 'frames/big.png IHDR', value: '513x513' },
      ['单帧 513x513 超过 512 上限；文件本身很小，必须在解码前按头拒绝'],
      baseManifest('idle', 'frames/big.png', 100),
      new Map([['frames/big.png', { pngBytes: bigPng }]]),
    ),
  );

  fixtures.push(
    rejectFixture(
      'reject-missing-asset',
      'asset_missing',
      { where: '$.clips[0].frames[0].path', value: 'frames/missing.png' },
      ['清单引用的文件不存在；不得静默跳过或回退占位图'],
      baseManifest('idle', 'frames/missing.png', 100),
      new Map(),
    ),
  );

  // 坏 PNG 流：在合法 PNG 基础上精确制造单点缺陷。
  const pngChunks = splitPngChunks(png());
  const idatIndex = pngChunks.findIndex((chunk) => chunk.type === 'IDAT');

  const badSignature = Buffer.from(png());
  badSignature[0] ^= 0xff;
  fixtures.push(
    rejectFixture(
      'reject-png-bad-signature',
      'png_signature',
      { where: '文件头', value: '0x89 被改为 0x76' },
      ['PNG 魔数错误；扩展名不证明格式'],
      baseManifest('idle', 'frames/bad.png', 100),
      new Map([['frames/bad.png', { pngBytes: badSignature }]]),
    ),
  );

  const apngChunks = pngChunks.map((chunk) => ({ ...chunk }));
  apngChunks.splice(idatIndex, 0, {
    type: 'acTL',
    data: (() => {
      const data = Buffer.alloc(8);
      data.writeUInt32BE(1, 0);
      data.writeUInt32BE(0, 4);
      return data;
    })(),
  });
  fixtures.push(
    rejectFixture(
      'reject-png-apng',
      'png_apng',
      { where: 'IHDR 之后', value: '存在 acTL 块' },
      ['APNG 不在静态 PNG 支持范围，明确拒绝而不是只播首帧'],
      baseManifest('idle', 'frames/anim.png', 100),
      new Map([['frames/anim.png', { pngBytes: joinPngChunks(apngChunks) }]]),
    ),
  );

  const corruptCrc = Buffer.from(png());
  {
    const chunk = splitPngChunks(png())[idatIndex];
    // 定位 IDAT 块尾部 CRC 并翻转一位；数据区与 deflate 流保持有效。
    let offset = 8;
    for (let index = 0; index < idatIndex; index += 1) {
      offset += 12 + pngChunks[index].data.length;
    }
    const crcOffset = offset + 8 + chunk.data.length;
    corruptCrc[crcOffset] ^= 0x01;
  }
  fixtures.push(
    rejectFixture(
      'reject-png-bad-crc',
      'png_crc',
      { where: 'IDAT 块 CRC', value: '单比特翻转' },
      ['IDAT 载荷完好但 CRC 错误，须拒绝而不是信任解压结果'],
      baseManifest('idle', 'frames/crc.png', 100),
      new Map([['frames/crc.png', { pngBytes: corruptCrc }]]),
    ),
  );

  const corruptIdatChunks = pngChunks.map((chunk) => ({ ...chunk }));
  {
    const data = Buffer.from(corruptIdatChunks[idatIndex].data);
    data[Math.floor(data.length / 2)] ^= 0xff;
    corruptIdatChunks[idatIndex].data = data;
  }
  fixtures.push(
    rejectFixture(
      'reject-png-corrupt-idat',
      'png_deflate',
      { where: 'IDAT zlib 流', value: '中部字节被翻转（CRC 已重算）' },
      ['deflate 流损坏但块 CRC 正确，须在解压阶段失败'],
      baseManifest('idle', 'frames/corrupt.png', 100),
      new Map([['frames/corrupt.png', { pngBytes: joinPngChunks(corruptIdatChunks) }]]),
    ),
  );

  {
    const full = png();
    const truncated = full.subarray(0, 8 + 25 + 40);
    fixtures.push(
      rejectFixture(
        'reject-png-truncated',
        'png_truncated',
        { where: 'IDAT 中段', value: `截断至 ${truncated.length}/${full.length} 字节` },
        ['无 IEND 且 IDAT 不完整；不交付部分帧'],
        baseManifest('idle', 'frames/cut.png', 100),
        new Map([['frames/cut.png', { pngBytes: Buffer.from(truncated) }]]),
      ),
    );
  }

  // 坏 GIF 流。
  const gifSpec = gifDisposalSpec();
  {
    const zeroDelay = {
      ...gifSpec,
      frames: gifSpec.frames.map((frame, index) =>
        index === 1 ? { ...frame, delayCs: 0 } : frame,
      ),
    };
    fixtures.push(
      rejectFixture(
        'reject-gif-zero-delay',
        'gif_zero_delay',
        { where: 'clips/zero.gif 第 2 帧 GCE', value: 'delay=0' },
        ['GIF 内嵌零延时；不得猜 FPS 或替换为默认时长'],
        {
          ...baseManifest(),
          clips: [{ id: 'idle', type: 'gif', path: 'clips/zero.gif' }],
          actions: [{ id: 'idle', label: '待机', clip: 'idle', mode: 'loop' }],
          defaultAction: 'idle',
        },
        new Map([['clips/zero.gif', { gif: zeroDelay }]]),
      ),
    );
  }

  {
    const gif = encodeGif(gifSpec);
    const truncated = gif.subarray(0, Math.floor(gif.length * 0.55));
    fixtures.push(
      rejectFixture(
        'reject-gif-truncated',
        'gif_truncated',
        { where: 'LZW 数据子块中段', value: `截断至 ${truncated.length}/${gif.length} 字节` },
        ['GIF 帧数据被截断且无 trailer；不交付部分帧'],
        {
          ...baseManifest(),
          clips: [{ id: 'idle', type: 'gif', path: 'clips/cut.gif' }],
          actions: [{ id: 'idle', label: '待机', clip: 'idle', mode: 'loop' }],
          defaultAction: 'idle',
        },
        new Map([['clips/cut.gif', { gifBytes: Buffer.from(truncated) }]]),
      ),
    );
  }

  {
    const gif = encodeGif(gifSpec);
    const noTrailer = gif.subarray(0, gif.length - 1);
    fixtures.push(
      rejectFixture(
        'reject-gif-missing-trailer',
        'gif_trailer',
        { where: '文件尾', value: '缺少 0x3B trailer' },
        ['GIF 缺 trailer；结构不完整即拒绝'],
        {
          ...baseManifest(),
          clips: [{ id: 'idle', type: 'gif', path: 'clips/notrailer.gif' }],
          actions: [{ id: 'idle', label: '待机', clip: 'idle', mode: 'loop' }],
          defaultAction: 'idle',
        },
        new Map([['clips/notrailer.gif', { gifBytes: Buffer.from(noTrailer) }]]),
      ),
    );
  }

  {
    // 单帧、无 NETSCAPE、无局部调色板：图像描述符偏移可确定性计算，
    // 直接把帧宽改成 65535，制造“结构合法但几何非法”的样本。
    const single = {
      width: 8,
      height: 8,
      palette: GIF_PALETTE,
      transparentIndex: null,
      loopCount: null,
      frames: [
        {
          index: 0,
          left: 0,
          top: 0,
          width: 8,
          height: 8,
          disposal: 0,
          delayCs: 5,
          indices: indexPattern(8, 8, () => 1),
        },
      ],
    };
    const gif = Buffer.from(encodeGif(single));
    const descriptorOffset = 13 + GIF_PALETTE.length * 3 + 8;
    if (gif[descriptorOffset] !== 0x2c) {
      throw new Error(`GIF 图像描述符偏移假设失效：0x${gif[descriptorOffset].toString(16)}`);
    }
    const patched = Buffer.from(gif);
    patched.writeUInt16LE(0xffff, descriptorOffset + 5);
    fixtures.push(
      rejectFixture(
        'reject-gif-frame-geometry',
        'gif_geometry',
        { where: 'clips/geom.gif 首帧图像描述符', value: 'width=65535 超出画布/512 上限' },
        ['帧矩形超出逻辑屏幕或 512 上限，须在分配缓冲前拒绝'],
        {
          ...baseManifest(),
          clips: [{ id: 'idle', type: 'gif', path: 'clips/geom.gif' }],
          actions: [{ id: 'idle', label: '待机', clip: 'idle', mode: 'loop' }],
          defaultAction: 'idle',
        },
        new Map([['clips/geom.gif', { gifBytes: patched }]]),
      ),
    );
  }

  return fixtures;
}

// ---------------------------------------------------------------------------
// 预算级样本（仅 --budget，且必须落在 target/ 下）
// ---------------------------------------------------------------------------

function buildBudgetFixtures() {
  const fixtures = [];
  const width = 512;
  const height = 512;
  const flat = rectOps(width, height, [{ op: 'rect', args: [0, 0, width, height, PAPER] }]);
  const flatPng = encodePng({ width, height, rgba: flat, filters: 'none' });

  // 编码文件超 8MiB：像素仍在 512x512 内，用合法对比度文本块把编码体积推过上限。
  {
    const padding = Buffer.alloc(9 * 1024 * 1024, 0x20);
    const padded = joinPngChunks([
      { type: 'IHDR', data: splitPngChunks(flatPng).find((chunk) => chunk.type === 'IHDR').data },
      { type: 'tEXt', data: padding },
      { type: 'IDAT', data: splitPngChunks(flatPng).find((chunk) => chunk.type === 'IDAT').data },
      { type: 'IEND', data: Buffer.alloc(0) },
    ]);
    fixtures.push({
      entry: {
        name: 'budget-encoded-file-over-8mib',
        expect: 'reject',
        rejectCode: 'encoded_file_over_8mib',
        budgetOnly: true,
        defect: { where: 'frames/huge.png 编码字节数', value: `${padded.length} > 8MiB` },
        notes: ['单文件编码体积超过 8MiB，须在读取/校验前按文件长度拒绝'],
      },
      files: new Map([['frames/huge.png', { pngBytes: padded }]]),
      manifest: baseManifest('idle', 'frames/huge.png', 100),
    });
  }

  // 全包编码累计超 16MiB：3 个约 6MiB 文件，每个都在 8MiB 以内。
  {
    const files = new Map();
    const frames = [];
    for (let index = 0; index < 3; index += 1) {
      const padding = Buffer.alloc(6 * 1024 * 1024, 0x20);
      const padded = joinPngChunks([
        { type: 'IHDR', data: splitPngChunks(flatPng).find((chunk) => chunk.type === 'IHDR').data },
        { type: 'tEXt', data: padding },
        { type: 'IDAT', data: splitPngChunks(flatPng).find((chunk) => chunk.type === 'IDAT').data },
        { type: 'IEND', data: Buffer.alloc(0) },
      ]);
      const file = `frames/part${index}.png`;
      files.set(file, { pngBytes: padded });
      frames.push({ path: file, durationMs: 100 });
    }
    fixtures.push({
      entry: {
        name: 'budget-encoded-total-over-16mib',
        expect: 'reject',
        rejectCode: 'encoded_total_over_16mib',
        budgetOnly: true,
        defect: { where: '全包读取字节数', value: '3 × 6MiB ≈ 18MiB > 16MiB' },
        notes: ['单文件均 <8MiB，但累计读取超过 16MiB；须在打开第 3 个文件前拒绝'],
      },
      files,
      manifest: { ...baseManifest(), clips: [{ id: 'idle', type: 'sequence', frames }] },
    });
  }

  // 解码像素超 32MiB：33 帧 512x512 引用同一扁平文件（编码仅数 KB）。
  {
    const frames = Array.from({ length: 33 }, () => ({ path: 'frames/flat.png', durationMs: 100 }));
    fixtures.push({
      entry: {
        name: 'budget-decoded-pixels-over-32mib',
        expect: 'reject',
        rejectCode: 'decoded_pixels_over_32mib',
        budgetOnly: true,
        defect: { where: '单包解码帧累计', value: `33 × 512x512x4 = ${(33 * 512 * 512 * 4 / (1024 * 1024)).toFixed(1)}MiB > 32MiB` },
        notes: ['编码很小但解码像素累计超预算，必须在分配/解码前按几何预算拒绝'],
      },
      files: new Map([['frames/flat.png', { pngBytes: flatPng }]]),
      manifest: { ...baseManifest(), clips: [{ id: 'idle', type: 'sequence', frames }] },
    });
  }

  // 上限满帧：512 帧 × 512x512（解码约 512MiB），用于显式压测拒绝路径。
  {
    const frames = Array.from({ length: 512 }, () => ({ path: 'frames/flat.png', durationMs: 1 }));
    fixtures.push({
      entry: {
        name: 'budget-max-frames-512',
        expect: 'reject',
        rejectCode: 'decoded_pixels_over_32mib',
        budgetOnly: true,
        defect: { where: '单包解码帧累计', value: `512 × 512x512x4 = 512MiB` },
        notes: ['帧数合法（512）但解码像素远超 32MiB，用于验证先预算后解码'],
      },
      files: new Map([['frames/flat.png', { pngBytes: flatPng }]]),
      manifest: { ...baseManifest(), clips: [{ id: 'idle', type: 'sequence', frames }] },
    });
  }

  return fixtures;
}

// ---------------------------------------------------------------------------
// 写出与索引
// ---------------------------------------------------------------------------

function assetBytes(asset) {
  if (asset.png !== undefined) {
    const { width, height, rgba, interlace = false, filters = 'cycle' } = asset.png;
    return encodePng({ width, height, rgba, interlace, filters });
  }
  if (asset.pngBytes !== undefined) return asset.pngBytes;
  if (asset.gif !== undefined) return encodeGif(asset.gif);
  if (asset.gifBytes !== undefined) return asset.gifBytes;
  throw new Error('未知素材条目');
}

export function buildFixtures({ budget = false } = {}) {
  const fixtures = [
    fixtureSequenceBasic(),
    fixtureGifDisposal(),
    fixtureGifInterlaced(),
    fixtureCombo(),
    fixtureMinimal(),
    fixtureMaxDuration(),
    fixtureMaxFrames(),
    ...buildRejectFixtures(),
  ];
  if (budget) fixtures.push(...buildBudgetFixtures());
  return fixtures;
}

function manifestText(fixture) {
  if (fixture.rawManifest !== undefined) return fixture.rawManifest;
  return `${JSON.stringify(fixture.manifest, null, 2)}\n`;
}

export function writeFixtures({ outDir, budget = false, only = null, log = () => {} }) {
  const fixtures = buildFixtures({ budget }).filter(
    (fixture) => only === null || only.includes(fixture.entry.name),
  );
  const indexEntries = [];
  for (const fixture of fixtures) {
    const entry = { ...fixture.entry };
    const packageDir = path.join(outDir, 'packages', entry.name);
    mkdirSync(packageDir, { recursive: true });
    writeFileSync(path.join(packageDir, 'manifest.json'), manifestText(fixture), 'utf8');
    for (const [relative, asset] of fixture.files) {
      const target = path.join(packageDir, ...relative.split('/'));
      mkdirSync(path.dirname(target), { recursive: true });
      writeFileSync(target, assetBytes(asset));
    }
    entry.packageDir = path.posix.join('packages', entry.name);
    if (fixture.expected !== undefined) {
      const expectedDir = path.join(outDir, 'expected');
      mkdirSync(expectedDir, { recursive: true });
      const expectedFile = path.join(expectedDir, `${entry.name}.json`);
      writeFileSync(
        expectedFile,
        `${JSON.stringify(
          {
            fixture: entry.name,
            source: 'apps/moe-buddy-tauri/tests/material-fixtures.mjs',
            derivation: '由构造场景直接推导，未经任何解码器',
            ...fixture.expected,
          },
          null,
          2,
        )}\n`,
        'utf8',
      );
      entry.expectedFile = path.posix.join('expected', `${entry.name}.json`);
    }
    indexEntries.push(entry);
    log(`  ${entry.expect === 'accept' ? '接受' : '拒绝'} ${entry.name}${entry.budgetOnly ? '（预算样本）' : ''}`);
  }
  const index = {
    formatVersion: 1,
    contract: 'docs/GATEWAY.md#c-素材包与播放器施工契约2026-10-19',
    generator: 'apps/moe-buddy-tauri/tests/material-fixtures.mjs',
    verify: 'apps/moe-buddy-tauri/tests/material-fixtures-verify.mjs',
    note: 'rejectCode 是本夹具包的人读标签，不是已冻结 IPC/错误码；宿主接线时由领队统一映射',
    generatedWithBudget: budget,
    fixtures: indexEntries,
  };
  writeFileSync(path.join(outDir, 'index.json'), `${JSON.stringify(index, null, 2)}\n`, 'utf8');
  return index;
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

function parseArgs(argv) {
  const options = { outDir: null, budget: false, only: null, list: false, verify: false, help: false };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === '--out') options.outDir = argv[++i] ?? null;
    else if (arg === '--budget') options.budget = true;
    else if (arg === '--only') options.only = (argv[++i] ?? '').split(',').map((name) => name.trim()).filter(Boolean);
    else if (arg === '--list') options.list = true;
    else if (arg === '--verify') options.verify = true;
    else if (arg === '--help' || arg === '-h') options.help = true;
    else if (arg.startsWith('--')) throw new Error(`未知参数 ${arg}`);
  }
  return options;
}

function assertSafeOutDir(outDir, budget) {
  const resolved = path.resolve(outDir);
  if (resolved === path.parse(resolved).root) throw new Error('拒绝把输出写到文件系统根');
  const root = locateRepoRoot();
  if (resolved === root) throw new Error('拒绝把输出写到仓库根');
  if (budget) {
    const segments = resolved.split(path.sep).map((segment) => segment.toLowerCase());
    if (!segments.includes('target')) {
      throw new Error('预算级样本必须显式写入 target/ 下的临时目录，避免污染工作区');
    }
  } else if (!resolved.toLowerCase().split(path.sep).includes('target')) {
    console.warn(`[material-fixtures] 输出目录不在 target/ 下：${resolved}`);
  }
  return resolved;
}

function usage() {
  return [
    '用法：node tests/material-fixtures.mjs [--out <目录>] [--budget] [--only <名称,...>] [--verify] [--list]',
    '',
    '  --out     输出目录；默认 <仓库>/target/c-material-fixtures（git 忽略）',
    '  --budget  追加编码/像素预算级大样本；要求 --out 落在 target/ 下',
    '  --only    只生成指定夹具（逗号分隔，用于单点调试）',
    '  --verify  生成后立即调用 material-fixtures-verify.mjs 做格式自检',
    '  --list    只列出夹具名称与期望，不写文件',
    '',
    '只写指定目录，不递归删除、不读取 daily、不联网；不产生解码器依赖。',
  ].join('\n');
}

function main() {
  const options = parseArgs(process.argv.slice(2));
  if (options.help) {
    process.stdout.write(`${usage()}\n`);
    return;
  }
  if (options.list) {
    for (const fixture of buildFixtures({ budget: options.budget })) {
      process.stdout.write(
        `${fixture.entry.expect === 'accept' ? 'accept' : `reject:${fixture.entry.rejectCode}`}  ${fixture.entry.name}\n`,
      );
    }
    return;
  }
  const outDir = assertSafeOutDir(
    options.outDir ?? path.join(locateRepoRoot(), 'target', 'c-material-fixtures'),
    options.budget,
  );
  mkdirSync(outDir, { recursive: true });
  process.stdout.write(`[material-fixtures] 输出目录 ${outDir}\n`);
  const index = writeFixtures({
    outDir,
    budget: options.budget,
    only: options.only,
    log: (line) => process.stdout.write(`${line}\n`),
  });
  const accepts = index.fixtures.filter((entry) => entry.expect === 'accept').length;
  process.stdout.write(
    `[material-fixtures] 完成：${index.fixtures.length} 个夹具（接受 ${accepts} / 拒绝 ${index.fixtures.length - accepts}），预算样本 ${options.budget ? '已生成' : '未生成'}\n`,
  );
  if (options.verify) {
    const result = spawnSync(process.execPath, [path.join(HERE, 'material-fixtures-verify.mjs'), '--out', outDir], {
      stdio: 'inherit',
    });
    if (result.error) throw result.error;
    if (result.status !== 0) process.exitCode = result.status ?? 1;
  }
}

if (process.argv[1] !== undefined && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    main();
  } catch (error) {
    console.error(`[material-fixtures] ${error instanceof Error ? error.message : String(error)}`);
    process.exitCode = 1;
  }
}
