use std::error::Error;
use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};

use super::manifest::{MAX_FRAME_DIMENSION, MAX_FRAME_DURATION_MS, MAX_FRAMES_PER_CLIP};

#[derive(Debug)]
pub(super) struct DecodedFrame {
    pub width: u32,
    pub height: u32,
    pub duration_ms: u64,
    pub rgba: Vec<u8>,
}

#[derive(Debug)]
pub(super) struct DecodeError(pub(super) &'static str);
impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl Error for DecodeError {}
type DResult<T> = Result<T, DecodeError>;
fn fail<T>(message: &'static str) -> DResult<T> {
    Err(DecodeError(message))
}

#[cfg(test)]
pub(super) fn decode_png(data: &[u8], max_pixels: usize) -> DResult<DecodedFrame> {
    decode_png_cancellable(data, max_pixels, None)
}

pub(super) fn decode_png_cancellable(
    data: &[u8],
    max_pixels: usize,
    cancelled: Option<&AtomicBool>,
) -> DResult<DecodedFrame> {
    if data.len() < 57 || !data.starts_with(b"\x89PNG\r\n\x1a\n") {
        return fail("invalid or truncated PNG signature/header");
    }
    let mut offset = 8usize;
    let mut width = 0u32;
    let mut height = 0u32;
    let mut color = 0u8;
    let mut palette: Option<Vec<u8>> = None;
    let mut transparency: Option<Vec<u8>> = None;
    let mut compressed = Vec::new();
    let mut saw_header = false;
    let mut saw_end = false;
    let mut saw_data = false;
    let mut ended_data = false;
    while offset < data.len() {
        if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return fail("cancelled");
        }
        if data.len() - offset < 12 {
            return fail("truncated PNG chunk");
        }
        let length = u32::from_be_bytes(data[offset..offset + 4].try_into().unwrap()) as usize;
        let end = offset
            .checked_add(12)
            .and_then(|v| v.checked_add(length))
            .ok_or(DecodeError("PNG chunk length overflow"))?;
        if end > data.len() {
            return fail("PNG chunk exceeds file bounds");
        }
        let kind = &data[offset + 4..offset + 8];
        let payload = &data[offset + 8..offset + 8 + length];
        let expected_crc = u32::from_be_bytes(data[offset + 8 + length..end].try_into().unwrap());
        if crc32(&data[offset + 4..offset + 8 + length]) != expected_crc {
            return fail("PNG chunk CRC mismatch");
        }
        match kind {
            b"IHDR" => {
                if saw_header || offset != 8 || length != 13 {
                    return fail("invalid PNG IHDR placement or length");
                }
                width = u32::from_be_bytes(payload[0..4].try_into().unwrap());
                height = u32::from_be_bytes(payload[4..8].try_into().unwrap());
                let bit_depth = payload[8];
                color = payload[9];
                if width == 0
                    || height == 0
                    || width > MAX_FRAME_DIMENSION
                    || height > MAX_FRAME_DIMENSION
                {
                    return fail("PNG dimensions exceed limits");
                }
                let pixels = (width as usize)
                    .checked_mul(height as usize)
                    .and_then(|n| n.checked_mul(4))
                    .ok_or(DecodeError("PNG pixel byte count overflow"))?;
                if pixels > max_pixels {
                    return fail("PNG decoded pixels exceed package budget");
                }
                if bit_depth != 8 || payload[10] != 0 || payload[11] != 0 || payload[12] != 0 {
                    return fail("unsupported PNG bit depth, compression, filter or interlace");
                }
                if !matches!(color, 0 | 2 | 3 | 4 | 6) {
                    return fail("unsupported PNG color type");
                }
                saw_header = true;
            }
            b"PLTE" => {
                if !saw_header
                    || saw_data
                    || palette.is_some()
                    || matches!(color, 0 | 4)
                    || length == 0
                    || !length.is_multiple_of(3)
                    || length > 768
                {
                    return fail("invalid PNG palette");
                }
                palette = Some(payload.to_vec());
            }
            b"tRNS" => {
                if !saw_header || saw_data || transparency.is_some() || matches!(color, 4 | 6) {
                    return fail("misplaced PNG transparency chunk");
                }
                let valid = match color {
                    0 => length == 2 && payload[0] == 0,
                    2 => length == 6 && payload[0] == 0 && payload[2] == 0 && payload[4] == 0,
                    3 => palette
                        .as_ref()
                        .is_some_and(|table| length <= table.len() / 3),
                    _ => false,
                };
                if !valid {
                    return fail("invalid PNG transparency chunk");
                }
                transparency = Some(payload.to_vec());
            }
            b"IDAT" => {
                if !saw_header || saw_end || ended_data || (color == 3 && palette.is_none()) {
                    return fail("misplaced PNG image data");
                }
                saw_data = true;
                if compressed
                    .len()
                    .checked_add(length)
                    .is_none_or(|size| size > 2 * 1024 * 1024)
                {
                    return fail("PNG compressed stream exceeds file budget");
                }
                compressed.extend_from_slice(payload);
            }
            b"IEND" => {
                if length != 0 || !saw_header || !saw_data {
                    return fail("invalid PNG IEND");
                }
                saw_end = true;
                offset = end;
                break;
            }
            b"acTL" => return fail("APNG is unsupported"),
            _ => {
                if saw_data {
                    ended_data = true;
                }
                if kind[0] & 0x20 == 0 {
                    return fail("unknown critical PNG chunk");
                }
            }
        }
        if kind != b"IDAT" && saw_data && kind != b"IEND" {
            ended_data = true;
        }
        offset = end;
    }
    if !saw_end || offset != data.len() {
        return fail("PNG missing IEND or has trailing bytes");
    }
    let channels = match color {
        0 | 3 => 1usize,
        2 => 3,
        4 => 2,
        6 => 4,
        _ => unreachable!(),
    };
    let pixel_count = (width as usize)
        .checked_mul(height as usize)
        .ok_or(DecodeError("PNG pixel count overflow"))?;
    let row_bytes = (width as usize)
        .checked_mul(channels)
        .ok_or(DecodeError("PNG row size overflow"))?;
    let expected = row_bytes
        .checked_add(1)
        .and_then(|v| v.checked_mul(height as usize))
        .ok_or(DecodeError("PNG output size overflow"))?;
    if expected > 2 * 1024 * 1024 || pixel_count > max_pixels / 4 {
        return fail("PNG decode scratch budget exceeded");
    }
    let pixel_bytes = row_bytes
        .checked_mul(height as usize)
        .ok_or(DecodeError("PNG pixel buffer size overflow"))?;
    let rgba_bytes = pixel_count
        .checked_mul(4)
        .ok_or(DecodeError("PNG RGBA buffer size overflow"))?;
    if compressed
        .len()
        .checked_add(expected)
        .and_then(|size| size.checked_add(pixel_bytes))
        .and_then(|size| size.checked_add(rgba_bytes))
        .is_none_or(|size| size > 8 * 1024 * 1024)
    {
        return fail("PNG decode scratch budget exceeded");
    }
    let raw = inflate_zlib_cancellable(&compressed, expected, cancelled)?;
    let mut pixels = vec![0u8; row_bytes * height as usize];
    for y in 0..height as usize {
        if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return fail("cancelled");
        }
        let src = y * (row_bytes + 1);
        let filter = raw[src];
        for x in 0..row_bytes {
            let input = raw[src + 1 + x];
            let a = if x >= channels {
                pixels[y * row_bytes + x - channels]
            } else {
                0
            };
            let b = if y > 0 {
                pixels[(y - 1) * row_bytes + x]
            } else {
                0
            };
            let c = if y > 0 && x >= channels {
                pixels[(y - 1) * row_bytes + x - channels]
            } else {
                0
            };
            pixels[y * row_bytes + x] = match filter {
                0 => input,
                1 => input.wrapping_add(a),
                2 => input.wrapping_add(b),
                3 => input.wrapping_add(((a as u16 + b as u16) / 2) as u8),
                4 => input.wrapping_add(paeth(a, b, c)),
                _ => return fail("invalid PNG scanline filter"),
            };
        }
    }
    let mut rgba = Vec::with_capacity(pixel_count * 4);
    match color {
        0 => {
            if transparency
                .as_ref()
                .is_some_and(|t| t.len() != 2 || t[0] != 0)
            {
                return fail("invalid grayscale tRNS chunk");
            }
            for &gray in &pixels {
                rgba.extend_from_slice(&[
                    gray,
                    gray,
                    gray,
                    if transparency.as_ref().is_some_and(|t| t[1] == gray) {
                        0
                    } else {
                        255
                    },
                ]);
            }
        }
        2 => {
            if transparency
                .as_ref()
                .is_some_and(|t| t.len() != 6 || t[0] != 0 || t[2] != 0 || t[4] != 0)
            {
                return fail("invalid truecolor tRNS chunk");
            }
            for p in pixels.as_chunks::<3>().0 {
                let alpha = if transparency
                    .as_ref()
                    .is_some_and(|t| t[1] == p[0] && t[3] == p[1] && t[5] == p[2])
                {
                    0
                } else {
                    255
                };
                rgba.extend_from_slice(&[p[0], p[1], p[2], alpha]);
            }
        }
        3 => {
            let palette = palette.ok_or(DecodeError("indexed PNG has no palette"))?;
            if transparency
                .as_ref()
                .is_some_and(|t| t.len() > palette.len() / 3)
            {
                return fail("indexed PNG transparency exceeds palette");
            }
            for &index in &pixels {
                let base = index as usize * 3;
                if base + 2 >= palette.len() {
                    return fail("PNG palette index is out of range");
                }
                rgba.extend_from_slice(&[
                    palette[base],
                    palette[base + 1],
                    palette[base + 2],
                    transparency
                        .as_ref()
                        .and_then(|t| t.get(index as usize))
                        .copied()
                        .unwrap_or(255),
                ]);
            }
        }
        4 => {
            for p in pixels.as_chunks::<2>().0 {
                rgba.extend_from_slice(&[p[0], p[0], p[0], p[1]]);
            }
        }
        6 => {
            for p in pixels.as_chunks::<4>().0 {
                rgba.extend_from_slice(p);
            }
        }
        _ => unreachable!(),
    }
    Ok(DecodedFrame {
        width,
        height,
        duration_ms: 0,
        rgba,
    })
}

#[cfg(test)]
pub(super) fn decode_gif(
    data: &[u8],
    max_frames: usize,
    max_pixels: usize,
    max_duration_ms: u64,
) -> DResult<Vec<DecodedFrame>> {
    decode_gif_cancellable(data, max_frames, max_pixels, max_duration_ms, None)
}

pub(super) fn decode_gif_cancellable(
    data: &[u8],
    max_frames: usize,
    max_pixels: usize,
    max_duration_ms: u64,
    cancelled: Option<&AtomicBool>,
) -> DResult<Vec<DecodedFrame>> {
    if max_frames == 0 {
        return fail("GIF frame count exceeds package budget");
    }
    let mut reader = Reader { data, pos: 0 };
    let signature = reader.take(6)?;
    if signature != b"GIF87a" && signature != b"GIF89a" {
        return fail("invalid GIF signature");
    }
    let width = reader.u16()? as u32;
    let height = reader.u16()? as u32;
    if width == 0 || height == 0 || width > MAX_FRAME_DIMENSION || height > MAX_FRAME_DIMENSION {
        return fail("GIF canvas dimensions exceed limits");
    }
    let canvas_len = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4))
        .ok_or(DecodeError("GIF canvas size overflow"))?;
    if canvas_len > max_pixels || canvas_len > 1024 * 1024 {
        return fail("GIF canvas exceeds pixel budget");
    }
    let packed = reader.byte()?;
    let _background_index = reader.byte()?;
    let _aspect = reader.byte()?;
    let global_palette = if packed & 0x80 != 0 {
        Some(read_palette(
            &mut reader,
            1usize << ((packed & 7) as usize + 1),
        )?)
    } else {
        None
    };
    let mut canvas = vec![0u8; canvas_len];
    let mut frames = Vec::new();
    let mut gce = GraphicControl {
        delay: 0,
        disposal: 0,
        transparent: None,
    };
    let mut previous: Option<(Vec<u8>, Rect, u8)> = None;
    let mut total_duration_ms = 0u64;
    loop {
        if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return fail("cancelled");
        }
        match reader.byte()? {
            0x3b => break,
            0x21 => match reader.byte()? {
                0xf9 => {
                    if reader.byte()? != 4 {
                        return fail("invalid GIF graphic control extension");
                    }
                    let flags = reader.byte()?;
                    if flags & 0xe2 != 0 {
                        return fail("unsupported GIF control flags");
                    }
                    gce.disposal = (flags >> 2) & 7;
                    if gce.disposal > 3 {
                        return fail("unsupported GIF disposal method");
                    }
                    gce.delay = reader.u16()?;
                    let transparent = reader.byte()?;
                    if reader.byte()? != 0 {
                        return fail("invalid GIF graphic control terminator");
                    }
                    gce.transparent = if flags & 1 != 0 {
                        Some(transparent)
                    } else {
                        None
                    };
                }
                0xfe | 0xff => {
                    reader.skip_sub_blocks(cancelled)?;
                }
                0x01 => return fail("GIF plain-text frames are unsupported"),
                _ => return fail("unsupported GIF extension"),
            },
            0x2c => {
                if frames.len() >= max_frames.min(MAX_FRAMES_PER_CLIP) {
                    return fail("GIF frame count exceeds package budget");
                }
                if frames
                    .len()
                    .checked_add(1)
                    .and_then(|count| count.checked_mul(canvas_len))
                    .is_none_or(|bytes| bytes > max_pixels)
                {
                    return fail("GIF composited pixels exceed package budget");
                }
                let delay = (gce.delay as u64)
                    .checked_mul(10)
                    .ok_or(DecodeError("GIF delay overflow"))?;
                if delay == 0 || delay > MAX_FRAME_DURATION_MS {
                    return fail("GIF frame delay is outside the supported range");
                }
                total_duration_ms = total_duration_ms
                    .checked_add(delay)
                    .ok_or(DecodeError("GIF total duration overflow"))?;
                if total_duration_ms > max_duration_ms {
                    return fail("GIF clip duration exceeds package limit");
                }
                if let Some((saved, rect, disposal)) = previous.take() {
                    match disposal {
                        2 => fill_rect(&mut canvas, width as usize, rect, [0, 0, 0, 0]),
                        3 => canvas = saved,
                        _ => {}
                    }
                }
                let rect = Rect {
                    left: reader.u16()? as usize,
                    top: reader.u16()? as usize,
                    width: reader.u16()? as usize,
                    height: reader.u16()? as usize,
                };
                if rect.width == 0
                    || rect.height == 0
                    || rect
                        .left
                        .checked_add(rect.width)
                        .is_none_or(|v| v > width as usize)
                    || rect
                        .top
                        .checked_add(rect.height)
                        .is_none_or(|v| v > height as usize)
                {
                    return fail("GIF image rectangle exceeds canvas");
                }
                let desc = reader.byte()?;
                if desc & 0x38 != 0 {
                    return fail("unsupported GIF image descriptor flags");
                }
                let palette = if desc & 0x80 != 0 {
                    read_palette(&mut reader, 1usize << ((desc & 7) as usize + 1))?
                } else {
                    global_palette
                        .clone()
                        .ok_or(DecodeError("GIF frame has no color table"))?
                };
                let minimum = reader.byte()?;
                let compressed = reader.sub_blocks(2 * 1024 * 1024, cancelled)?;
                let count = rect
                    .width
                    .checked_mul(rect.height)
                    .ok_or(DecodeError("GIF frame size overflow"))?;
                let indices = gif_lzw(&compressed, minimum, count, cancelled)?;
                let saved = if gce.disposal == 3 {
                    Some(canvas.clone())
                } else {
                    None
                };
                let rows: Vec<usize> = if desc & 0x40 == 0 {
                    (0..rect.height).collect()
                } else {
                    let mut rows = Vec::with_capacity(rect.height);
                    for (start, step) in [(0, 8), (4, 8), (2, 4), (1, 2)] {
                        rows.extend((start..rect.height).step_by(step));
                    }
                    rows
                };
                for (source_y, row) in rows.into_iter().enumerate() {
                    if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                        return fail("cancelled");
                    }
                    for x in 0..rect.width {
                        let index = indices[source_y * rect.width + x];
                        if gce.transparent == Some(index) {
                            continue;
                        }
                        let palette_offset = index as usize * 3;
                        if palette_offset + 2 >= palette.len() {
                            return fail("GIF palette index is out of range");
                        }
                        let target = ((rect.top + row) * width as usize + rect.left + x) * 4;
                        canvas[target..target + 4].copy_from_slice(&[
                            palette[palette_offset],
                            palette[palette_offset + 1],
                            palette[palette_offset + 2],
                            255,
                        ]);
                    }
                }
                frames.push(DecodedFrame {
                    width,
                    height,
                    duration_ms: delay,
                    rgba: canvas.clone(),
                });
                previous = Some((saved.unwrap_or_default(), rect, gce.disposal));
                gce = GraphicControl {
                    delay: 0,
                    disposal: 0,
                    transparent: None,
                };
            }
            _ => return fail("invalid GIF block marker"),
        }
    }
    if frames.is_empty() {
        return fail("GIF contains no image frames");
    }
    if reader.pos != data.len() {
        return fail("GIF has trailing bytes");
    }
    Ok(frames)
}

#[derive(Clone, Copy)]
struct GraphicControl {
    delay: u16,
    disposal: u8,
    transparent: Option<u8>,
}
#[derive(Clone, Copy)]
struct Rect {
    left: usize,
    top: usize,
    width: usize,
    height: usize,
}

fn fill_rect(canvas: &mut [u8], canvas_width: usize, rect: Rect, color: [u8; 4]) {
    for y in rect.top..rect.top + rect.height {
        for x in rect.left..rect.left + rect.width {
            let p = (y * canvas_width + x) * 4;
            canvas[p..p + 4].copy_from_slice(&color);
        }
    }
}

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, len: usize) -> DResult<&'a [u8]> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or(DecodeError("image offset overflow"))?;
        if end > self.data.len() {
            return fail("truncated image data");
        }
        let value = &self.data[self.pos..end];
        self.pos = end;
        Ok(value)
    }
    fn byte(&mut self) -> DResult<u8> {
        Ok(self.take(1)?[0])
    }
    fn u16(&mut self) -> DResult<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
    fn sub_blocks(&mut self, maximum: usize, cancelled: Option<&AtomicBool>) -> DResult<Vec<u8>> {
        let mut out = Vec::new();
        loop {
            if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                return fail("cancelled");
            }
            let size = self.byte()? as usize;
            if size == 0 {
                break;
            }
            if out.len().checked_add(size).is_none_or(|n| n > maximum) {
                return fail("GIF image stream exceeds budget");
            }
            out.extend_from_slice(self.take(size)?);
        }
        Ok(out)
    }
    fn skip_sub_blocks(&mut self, cancelled: Option<&AtomicBool>) -> DResult<()> {
        loop {
            if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                return fail("cancelled");
            }
            let size = self.byte()? as usize;
            if size == 0 {
                return Ok(());
            }
            self.take(size)?;
        }
    }
}

fn read_palette(reader: &mut Reader<'_>, count: usize) -> DResult<Vec<u8>> {
    Ok(reader
        .take(
            count
                .checked_mul(3)
                .ok_or(DecodeError("palette size overflow"))?,
        )?
        .to_vec())
}

fn gif_lzw(
    data: &[u8],
    minimum: u8,
    expected: usize,
    cancelled: Option<&AtomicBool>,
) -> DResult<Vec<u8>> {
    if !(2..=8).contains(&minimum) {
        return fail("invalid GIF LZW code size");
    }
    let clear = 1u16 << minimum;
    let end = clear + 1;
    let mut table: Vec<Vec<u8>> = (0..clear).map(|value| vec![value as u8]).collect();
    table.resize(clear as usize + 2, Vec::new());
    let mut bits = BitReader { data, bit: 0 };
    let mut code_size = minimum + 1;
    let mut next = end + 1;
    let mut previous: Option<Vec<u8>> = None;
    let mut output = Vec::with_capacity(expected);
    loop {
        if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return fail("cancelled");
        }
        let code = bits.read(code_size)? as u16;
        if code == clear {
            table.truncate(clear as usize + 2);
            code_size = minimum + 1;
            next = end + 1;
            previous = None;
            continue;
        }
        if code == end {
            break;
        }
        let entry = if (code as usize) < table.len() && !table[code as usize].is_empty() {
            table[code as usize].clone()
        } else if code == next {
            let mut p = previous
                .clone()
                .ok_or(DecodeError("invalid GIF LZW first code"))?;
            p.push(p[0]);
            p
        } else {
            return fail("invalid GIF LZW code");
        };
        if output
            .len()
            .checked_add(entry.len())
            .is_none_or(|n| n > expected)
        {
            return fail("GIF LZW output exceeds frame dimensions");
        }
        output.extend_from_slice(&entry);
        if let Some(previous) = previous
            && next < 4096
        {
            let mut added = previous;
            added.push(entry[0]);
            if table.len() <= next as usize {
                table.resize(next as usize + 1, Vec::new());
            }
            table[next as usize] = added;
            next += 1;
            if next == 1u16 << code_size && code_size < 12 {
                code_size += 1;
            }
        }
        previous = Some(entry);
    }
    if output.len() != expected {
        return fail("GIF LZW output length does not match image rectangle");
    }
    Ok(output)
}

struct BitReader<'a> {
    data: &'a [u8],
    bit: usize,
}
impl BitReader<'_> {
    fn read(&mut self, count: u8) -> DResult<u32> {
        let end = self
            .bit
            .checked_add(count as usize)
            .ok_or(DecodeError("bit offset overflow"))?;
        if end > self.data.len() * 8 {
            return fail("truncated compressed image stream");
        }
        let mut value = 0;
        for shift in 0..count {
            value |= (((self.data[self.bit / 8] >> (self.bit % 8)) & 1) as u32) << shift;
            self.bit += 1;
        }
        Ok(value)
    }
}

#[cfg(test)]
pub(super) fn inflate_zlib(data: &[u8], expected: usize) -> DResult<Vec<u8>> {
    inflate_zlib_cancellable(data, expected, None)
}

fn inflate_zlib_cancellable(
    data: &[u8],
    expected: usize,
    cancelled: Option<&AtomicBool>,
) -> DResult<Vec<u8>> {
    if data.len() < 6 {
        return fail("truncated zlib stream");
    }
    let cmf = data[0];
    let flg = data[1];
    if cmf & 15 != 8
        || cmf >> 4 > 7
        || (u16::from(cmf) * 256 + u16::from(flg)) % 31 != 0
        || flg & 0x20 != 0
    {
        return fail("unsupported zlib header");
    }
    let compressed_end = data.len() - 4;
    let mut bits = DeflateBits {
        data: &data[2..compressed_end],
        bit: 0,
    };
    let mut output = Vec::with_capacity(expected);
    let mut final_block = false;
    while !final_block {
        if cancelled.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return fail("cancelled");
        }
        final_block = bits.read(1)? != 0;
        match bits.read(2)? {
            0 => {
                bits.align();
                let len = bits.read(16)? as u16;
                let nlen = bits.read(16)? as u16;
                if len != !nlen {
                    return fail("invalid uncompressed DEFLATE block length");
                }
                for _ in 0..len {
                    if output.len().is_multiple_of(1024)
                        && cancelled.is_some_and(|flag| flag.load(Ordering::Acquire))
                    {
                        return fail("cancelled");
                    }
                    push_bounded(&mut output, bits.read(8)? as u8, expected)?;
                }
            }
            kind @ (1 | 2) => {
                let (lit, dist) = if kind == 1 {
                    (Huffman::fixed_lit()?, Huffman::fixed_dist()?)
                } else {
                    dynamic_tables(&mut bits)?
                };
                loop {
                    if output.len().is_multiple_of(1024)
                        && cancelled.is_some_and(|flag| flag.load(Ordering::Acquire))
                    {
                        return fail("cancelled");
                    }
                    let symbol = lit.decode(&mut bits)? as usize;
                    match symbol {
                        0..=255 => push_bounded(&mut output, symbol as u8, expected)?,
                        256 => break,
                        257..=285 => {
                            const LB: [u16; 29] = [
                                3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43,
                                51, 59, 67, 83, 99, 115, 131, 163, 195, 227, 258,
                            ];
                            const LE: [u8; 29] = [
                                0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4,
                                4, 4, 5, 5, 5, 5, 0,
                            ];
                            const DB: [u16; 30] = [
                                1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257,
                                385, 513, 769, 1025, 1537, 2049, 3073, 4097, 6145, 8193, 12289,
                                16385, 24577,
                            ];
                            const DE: [u8; 30] = [
                                0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9,
                                10, 10, 11, 11, 12, 12, 13, 13,
                            ];
                            let index = symbol - 257;
                            let length = LB[index] as usize + bits.read(LE[index])? as usize;
                            let dsym = dist.decode(&mut bits)? as usize;
                            if dsym >= DB.len() {
                                return fail("invalid DEFLATE distance code");
                            }
                            let distance = DB[dsym] as usize + bits.read(DE[dsym])? as usize;
                            if distance == 0 || distance > output.len() {
                                return fail("DEFLATE distance points before output");
                            }
                            for _ in 0..length {
                                if output.len().is_multiple_of(1024)
                                    && cancelled.is_some_and(|flag| flag.load(Ordering::Acquire))
                                {
                                    return fail("cancelled");
                                }
                                let byte = output[output.len() - distance];
                                push_bounded(&mut output, byte, expected)?;
                            }
                        }
                        _ => return fail("reserved DEFLATE literal/length symbol"),
                    }
                }
            }
            _ => return fail("reserved DEFLATE block type"),
        }
    }
    if bits.bit.div_ceil(8) != bits.data.len() {
        return fail("zlib stream has trailing DEFLATE bytes");
    }
    if output.len() != expected {
        return fail("DEFLATE output length does not match image dimensions");
    }
    let checksum = u32::from_be_bytes(data[compressed_end..].try_into().unwrap());
    if adler32(&output) != checksum {
        return fail("zlib Adler-32 mismatch");
    }
    Ok(output)
}

fn push_bounded(out: &mut Vec<u8>, byte: u8, expected: usize) -> DResult<()> {
    if out.len() >= expected {
        return fail("DEFLATE output exceeds image dimensions");
    }
    out.push(byte);
    Ok(())
}

struct DeflateBits<'a> {
    data: &'a [u8],
    bit: usize,
}
impl DeflateBits<'_> {
    fn read(&mut self, count: u8) -> DResult<u32> {
        if count > 24
            || self
                .bit
                .checked_add(count as usize)
                .is_none_or(|n| n > self.data.len() * 8)
        {
            return fail("truncated DEFLATE stream");
        }
        let mut value = 0;
        for shift in 0..count {
            value |= (((self.data[self.bit / 8] >> (self.bit % 8)) & 1) as u32) << shift;
            self.bit += 1;
        }
        Ok(value)
    }
    fn align(&mut self) {
        self.bit = (self.bit + 7) & !7;
    }
}

#[derive(Clone)]
struct Huffman {
    entries: Vec<(u32, u8, u16)>,
}
impl Huffman {
    fn new(lengths: &[u8]) -> DResult<Self> {
        let max = *lengths.iter().max().unwrap_or(&0);
        if max > 15 {
            return fail("invalid DEFLATE Huffman table");
        }
        if max == 0 {
            return Ok(Self {
                entries: Vec::new(),
            });
        }
        let mut counts = [0u16; 16];
        for &len in lengths {
            if len > 0 {
                counts[len as usize] += 1;
            }
        }
        let mut code = 0u32;
        let mut next = [0u32; 16];
        let mut remaining_codes = 1i32;
        for bits in 1..=15 {
            remaining_codes = (remaining_codes << 1) - counts[bits] as i32;
            if remaining_codes < 0 {
                return fail("oversubscribed DEFLATE Huffman table");
            }
            code = (code + counts[bits - 1] as u32) << 1;
            next[bits] = code;
        }
        let mut entries = Vec::with_capacity(lengths.len());
        for (symbol, &len) in lengths.iter().enumerate() {
            if len > 0 {
                let value = next[len as usize];
                next[len as usize] += 1;
                entries.push((value, len, symbol as u16));
            }
        }
        Ok(Self { entries })
    }
    fn decode(&self, bits: &mut DeflateBits<'_>) -> DResult<u16> {
        let mut code = 0u32;
        for len in 1..=15 {
            code = (code << 1) | bits.read(1)?;
            if let Some((_, _, symbol)) = self
                .entries
                .iter()
                .find(|(candidate, n, _)| *n == len && *candidate == code)
            {
                return Ok(*symbol);
            }
        }
        fail("invalid DEFLATE Huffman code")
    }
    fn fixed_lit() -> DResult<Self> {
        let mut lengths = vec![0; 288];
        lengths[..144].fill(8);
        lengths[144..256].fill(9);
        lengths[256..280].fill(7);
        lengths[280..].fill(8);
        Self::new(&lengths)
    }
    fn fixed_dist() -> DResult<Self> {
        Self::new(&[5; 32])
    }
}

fn dynamic_tables(bits: &mut DeflateBits<'_>) -> DResult<(Huffman, Huffman)> {
    let literal_count = bits.read(5)? as usize + 257;
    let distance_count = bits.read(5)? as usize + 1;
    let code_count = bits.read(4)? as usize + 4;
    if literal_count > 286 || distance_count > 32 {
        return fail("invalid DEFLATE dynamic table sizes");
    }
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let mut code_lengths = [0u8; 19];
    for &index in ORDER.iter().take(code_count) {
        code_lengths[index] = bits.read(3)? as u8;
    }
    let code_table = Huffman::new(&code_lengths)?;
    let total = literal_count + distance_count;
    let mut lengths = Vec::with_capacity(total);
    while lengths.len() < total {
        match code_table.decode(bits)? {
            symbol @ 0..=15 => lengths.push(symbol as u8),
            16 => {
                let previous = *lengths
                    .last()
                    .ok_or(DecodeError("DEFLATE repeat has no previous code length"))?;
                let repeat = bits.read(2)? as usize + 3;
                if lengths.len() + repeat > total {
                    return fail("DEFLATE code length repeat overrun");
                }
                lengths.resize(lengths.len() + repeat, previous);
            }
            17 => {
                let repeat = bits.read(3)? as usize + 3;
                if lengths.len() + repeat > total {
                    return fail("DEFLATE code length repeat overrun");
                }
                lengths.resize(lengths.len() + repeat, 0);
            }
            18 => {
                let repeat = bits.read(7)? as usize + 11;
                if lengths.len() + repeat > total {
                    return fail("DEFLATE code length repeat overrun");
                }
                lengths.resize(lengths.len() + repeat, 0);
            }
            _ => return fail("invalid DEFLATE code-length symbol"),
        }
    }
    if lengths.get(256) == Some(&0) {
        return fail("DEFLATE table has no end-of-block code");
    }
    let literal_table = Huffman::new(&lengths[..literal_count])?;
    let distance_lengths = &lengths[literal_count..];
    let distance_table = if distance_lengths.iter().all(|length| *length == 0) {
        Huffman {
            entries: Vec::new(),
        }
    } else {
        Huffman::new(distance_lengths)?
    };
    Ok((literal_table, distance_table))
}

fn adler32(data: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            a += byte as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}
fn crc32(data: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i32 + b as i32 - c as i32;
    let pa = (p - a as i32).abs();
    let pb = (p - b as i32).abs();
    let pc = (p - c as i32).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}
