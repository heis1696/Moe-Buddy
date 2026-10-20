#!/usr/bin/env node
/**
 * C 阶段合成素材夹具自检器（仅 Node 标准库）。
 *
 * 定位：**格式自检 / 夹具自证**，不是生产解码器，也不是播放器验收。
 * - 独立重解本仓库生成器写出的微型 PNG/GIF（自带最小解析器，不复用生成器代码）；
 * - 与 `expected/*.json` 里的期望 RGBA、帧时长、循环提示逐像素比对；
 * - 按 GATEWAY C 契约复核清单结构、路径、预算；
 * - 对每个拒绝夹具证明“缺陷确实存在于字节里”，防止夹具退化成空壳。
 *
 * 已知不覆盖（生产解码必须另行验证）：APNG、非 8 位/非 RGBA 的 PNG、其他
 * 压缩/滤波方法的 PNG、GIF 以外的图片格式、真实像素缩放/合成管线。
 */

import { createHash } from 'node:crypto';
import { existsSync, readFileSync, statSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { inflateSync } from 'node:zlib';

const HERE = path.dirname(fileURLToPath(import.meta.url));

const MANIFEST_MAX_BYTES = 64 * 1024;
const MAX_CLIPS = 32;
const MAX_ACTIONS = 32;
const MAX_FRAMES_PER_CLIP = 256;
const MAX_FRAMES_TOTAL = 512;
const MAX_FRAME_DIMENSION = 512;
const MAX_ENCODED_FILE_BYTES = 8 * 1024 * 1024;
const MAX_ENCODED_TOTAL_BYTES = 16 * 1024 * 1024;
const MAX_DECODED_PIXEL_BYTES = 32 * 1024 * 1024;
const MAX_FRAME_DURATION_MS = 600000;
const MAX_CLIP_DURATION_MS = 3600000;

// ---------------------------------------------------------------------------
// 严格 JSON 扫描（重复键检测；JSON.parse 会静默覆盖，不能用于严格判定）
// ---------------------------------------------------------------------------

/**
 * 极简 JSON 词法扫描，只为了发现同一对象内重复的成员名。
 * 不负责描述错误位置以外的语义解析。
 */
function scanDuplicateKeys(text) {
  const duplicates = [];
  const stack = [];
  let index = 0;
  let expectKey = false;
  let currentKey = null;
  const length = text.length;

  const skipWhitespace = () => {
    while (index < length && /\s/.test(text[index])) index += 1;
  };
  const readString = () => {
    const start = index;
    index += 1;
    while (index < length) {
      const char = text[index];
      if (char === '\\') {
        index += 2;
        continue;
      }
      if (char === '"') {
        index += 1;
        return JSON.parse(text.slice(start, index));
      }
      index += 1;
    }
    throw new Error('JSON 字符串未闭合');
  };
  const skipValue = () => {
    skipWhitespace();
    if (text[index] === '"') {
      readString();
      return;
    }
    if (text[index] === '{' || text[index] === '[') {
      const open = text[index];
      const close = open === '{' ? '}' : ']';
      let depth = 0;
      while (index < length) {
        const char = text[index];
        if (char === '"') {
          readString();
          continue;
        }
        if (char === open) depth += 1;
        else if (char === close) {
          depth -= 1;
          index += 1;
          if (depth === 0) return;
          continue;
        }
        index += 1;
      }
      throw new Error('JSON 容器未闭合');
    }
    while (index < length && !/[\s,\]}]/.test(text[index])) index += 1;
  };

  while (index < length) {
    skipWhitespace();
    if (index >= length) break;
    const char = text[index];
    if (char === '{' || char === '[') {
      stack.push({ open: char, keys: new Set() });
      index += 1;
      expectKey = char === '{';
      continue;
    }
    if (char === '}' || char === ']') {
      stack.pop();
      index += 1;
      expectKey = false;
      continue;
    }
    if (char === ',') {
      index += 1;
      expectKey = stack.length > 0 && stack[stack.length - 1].open === '{';
      continue;
    }
    if (char === ':') {
      index += 1;
      skipValue();
      expectKey = false;
      continue;
    }
    if (char === '"' && expectKey) {
      currentKey = readString();
      const top = stack[stack.length - 1];
      if (top !== undefined && top.keys.has(currentKey)) {
        duplicates.push(currentKey);
      }
      top?.keys.add(currentKey);
      expectKey = false;
      continue;
    }
    index += 1;
  }
  return duplicates;
}

// ---------------------------------------------------------------------------
// PNG 解析（仅 8bit RGBA、filter 0..4、Adam7 交错）
// ---------------------------------------------------------------------------

function readChunks(bytes) {
  const signature = '89504e470d0a1a0a';
  if (bytes.subarray(0, 8).toString('hex') !== signature) {
    throw new Error('PNG 签名错误');
  }
  const chunks = [];
  let offset = 8;
  while (offset + 12 <= bytes.length) {
    const length = bytes.readUInt32BE(offset);
    const type = bytes.subarray(offset + 4, offset + 8).toString('ascii');
    if (offset + 12 + length > bytes.length) throw new Error(`PNG 块 ${type} 被截断`);
    const data = bytes.subarray(offset + 8, offset + 8 + length);
    const storedCrc = bytes.readUInt32BE(offset + 8 + length);
    chunks.push({ type, data, storedCrc, crcBody: bytes.subarray(offset + 4, offset + 8 + length) });
    offset += 12 + length;
  }
  if (chunks.length === 0 || chunks[chunks.length - 1].type !== 'IEND') {
    throw new Error('PNG 缺少 IEND');
  }
  return chunks;
}

function crc32Of(buffer) {
  let c = 0xffffffff;
  for (let i = 0; i < buffer.length; i += 1) {
    c ^= buffer[i];
    for (let k = 0; k < 8; k += 1) c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
  }
  return (c ^ 0xffffffff) >>> 0;
}

function paethPredictor(left, up, upperLeft) {
  const estimate = left + up - upperLeft;
  const dLeft = Math.abs(estimate - left);
  const dUp = Math.abs(estimate - up);
  const dUpperLeft = Math.abs(estimate - upperLeft);
  if (dLeft <= dUp && dLeft <= dUpperLeft) return left;
  if (dUp <= dUpperLeft) return up;
  return upperLeft;
}

function unfilterRows(data, width, height, bpp) {
  const stride = width * bpp;
  const out = Buffer.alloc(stride * height);
  let offset = 0;
  let previous = Buffer.alloc(stride);
  for (let y = 0; y < height; y += 1) {
    if (offset >= data.length) throw new Error('PNG 扫描线不足');
    const filterType = data[offset];
    offset += 1;
    const raw = data.subarray(offset, offset + stride);
    if (raw.length !== stride) throw new Error('PNG 扫描线被截断');
    offset += stride;
    const row = Buffer.alloc(stride);
    for (let i = 0; i < stride; i += 1) {
      const left = i >= bpp ? row[i - bpp] : 0;
      const up = previous[i];
      const upperLeft = i >= bpp ? previous[i - bpp] : 0;
      let predictor = 0;
      if (filterType === 1) predictor = left;
      else if (filterType === 2) predictor = up;
      else if (filterType === 3) predictor = Math.floor((left + up) / 2);
      else if (filterType === 4) predictor = paethPredictor(left, up, upperLeft);
      else if (filterType !== 0) throw new Error(`PNG 未知行过滤器 ${filterType}`);
      row[i] = (raw[i] + predictor) & 0xff;
    }
    row.copy(out, y * stride);
    previous = row;
  }
  return out;
}

const ADAM7 = [
  [0, 0, 8, 8],
  [4, 0, 8, 8],
  [0, 4, 4, 8],
  [2, 0, 4, 4],
  [0, 2, 2, 4],
  [1, 0, 2, 2],
  [0, 1, 1, 2],
];

export function decodePng(bytes) {
  const chunks = readChunks(bytes);
  for (const chunk of chunks) {
    if (crc32Of(chunk.crcBody) !== chunk.storedCrc) {
      throw new Error(`PNG 块 ${chunk.type} CRC 不匹配`);
    }
  }
  const ihdr = chunks.find((chunk) => chunk.type === 'IHDR');
  if (ihdr === undefined) throw new Error('PNG 缺少 IHDR');
  const width = ihdr.data.readUInt32BE(0);
  const height = ihdr.data.readUInt32BE(4);
  const bitDepth = ihdr.data[8];
  const colorType = ihdr.data[9];
  const interlace = ihdr.data[12];
  if (bitDepth !== 8 || colorType !== 6) {
    throw new Error(`自检器只解码 8bit RGBA PNG（bitDepth=${bitDepth}, colorType=${colorType}）`);
  }
  if (chunks.some((chunk) => chunk.type === 'acTL')) throw new Error('PNG 含 APNG acTL');
  const idat = Buffer.concat(chunks.filter((chunk) => chunk.type === 'IDAT').map((chunk) => chunk.data));
  const raw = inflateSync(idat);
  const rgba = Buffer.alloc(width * height * 4);
  if (interlace === 0) {
    unfilterRows(raw, width, height, 4).copy(rgba);
    return { width, height, rgba };
  }
  if (interlace !== 1) throw new Error(`未知 PNG 交错方式 ${interlace}`);
  let offset = 0;
  for (const [startX, startY, stepX, stepY] of ADAM7) {
    const passWidth = Math.ceil((width - startX) / stepX);
    const passHeight = Math.ceil((height - startY) / stepY);
    if (passWidth <= 0 || passHeight <= 0) continue;
    const expectedBytes = (passWidth * 4 + 1) * passHeight;
    const slice = raw.subarray(offset, offset + expectedBytes);
    if (slice.length !== expectedBytes) throw new Error('PNG Adam7 数据不足');
    offset += expectedBytes;
    const pass = unfilterRows(slice, passWidth, passHeight, 4);
    for (let py = 0; py < passHeight; py += 1) {
      for (let px = 0; px < passWidth; px += 1) {
        const source = (py * passWidth + px) * 4;
        const target = ((startY + py * stepY) * width + (startX + px * stepX)) * 4;
        pass.copy(rgba, target, source, source + 4);
      }
    }
  }
  if (offset !== raw.length) throw new Error('PNG Adam7 有多余数据');
  return { width, height, rgba };
}

// ---------------------------------------------------------------------------
// GIF 解析（调色板、透明、局部矩形、交错、disposal、循环提示）
// ---------------------------------------------------------------------------

function lzwDecode(data, minCodeSize) {
  if (minCodeSize < 2 || minCodeSize > 8) throw new Error(`GIF 最小码长 ${minCodeSize} 非法`);
  const clearCode = 1 << minCodeSize;
  const endCode = clearCode + 1;
  let codeSize = minCodeSize + 1;
  let nextCode = endCode + 1;
  let table = [];
  const resetTable = () => {
    table = new Array(clearCode + 2);
    for (let i = 0; i < clearCode; i += 1) table[i] = [i];
    table[clearCode] = [];
    table[endCode] = [];
    codeSize = minCodeSize + 1;
    nextCode = endCode + 1;
  };
  resetTable();

  const output = [];
  let bitBuffer = 0;
  let bitCount = 0;
  let position = 0;
  let previous = null;

  for (;;) {
    while (bitCount < codeSize) {
      if (position >= data.length) throw new Error('GIF LZW 数据提前结束');
      bitBuffer |= data[position] << bitCount;
      position += 1;
      bitCount += 8;
    }
    const code = bitBuffer & ((1 << codeSize) - 1);
    bitBuffer >>>= codeSize;
    bitCount -= codeSize;

    if (code === clearCode) {
      resetTable();
      previous = null;
      continue;
    }
    if (code === endCode) return Buffer.from(output);

    let entry;
    if (code < nextCode && table[code] !== undefined) {
      entry = table[code];
    } else if (code === nextCode && previous !== null) {
      entry = [...previous, previous[0]];
    } else {
      throw new Error(`GIF LZW 非法码 ${code}（nextCode=${nextCode}）`);
    }
    for (const value of entry) output.push(value);
    if (previous !== null && nextCode < 4096) {
      table[nextCode] = [...previous, entry[0]];
      nextCode += 1;
      if (nextCode === 1 << codeSize && codeSize < 12) codeSize += 1;
    }
    previous = entry;
  }
}

function readSubBlocks(bytes, offset) {
  const parts = [];
  let cursor = offset;
  for (;;) {
    if (cursor >= bytes.length) throw new Error('GIF 子块被截断');
    const length = bytes[cursor];
    cursor += 1;
    if (length === 0) break;
    if (cursor + length > bytes.length) throw new Error('GIF 子块数据被截断');
    parts.push(bytes.subarray(cursor, cursor + length));
    cursor += length;
  }
  return { data: Buffer.concat(parts), offset: cursor };
}

function interlaceRows(height) {
  const rows = [];
  for (let y = 0; y < height; y += 8) rows.push(y);
  for (let y = 4; y < height; y += 8) rows.push(y);
  for (let y = 2; y < height; y += 4) rows.push(y);
  for (let y = 1; y < height; y += 2) rows.push(y);
  return rows;
}

export function decodeGif(bytes) {
  if (bytes.subarray(0, 6).toString('ascii') !== 'GIF89a' && bytes.subarray(0, 6).toString('ascii') !== 'GIF87a') {
    throw new Error('GIF 签名错误');
  }
  const width = bytes.readUInt16LE(6);
  const height = bytes.readUInt16LE(8);
  const packed = bytes[10];
  const globalTableBits = (packed & 0x07) + 1;
  let offset = 13;
  let globalPalette = null;
  if (packed & 0x80) {
    const size = 1 << globalTableBits;
    globalPalette = [];
    for (let i = 0; i < size; i += 1) {
      globalPalette.push([bytes[offset + i * 3], bytes[offset + i * 3 + 1], bytes[offset + i * 3 + 2]]);
    }
    offset += size * 3;
  }

  let loopCount = null;
  let pending = { disposal: 0, delayCs: 0, transparentIndex: null };
  const rawFrames = [];
  let sawTrailer = false;

  while (offset < bytes.length) {
    const introducer = bytes[offset];
    if (introducer === 0x3b) {
      offset += 1;
      sawTrailer = true;
      break;
    }
    if (introducer === 0x21) {
      const label = bytes[offset + 1];
      offset += 2;
      if (label === 0xf9) {
        const length = bytes[offset];
        if (length !== 4) throw new Error('GIF GCE 长度非法');
        const control = bytes[offset + 1];
        pending = {
          disposal: (control >> 2) & 0x07,
          delayCs: bytes.readUInt16LE(offset + 2),
          transparentIndex: control & 0x01 ? bytes[offset + 4] : null,
        };
        offset += 5;
        if (bytes[offset] !== 0x00) throw new Error('GIF GCE 缺少终止符');
        offset += 1;
      } else if (label === 0xff) {
        const length = bytes[offset];
        const identifier = bytes.subarray(offset + 1, offset + 1 + length).toString('ascii');
        offset += 1 + length;
        const blocks = readSubBlocks(bytes, offset);
        offset = blocks.offset;
        if (identifier.startsWith('NETSCAPE2.0') && blocks.data.length >= 3 && blocks.data[0] === 0x01) {
          loopCount = blocks.data.readUInt16LE(1);
        }
      } else {
        const blocks = readSubBlocks(bytes, offset);
        offset = blocks.offset;
      }
      continue;
    }
    if (introducer === 0x2c) {
      const left = bytes.readUInt16LE(offset + 1);
      const top = bytes.readUInt16LE(offset + 3);
      const frameWidth = bytes.readUInt16LE(offset + 5);
      const frameHeight = bytes.readUInt16LE(offset + 7);
      const descriptorPacked = bytes[offset + 9];
      offset += 10;
      let palette = globalPalette;
      if (descriptorPacked & 0x80) {
        const size = 1 << ((descriptorPacked & 0x07) + 1);
        palette = [];
        for (let i = 0; i < size; i += 1) {
          palette.push([bytes[offset + i * 3], bytes[offset + i * 3 + 1], bytes[offset + i * 3 + 2]]);
        }
        offset += size * 3;
      }
      if (palette === null) throw new Error('GIF 帧既无全局也无局部调色板');
      const minCodeSize = bytes[offset];
      offset += 1;
      const blocks = readSubBlocks(bytes, offset);
      offset = blocks.offset;
      const indices = lzwDecode(blocks.data, minCodeSize);
      if (indices.length < frameWidth * frameHeight) {
        throw new Error('GIF LZW 输出像素不足');
      }
      rawFrames.push({
        left,
        top,
        width: frameWidth,
        height: frameHeight,
        interlace: (descriptorPacked & 0x40) !== 0,
        palette,
        indices,
        disposal: pending.disposal,
        delayCs: pending.delayCs,
        transparentIndex: pending.transparentIndex,
        hasLocalPalette: (descriptorPacked & 0x80) !== 0,
      });
      pending = { disposal: 0, delayCs: 0, transparentIndex: null };
      continue;
    }
    throw new Error(`GIF 未知块引导字节 0x${introducer.toString(16)}`);
  }
  if (!sawTrailer) throw new Error('GIF 缺少 trailer');
  if (offset < bytes.length) throw new Error('GIF trailer 之后仍有数据');

  // 合成显示帧（独立实现，按 GIF89a disposal 语义）。
  let canvas = Buffer.alloc(width * height * 4, 0);
  const frames = [];
  for (const frame of rawFrames) {
    const saved = frame.disposal === 3 ? Buffer.from(canvas) : null;
    // 交错流按 4 个 pass 传输：第 r 行传输的是画布行 transmitToCanvas[r]。
    const transmitToCanvas = frame.interlace ? interlaceRows(frame.height) : null;
    const canvasToTransmit = new Array(frame.height);
    if (transmitToCanvas !== null) {
      transmitToCanvas.forEach((canvasRow, transmitRow) => {
        canvasToTransmit[canvasRow] = transmitRow;
      });
    }
    for (let row = 0; row < frame.height; row += 1) {
      const sourceRow = transmitToCanvas === null ? row : canvasToTransmit[row];
      for (let x = 0; x < frame.width; x += 1) {
        const paletteIndex = frame.indices[sourceRow * frame.width + x];
        if (frame.transparentIndex !== null && paletteIndex === frame.transparentIndex) continue;
        const color = frame.palette[paletteIndex];
        if (color === undefined) throw new Error(`GIF 调色板下标 ${paletteIndex} 越界`);
        const target = ((frame.top + row) * width + (frame.left + x)) * 4;
        if (target < 0 || target + 3 >= canvas.length) throw new Error('GIF 帧矩形超出画布');
        canvas[target] = color[0];
        canvas[target + 1] = color[1];
        canvas[target + 2] = color[2];
        canvas[target + 3] = 255;
      }
    }
    frames.push({
      rgba: Buffer.from(canvas),
      delayMs: frame.delayCs * 10,
      disposal: frame.disposal,
      transparent: frame.transparentIndex !== null,
      localPalette: frame.hasLocalPalette,
      interlace: frame.interlace,
    });
    if (frame.disposal === 2) {
      for (let row = 0; row < frame.height; row += 1) {
        for (let x = 0; x < frame.width; x += 1) {
          const target = ((frame.top + row) * width + (frame.left + x)) * 4;
          canvas.fill(0, target, target + 4);
        }
      }
    } else if (frame.disposal === 3 && saved !== null) {
      canvas = saved;
    }
  }
  return { width, height, frames, loopCount };
}

// ---------------------------------------------------------------------------
// 清单校验（GATEWAY C 契约）
// ---------------------------------------------------------------------------

const ROOT_KEYS = ['formatVersion', 'id', 'version', 'label', 'clips', 'actions', 'defaultAction'];
const CLIP_SEQUENCE_KEYS = ['id', 'type', 'frames'];
const CLIP_GIF_KEYS = ['id', 'type', 'path'];
const FRAME_KEYS = ['path', 'durationMs'];
const ACTION_KEYS = ['id', 'label', 'clip', 'mode'];

const ID_PATTERN = /^[A-Za-z0-9_-]{1,64}$/;

function unknownKeys(value, allowed) {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) return null;
  return Object.keys(value).filter((key) => !allowed.includes(key));
}

function controlScalar(value) {
  for (const char of value) {
    const code = char.codePointAt(0);
    if (code < 0x20 || code === 0x7f) return true;
    if (code >= 0x80 && code <= 0x9f) return true;
  }
  return false;
}

function labelProblem(value) {
  if (typeof value !== 'string') return 'not-string';
  const scalars = [...value];
  if (scalars.length < 1 || scalars.length > 64) return 'length';
  if (controlScalar(value)) return 'control';
  return null;
}

function pathProblem(value) {
  if (typeof value !== 'string' || value.length === 0) return 'empty';
  if (value.includes('\\')) return 'backslash';
  if (/[\u0000-\u001f\u007f]/.test(value)) return 'control';
  if (/^[A-Za-z]:/.test(value)) return 'drive';
  if (value.includes(':')) return 'colon';
  if (value.startsWith('/')) return 'absolute';
  const lower = value.toLowerCase();
  if (/^(con|prn|aux|nul|com\d|lpt\d)(\..*)?$/.test(lower)) return 'device';
  const parts = value.split('/');
  for (const part of parts) {
    if (part === '') return 'empty_component';
    if (part === '.') return 'dot_component';
    if (part === '..') return 'escape';
  }
  return null;
}

function isSafeInteger(value) {
  return typeof value === 'number' && Number.isSafeInteger(value);
}

/** 返回问题列表；空数组表示按 C 契约结构合法。 */
function validateManifest(manifest) {
  const problems = [];
  if (typeof manifest !== 'object' || manifest === null || Array.isArray(manifest)) {
    return ['根节点不是对象'];
  }
  const extra = unknownKeys(manifest, ROOT_KEYS);
  if (extra !== null && extra.length > 0) problems.push(`未知字段 ${extra.join(',')}`);
  if (manifest.formatVersion !== 1) problems.push('formatVersion 必须为 1');
  if (typeof manifest.id !== 'string' || !ID_PATTERN.test(manifest.id)) problems.push('根 id 非法');
  if (!isSafeInteger(manifest.version) || manifest.version <= 0) problems.push('version 必须为正安全整数');
  const labelIssue = labelProblem(manifest.label);
  if (labelIssue !== null) problems.push(`label ${labelIssue}`);
  if (!Array.isArray(manifest.clips)) {
    problems.push('clips 必须为数组');
    return problems;
  }
  if (!Array.isArray(manifest.actions)) {
    problems.push('actions 必须为数组');
    return problems;
  }
  if (manifest.clips.length > MAX_CLIPS) problems.push('Clip 数量超过 32');
  if (manifest.actions.length > MAX_ACTIONS) problems.push('动作数量超过 32');

  const clipIds = new Set();
  const actionIds = new Set();
  const clipById = new Map();
  let totalFrames = 0;

  manifest.clips.forEach((clip, index) => {
    const where = `clips[${index}]`;
    if (typeof clip !== 'object' || clip === null) {
      problems.push(`${where} 不是对象`);
      return;
    }
    for (const key of unknownKeys(clip, [...new Set([...CLIP_SEQUENCE_KEYS, ...CLIP_GIF_KEYS])]) ?? []) {
      problems.push(`${where} 未知字段 ${key}`);
    }
    if (typeof clip.id !== 'string' || !ID_PATTERN.test(clip.id)) problems.push(`${where}.id 非法`);
    else if (clipIds.has(clip.id)) problems.push(`${where}.id 重复`);
    else clipIds.add(clip.id);
    if (clip.id !== undefined) clipById.set(clip.id, clip);
    if (clip.type === 'sequence') {
      if (!Array.isArray(clip.frames) || clip.frames.length === 0) {
        problems.push(`${where}.frames 必须为非空数组`);
        return;
      }
      if (clip.frames.length > MAX_FRAMES_PER_CLIP) problems.push(`${where}.frames 超过 256`);
      totalFrames += clip.frames.length;
      let clipTotal = 0;
      clip.frames.forEach((frame, frameIndex) => {
        const frameWhere = `${where}.frames[${frameIndex}]`;
        for (const key of unknownKeys(frame, FRAME_KEYS) ?? []) problems.push(`${frameWhere} 未知字段 ${key}`);
        const issue = pathProblem(frame.path);
        if (issue !== null) problems.push(`${frameWhere}.path ${issue}`);
        if (!isSafeInteger(frame.durationMs) || frame.durationMs < 1) {
          problems.push(`${frameWhere}.durationMs 必须为正整数`);
        } else if (frame.durationMs > MAX_FRAME_DURATION_MS) {
          problems.push(`${frameWhere}.durationMs 超过 600000`);
        } else {
          clipTotal += frame.durationMs;
        }
      });
      if (clipTotal > MAX_CLIP_DURATION_MS) problems.push(`${where} 总时长超过 3600000`);
    } else if (clip.type === 'gif') {
      const issue = pathProblem(clip.path);
      if (issue !== null) problems.push(`${where}.path ${issue}`);
    } else {
      problems.push(`${where}.type 未知`);
    }
  });

  if (totalFrames > MAX_FRAMES_TOTAL) problems.push('全包帧数超过 512');

  manifest.actions.forEach((action, index) => {
    const where = `actions[${index}]`;
    if (typeof action !== 'object' || action === null) {
      problems.push(`${where} 不是对象`);
      return;
    }
    for (const key of unknownKeys(action, ACTION_KEYS) ?? []) problems.push(`${where} 未知字段 ${key}`);
    if (typeof action.id !== 'string' || !ID_PATTERN.test(action.id)) problems.push(`${where}.id 非法`);
    else if (actionIds.has(action.id)) problems.push(`${where}.id 重复`);
    else actionIds.add(action.id);
    const issue = labelProblem(action.label);
    if (issue !== null) problems.push(`${where}.label ${issue}`);
    if (typeof action.clip !== 'string' || !clipIds.has(action.clip)) problems.push(`${where}.clip 不存在`);
    if (action.mode !== 'once' && action.mode !== 'loop') problems.push(`${where}.mode 非法`);
  });

  if (typeof manifest.defaultAction !== 'string' || !actionIds.has(manifest.defaultAction)) {
    problems.push('defaultAction 不存在');
  }
  return problems;
}

// ---------------------------------------------------------------------------
// 夹具包读取
// ---------------------------------------------------------------------------

function readManifest(packageDir) {
  const file = path.join(packageDir, 'manifest.json');
  if (!existsSync(file)) throw new Error('缺少 manifest.json');
  const bytes = readFileSync(file);
  let text;
  try {
    text = new TextDecoder('utf-8', { fatal: true }).decode(bytes);
  } catch {
    throw new Error('manifest.json 不是合法 UTF-8');
  }
  return { bytes, text };
}

function collectAssets(manifest) {
  const paths = new Set();
  for (const clip of manifest.clips ?? []) {
    if (clip?.type === 'sequence') for (const frame of clip.frames ?? []) paths.add(frame.path);
    if (clip?.type === 'gif') paths.add(clip.path);
  }
  return [...paths];
}

function resolveAsset(packageDir, relative) {
  const segments = String(relative).split('/');
  const resolved = path.resolve(packageDir, ...segments);
  const root = path.resolve(packageDir);
  if (resolved !== root && !resolved.startsWith(`${root}${path.sep}`)) {
    throw new Error(`资产路径逃逸包根：${relative}`);
  }
  return resolved;
}

function decodeAssetFile(file) {
  const bytes = readFileSync(file);
  const head = bytes.subarray(0, 8);
  if (head[0] === 0x89 && head.subarray(1, 4).toString('ascii') === 'PNG') return { kind: 'png', ...decodePng(bytes) };
  if (bytes.subarray(0, 3).toString('ascii') === 'GIF') return { kind: 'gif', ...decodeGif(bytes) };
  throw new Error('既不是 PNG 也不是 GIF');
}

function sha256(buffer) {
  return createHash('sha256').update(buffer).digest('hex');
}

function compareFrames(actual, expected, label) {
  const problems = [];
  if (actual.length !== expected.length) {
    problems.push(`${label} 帧数 ${actual.length} != 期望 ${expected.length}`);
    return problems;
  }
  expected.forEach((frame, index) => {
    const got = actual[index];
    if (got === undefined) {
      problems.push(`${label}[${index}] 缺失`);
      return;
    }
    if (frame.durationMs !== got.durationMs) {
      problems.push(`${label}[${index}] 时长 ${got.durationMs} != 期望 ${frame.durationMs}`);
    }
    const digest = sha256(got.rgba);
    if (digest !== frame.rgbaSha256) {
      problems.push(`${label}[${index}] RGBA sha256 不匹配`);
      return;
    }
    if (frame.rgbaBase64 !== undefined && got.rgba.toString('base64') !== frame.rgbaBase64) {
      problems.push(`${label}[${index}] RGBA 内容不匹配`);
    }
    const histogram = { opaque: 0, transparent: 0, semi: 0 };
    for (let offset = 3; offset < got.rgba.length; offset += 4) {
      const alpha = got.rgba[offset];
      if (alpha === 255) histogram.opaque += 1;
      else if (alpha === 0) histogram.transparent += 1;
      else histogram.semi += 1;
    }
    if (
      histogram.opaque !== frame.opaquePixels ||
      histogram.transparent !== frame.transparentPixels ||
      histogram.semi !== frame.semiTransparentPixels
    ) {
      problems.push(
        `${label}[${index}] alpha 直方图 ${JSON.stringify(histogram)} != 期望 ` +
          `${frame.opaquePixels}/${frame.transparentPixels}/${frame.semiTransparentPixels}`,
      );
    }
  });
  return problems;
}

function verifyAcceptFixture(indexEntry, outDir) {
  const packageDir = path.join(outDir, indexEntry.packageDir);
  const problems = [];
  const { bytes, text } = readManifest(packageDir);
  if (bytes.length > MANIFEST_MAX_BYTES) problems.push(`manifest ${bytes.length} 字节超过 64KiB`);
  const duplicates = scanDuplicateKeys(text);
  if (duplicates.length > 0) problems.push(`manifest 存在重复字段 ${duplicates.join(',')}`);

  let manifest;
  try {
    manifest = JSON.parse(text);
  } catch {
    return { problems: ['manifest 不是合法 JSON'] };
  }
  problems.push(...validateManifest(manifest));
  if (problems.length > 0) return { problems };

  const expectedFile = path.join(outDir, indexEntry.expectedFile);
  if (!existsSync(expectedFile)) return { problems: [`期望文件缺失 ${indexEntry.expectedFile}`] };
  const expected = JSON.parse(readFileSync(expectedFile, 'utf8'));

  const clipReports = [];
  for (const clip of manifest.clips) {
    const clipExpected = expected.clips?.[clip.id];
    if (clipExpected === undefined) {
      problems.push(`期望缺少 Clip ${clip.id}`);
      continue;
    }
    let frames = [];
    let canvas = null;
    if (clip.type === 'sequence') {
      let totalEncoded = 0;
      let totalPixelBytes = 0;
      for (const frame of clip.frames) {
        const file = resolveAsset(packageDir, frame.path);
        if (!existsSync(file)) {
          problems.push(`资产缺失 ${frame.path}`);
          continue;
        }
        const size = statSync(file).size;
        if (size > MAX_ENCODED_FILE_BYTES) problems.push(`资产 ${frame.path} 超过 8MiB`);
        totalEncoded += size;
        const decoded = decodeAssetFile(file);
        if (decoded.kind !== 'png') problems.push(`${frame.path} 不是 PNG`);
        if (decoded.width > MAX_FRAME_DIMENSION || decoded.height > MAX_FRAME_DIMENSION) {
          problems.push(`${frame.path} 尺寸 ${decoded.width}x${decoded.height} 超过 512`);
        }
        totalPixelBytes += decoded.width * decoded.height * 4;
        if (canvas === null) canvas = { width: decoded.width, height: decoded.height };
        else if (canvas.width !== decoded.width || canvas.height !== decoded.height) {
          problems.push(`${frame.path} 尺寸与同 Clip 首帧不一致`);
        }
        frames.push({ rgba: decoded.rgba, durationMs: frame.durationMs });
      }
      if (totalEncoded > MAX_ENCODED_TOTAL_BYTES) problems.push('读取编码数据超过 16MiB');
      if (totalPixelBytes > MAX_DECODED_PIXEL_BYTES) problems.push('解码像素超过 32MiB');
    } else {
      const file = resolveAsset(packageDir, clip.path);
      if (!existsSync(file)) {
        problems.push(`资产缺失 ${clip.path}`);
        continue;
      }
      if (statSync(file).size > MAX_ENCODED_FILE_BYTES) problems.push(`资产 ${clip.path} 超过 8MiB`);
      const decoded = decodeAssetFile(file);
      if (decoded.kind !== 'gif') problems.push(`${clip.path} 不是 GIF`);
      if (decoded.width > MAX_FRAME_DIMENSION || decoded.height > MAX_FRAME_DIMENSION) {
        problems.push(`${clip.path} 画布 ${decoded.width}x${decoded.height} 超过 512`);
      }
      canvas = { width: decoded.width, height: decoded.height };
      frames = decoded.frames.map((frame) => ({ rgba: frame.rgba, durationMs: frame.delayMs }));
      if (clipExpected.loopCount !== undefined && decoded.loopCount !== clipExpected.loopCount) {
        problems.push(`${clip.id} 循环提示 ${decoded.loopCount} != 期望 ${clipExpected.loopCount}`);
      }
      const totalPixelBytes = frames.reduce((sum, frame) => sum + frame.rgba.length, 0);
      if (totalPixelBytes > MAX_DECODED_PIXEL_BYTES) problems.push('解码像素超过 32MiB');
    }

    if (canvas === null) {
      problems.push(`Clip ${clip.id} 没有任何帧`);
      continue;
    }
    if (clipExpected.canvas !== undefined) {
      if (clipExpected.canvas.width !== canvas.width || clipExpected.canvas.height !== canvas.height) {
        problems.push(`${clip.id} 画布 ${canvas.width}x${canvas.height} != 期望`);
      }
    } else if (expected.canvas !== undefined) {
      // 序列帧画布与期望首帧尺寸比对已由像素哈希覆盖。
    }
    problems.push(...compareFrames(frames, clipExpected.frames, `clips.${clip.id}.frames`));
    if (clipExpected.durationsMs !== undefined) {
      const durations = frames.map((frame) => frame.durationMs);
      if (JSON.stringify(durations) !== JSON.stringify(clipExpected.durationsMs)) {
        problems.push(`clips.${clip.id}.durationsMs 不匹配`);
      }
    }
    clipReports.push({ clip: clip.id, frames: frames.length, totalDurationMs: frames.reduce((sum, frame) => sum + frame.durationMs, 0) });
  }
  return { problems, clipReports, manifest };
}

// ---------------------------------------------------------------------------
// 拒绝夹具：证明缺陷确实存在
// ---------------------------------------------------------------------------

function findGceDelays(bytes) {
  const delays = [];
  for (let i = 0; i + 7 < bytes.length; i += 1) {
    if (bytes[i] === 0x21 && bytes[i + 1] === 0xf9 && bytes[i + 2] === 0x04) {
      delays.push(bytes.readUInt16LE(i + 4));
    }
  }
  return delays;
}

function encodedFileSizes(packageDir, manifest) {
  return collectAssets(manifest).map((relative) => {
    const file = resolveAsset(packageDir, relative);
    return existsSync(file) ? statSync(file).size : -1;
  });
}

function decodedPixelBytes(packageDir, manifest) {
  let total = 0;
  for (const clip of manifest.clips ?? []) {
    if (clip.type === 'sequence') {
      for (const frame of clip.frames ?? []) {
        const file = resolveAsset(packageDir, frame.path);
        if (!existsSync(file)) continue;
        try {
          const decoded = decodeAssetFile(file);
          total += decoded.width * decoded.height * 4;
        } catch {
          total += 0;
        }
      }
    } else {
      const file = resolveAsset(packageDir, clip.path);
      if (!existsSync(file)) continue;
      try {
        const decoded = decodeAssetFile(file);
        total += decoded.frames.reduce((sum, frame) => sum + frame.rgba.length, 0);
      } catch {
        total += 0;
      }
    }
  }
  return total;
}

function firstAssetBytes(packageDir, manifest) {
  const assets = collectAssets(manifest);
  for (const relative of assets) {
    const file = resolveAsset(packageDir, relative);
    if (existsSync(file)) return { relative, bytes: readFileSync(file) };
  }
  return null;
}

const DEFECT_CHECKS = {
  manifest_unknown_field: ({ manifest }) => {
    const extra = unknownKeys(manifest, ROOT_KEYS) ?? [];
    return { ok: extra.length > 0, detail: `根未知字段 ${extra.join(',') || '（无）'}` };
  },
  manifest_duplicate_field: ({ text }) => {
    const duplicates = scanDuplicateKeys(text);
    return { ok: duplicates.length > 0, detail: `重复字段 ${duplicates.join(',') || '（无）'}` };
  },
  manifest_version_unsupported: ({ manifest }) => ({
    ok: manifest.formatVersion !== 1,
    detail: `formatVersion=${manifest.formatVersion}`,
  }),
  manifest_missing_field: ({ manifest }) => ({
    ok: manifest.defaultAction === undefined,
    detail: `defaultAction=${manifest.defaultAction}`,
  }),
  manifest_too_large: ({ bytes }) => ({
    ok: bytes.length > MANIFEST_MAX_BYTES,
    detail: `${bytes.length} 字节`,
  }),
  id_charset: ({ manifest }) => {
    const ids = (manifest.clips ?? []).map((clip) => clip.id).filter((id) => !ID_PATTERN.test(String(id)));
    return { ok: ids.length > 0, detail: `非法 ID ${JSON.stringify(ids)}` };
  },
  id_length: ({ manifest }) => {
    const ids = (manifest.clips ?? []).map((clip) => clip.id).filter((id) => String(id).length > 64);
    return { ok: ids.length > 0, detail: `超长 ID 长度 ${ids.map((id) => id.length).join(',')}` };
  },
  id_duplicate: ({ manifest }) => {
    const ids = (manifest.clips ?? []).map((clip) => clip.id);
    return { ok: new Set(ids).size !== ids.length, detail: `ID ${ids.join(',')}` };
  },
  action_clip_missing: ({ manifest }) => {
    const clipIds = new Set((manifest.clips ?? []).map((clip) => clip.id));
    const missing = (manifest.actions ?? []).map((action) => action.clip).filter((id) => !clipIds.has(id));
    return { ok: missing.length > 0, detail: `缺失 Clip ${missing.join(',')}` };
  },
  default_action_missing: ({ manifest }) => {
    const actionIds = new Set((manifest.actions ?? []).map((action) => action.id));
    return { ok: !actionIds.has(manifest.defaultAction), detail: `defaultAction=${manifest.defaultAction}` };
  },
  path_escape: ({ manifest }) => pathCheck(manifest, (issue) => issue === 'escape'),
  path_absolute: ({ manifest }) => pathCheck(manifest, (issue) => issue === 'absolute' || issue === 'drive'),
  path_backslash: ({ manifest }) => pathCheck(manifest, (issue) => issue === 'backslash'),
  path_unc: ({ manifest }) => pathCheck(manifest, (issue) => issue === 'absolute' || issue === 'empty_component'),
  path_dot_component: ({ manifest }) => pathCheck(manifest, (issue) => issue === 'dot_component'),
  path_empty_component: ({ manifest }) => pathCheck(manifest, (issue) => issue === 'empty_component'),
  path_device: ({ manifest }) => pathCheck(manifest, (issue) => issue === 'device'),
  path_colon: ({ manifest }) => pathCheck(manifest, (issue) => issue === 'colon'),
  duration_zero: ({ manifest }) => durationCheck(manifest, (value) => value === 0),
  duration_over_max: ({ manifest }) => durationCheck(manifest, (value) => value > MAX_FRAME_DURATION_MS),
  duration_clip_total: ({ manifest }) => {
    const over = (manifest.clips ?? []).some(
      (clip) =>
        clip.type === 'sequence' &&
        (clip.frames ?? []).reduce((sum, frame) => sum + (frame.durationMs ?? 0), 0) > MAX_CLIP_DURATION_MS,
    );
    return { ok: over, detail: '单 Clip 总时长超限' };
  },
  frames_per_clip: ({ manifest }) => ({
    ok: (manifest.clips ?? []).some((clip) => (clip.frames ?? []).length > MAX_FRAMES_PER_CLIP),
    detail: (manifest.clips ?? []).map((clip) => (clip.frames ?? []).length).join(','),
  }),
  frames_total: ({ manifest }) => {
    const total = (manifest.clips ?? []).reduce((sum, clip) => sum + (clip.frames ?? []).length, 0);
    return { ok: total > MAX_FRAMES_TOTAL, detail: `总帧数 ${total}` };
  },
  frame_dimension: ({ packageDir, manifest }) => {
    const asset = firstAssetBytes(packageDir, manifest);
    if (asset === null) return { ok: false, detail: '没有资产' };
    const header = decodePng(asset.bytes);
    return {
      ok: header.width > MAX_FRAME_DIMENSION || header.height > MAX_FRAME_DIMENSION,
      detail: `${header.width}x${header.height}`,
    };
  },
  asset_missing: ({ packageDir, manifest }) => {
    const missing = collectAssets(manifest).filter((relative) => !existsSync(resolveAsset(packageDir, relative)));
    return { ok: missing.length > 0, detail: `缺失 ${missing.join(',')}` };
  },
  png_signature: ({ packageDir, manifest }) => {
    const asset = firstAssetBytes(packageDir, manifest);
    return { ok: asset !== null && asset.bytes[0] !== 0x89, detail: asset === null ? '没有资产' : `首字节 0x${asset.bytes[0].toString(16)}` };
  },
  png_apng: ({ packageDir, manifest }) => {
    const asset = firstAssetBytes(packageDir, manifest);
    const hasActl = asset !== null && asset.bytes.includes(Buffer.from('acTL', 'ascii'));
    return { ok: hasActl, detail: hasActl ? '含 acTL' : '无 acTL' };
  },
  png_interlaced_unsupported: ({ packageDir, manifest }) => {
    const asset = firstAssetBytes(packageDir, manifest);
    if (asset === null) return { ok: false, detail: '没有资产' };
    const ihdr = readChunks(asset.bytes).find((chunk) => chunk.type === 'IHDR');
    if (ihdr === undefined) return { ok: false, detail: '缺 IHDR' };
    const interlace = ihdr.data[12];
    return { ok: interlace === 1, detail: `IHDR interlace=${interlace}（首版只接受 0）` };
  },
  png_crc: ({ packageDir, manifest }) => {
    const asset = firstAssetBytes(packageDir, manifest);
    if (asset === null) return { ok: false, detail: '没有资产' };
    try {
      decodePng(asset.bytes);
      return { ok: false, detail: '解码未发现 CRC 错误' };
    } catch (error) {
      return { ok: /CRC/.test(String(error)), detail: String(error) };
    }
  },
  png_deflate: ({ packageDir, manifest }) => {
    const asset = firstAssetBytes(packageDir, manifest);
    if (asset === null) return { ok: false, detail: '没有资产' };
    try {
      const chunks = readChunks(asset.bytes);
      const idat = Buffer.concat(chunks.filter((chunk) => chunk.type === 'IDAT').map((chunk) => chunk.data));
      inflateSync(idat);
      return { ok: false, detail: 'zlib 流仍可解压' };
    } catch (error) {
      return { ok: true, detail: String(error) };
    }
  },
  png_truncated: ({ packageDir, manifest }) => {
    const asset = firstAssetBytes(packageDir, manifest);
    if (asset === null) return { ok: false, detail: '没有资产' };
    try {
      decodePng(asset.bytes);
      return { ok: false, detail: '截断样本仍可解码' };
    } catch (error) {
      return { ok: true, detail: String(error) };
    }
  },
  gif_zero_delay: ({ packageDir, manifest }) => {
    const asset = firstAssetBytes(packageDir, manifest);
    if (asset === null) return { ok: false, detail: '没有资产' };
    const delays = findGceDelays(asset.bytes);
    return { ok: delays.includes(0), detail: `GCE 延时 ${delays.join(',')}` };
  },
  gif_truncated: ({ packageDir, manifest }) => {
    const asset = firstAssetBytes(packageDir, manifest);
    if (asset === null) return { ok: false, detail: '没有资产' };
    try {
      decodeGif(asset.bytes);
      return { ok: false, detail: '截断样本仍可解码' };
    } catch (error) {
      return { ok: true, detail: String(error) };
    }
  },
  gif_trailer: ({ packageDir, manifest }) => {
    const asset = firstAssetBytes(packageDir, manifest);
    if (asset === null) return { ok: false, detail: '没有资产' };
    return { ok: asset.bytes[asset.bytes.length - 1] !== 0x3b, detail: `末字节 0x${asset.bytes[asset.bytes.length - 1].toString(16)}` };
  },
  gif_geometry: ({ packageDir, manifest }) => {
    const asset = firstAssetBytes(packageDir, manifest);
    if (asset === null) return { ok: false, detail: '没有资产' };
    const bytes = asset.bytes;
    const canvasWidth = bytes.readUInt16LE(6);
    const canvasHeight = bytes.readUInt16LE(8);
    let offset = 13;
    const packed = bytes[10];
    if (packed & 0x80) offset += (1 << ((packed & 0x07) + 1)) * 3;
    while (offset < bytes.length) {
      if (bytes[offset] === 0x21) {
        const label = bytes[offset + 1];
        offset += 2;
        if (label === 0xf9) offset += 6;
        else {
          const length = bytes[offset];
          offset += 1 + length;
          const blocks = readSubBlocks(bytes, offset);
          offset = blocks.offset;
        }
        continue;
      }
      if (bytes[offset] === 0x2c) {
        const left = bytes.readUInt16LE(offset + 1);
        const top = bytes.readUInt16LE(offset + 3);
        const width = bytes.readUInt16LE(offset + 5);
        const height = bytes.readUInt16LE(offset + 7);
        const over =
          width > MAX_FRAME_DIMENSION ||
          height > MAX_FRAME_DIMENSION ||
          left + width > canvasWidth ||
          top + height > canvasHeight;
        return { ok: over, detail: `帧 ${left},${top} ${width}x${height} / 画布 ${canvasWidth}x${canvasHeight}` };
      }
      break;
    }
    return { ok: false, detail: '未找到图像描述符' };
  },
  encoded_file_over_8mib: ({ packageDir, manifest }) => {
    const sizes = encodedFileSizes(packageDir, manifest);
    return { ok: sizes.some((size) => size > MAX_ENCODED_FILE_BYTES), detail: `文件字节 ${sizes.join(',')}` };
  },
  encoded_total_over_16mib: ({ packageDir, manifest }) => {
    const total = encodedFileSizes(packageDir, manifest).reduce((sum, size) => sum + Math.max(size, 0), 0);
    return { ok: total > MAX_ENCODED_TOTAL_BYTES, detail: `累计 ${total} 字节` };
  },
  decoded_pixels_over_32mib: ({ packageDir, manifest }) => {
    const total = decodedPixelBytes(packageDir, manifest);
    return { ok: total > MAX_DECODED_PIXEL_BYTES, detail: `解码累计 ${total} 字节` };
  },
};

function pathCheck(manifest, predicate) {
  const paths = [];
  for (const clip of manifest.clips ?? []) {
    if (clip.type === 'sequence') for (const frame of clip.frames ?? []) paths.push(frame.path);
    else paths.push(clip.path);
  }
  const matched = paths.filter((value) => predicate(pathProblem(value)));
  return { ok: matched.length > 0, detail: `命中 ${JSON.stringify(matched)}` };
}

function durationCheck(manifest, predicate) {
  const values = [];
  for (const clip of manifest.clips ?? []) {
    for (const frame of clip.frames ?? []) values.push(frame.durationMs);
  }
  const matched = values.filter((value) => typeof value === 'number' && predicate(value));
  return { ok: matched.length > 0, detail: `命中 ${JSON.stringify(matched)}` };
}

function verifyRejectFixture(indexEntry, outDir) {
  const packageDir = path.join(outDir, indexEntry.packageDir);
  const { bytes, text } = readManifest(packageDir);
  let manifest = null;
  try {
    manifest = JSON.parse(text);
  } catch {
    manifest = null;
  }
  const check = DEFECT_CHECKS[indexEntry.rejectCode];
  if (check === undefined) {
    return { problems: [`未知的 rejectCode ${indexEntry.rejectCode}`] };
  }
  const result = check({ manifest, text, bytes, packageDir });
  return {
    problems: result.ok ? [] : [`缺陷未被证实在字节中（${indexEntry.rejectCode}）：${result.detail}`],
    detail: result.detail,
  };
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

function parseArgs(argv) {
  const options = { outDir: null, quiet: false, only: null };
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    if (arg === '--out') options.outDir = argv[++i] ?? null;
    else if (arg === '--quiet') options.quiet = true;
    else if (arg === '--only') options.only = (argv[++i] ?? '').split(',').filter(Boolean);
    else if (arg === '--help' || arg === '-h') options.help = true;
    else throw new Error(`未知参数 ${arg}`);
  }
  return options;
}

function defaultOutDir() {
  let dir = HERE;
  for (;;) {
    if (existsSync(path.join(dir, 'Cargo.toml')) && existsSync(path.join(dir, 'AGENTS.md'))) {
      return path.join(dir, 'target', 'c-material-fixtures');
    }
    const parent = path.dirname(dir);
    if (parent === dir) return path.join(HERE, '..', '..', '..', 'target', 'c-material-fixtures');
    dir = parent;
  }
}

function main() {
  const options = parseArgs(process.argv.slice(2));
  if (options.help) {
    process.stdout.write('用法：node tests/material-fixtures-verify.mjs [--out <目录>] [--only <名称,...>] [--quiet]\n');
    return;
  }
  const outDir = path.resolve(options.outDir ?? defaultOutDir());
  const indexPath = path.join(outDir, 'index.json');
  if (!existsSync(indexPath)) {
    console.error(`[material-fixtures-verify] 缺少 ${indexPath}，请先运行 material-fixtures.mjs`);
    process.exitCode = 1;
    return;
  }
  const index = JSON.parse(readFileSync(indexPath, 'utf8'));
  const entries = index.fixtures.filter((entry) => options.only === null || options.only.includes(entry.name));
  let failures = 0;
  const rows = [];
  for (const entry of entries) {
    const label = `${entry.expect === 'accept' ? 'accept' : 'reject'}:${entry.rejectCode ?? '-'}`;
    let outcome;
    try {
      outcome = entry.expect === 'accept' ? verifyAcceptFixture(entry, outDir) : verifyRejectFixture(entry, outDir);
    } catch (error) {
      outcome = { problems: [error instanceof Error ? error.message : String(error)] };
    }
    const ok = outcome.problems.length === 0;
    if (!ok) failures += 1;
    rows.push({ entry, label, ok, outcome });
    if (!options.quiet) {
      const note =
        entry.expect === 'accept'
          ? outcome.clipReports?.map((clip) => `${clip.clip}:${clip.frames}帧/${clip.totalDurationMs}ms`).join(' ') ?? ''
          : outcome.detail ?? '';
      process.stdout.write(`${ok ? 'ok    ' : 'FAIL  '} ${entry.name.padEnd(36)} ${label.padEnd(30)} ${note}\n`);
      for (const problem of outcome.problems) process.stdout.write(`         - ${problem}\n`);
    }
  }
  const accepts = entries.filter((entry) => entry.expect === 'accept').length;
  process.stdout.write(
    `[material-fixtures-verify] ${entries.length - failures}/${entries.length} 通过` +
      `（接受夹具 ${accepts}：像素/时长/循环提示逐项比对；拒绝夹具 ${entries.length - accepts}：缺陷存在性自证）\n`,
  );
  if (failures > 0) process.exitCode = 1;
}

if (process.argv[1] !== undefined && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    main();
  } catch (error) {
    console.error(`[material-fixtures-verify] ${error instanceof Error ? error.message : String(error)}`);
    process.exitCode = 1;
  }
}

export { scanDuplicateKeys, validateManifest, pathProblem };
