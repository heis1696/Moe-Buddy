use serde::Deserialize;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use moe_agent_runtime::animation::{AnimationClip, ClipPlayer, PlaybackBoundary, PlaybackMode};

use super::decode::{decode_gif, decode_png};
use super::manifest::{MaterialError, MaterialPack};
use super::path::read_asset;
use super::player::{SequenceClipPlayer, SequencePlayerError};

static NEXT_DIR: AtomicU64 = AtomicU64::new(1);

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "moe-material-{}-{}",
            std::process::id(),
            NEXT_DIR.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn write(&self, name: &str, bytes: &[u8]) {
        fs::write(self.0.join(name), bytes).unwrap();
    }
    fn path(&self) -> &Path {
        &self.0
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn png_rgba(width: u32, height: u32, pixels: &[[u8; 4]]) -> Vec<u8> {
    let mut raw = Vec::new();
    for row in pixels.chunks(width as usize) {
        raw.push(0);
        for pixel in row {
            raw.extend_from_slice(pixel);
        }
    }
    let mut zlib = vec![0x78, 0x01, 0x01];
    let len = raw.len() as u16;
    zlib.extend_from_slice(&len.to_le_bytes());
    zlib.extend_from_slice(&(!len).to_le_bytes());
    zlib.extend_from_slice(&raw);
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&width.to_be_bytes());
    ihdr.extend_from_slice(&height.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    png_chunk(&mut bytes, b"IHDR", &ihdr);
    png_chunk(&mut bytes, b"IDAT", &zlib);
    png_chunk(&mut bytes, b"IEND", &[]);
    bytes
}

fn png_chunk(out: &mut Vec<u8>, kind: &[u8; 4], payload: &[u8]) {
    out.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(payload);
    let start = out.len() - payload.len() - 4;
    out.extend_from_slice(&crc32(&out[start..]).to_be_bytes());
}

fn adler32(bytes: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    for &x in bytes {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

fn gif_header(width: u16, height: u16, colors: &[[u8; 3]], background: u8) -> Vec<u8> {
    let mut gif = b"GIF89a".to_vec();
    gif.extend_from_slice(&width.to_le_bytes());
    gif.extend_from_slice(&height.to_le_bytes());
    let count_bits = (colors.len().next_power_of_two().trailing_zeros() as u8).saturating_sub(1);
    gif.push(0x80 | (count_bits.max(1) & 7));
    gif.push(background);
    gif.push(0);
    for color in colors {
        gif.extend_from_slice(color);
    }
    for _ in colors.len()..(1usize << (count_bits.max(1) as usize + 1)) {
        gif.extend_from_slice(&[0, 0, 0]);
    }
    gif
}

#[allow(clippy::too_many_arguments)]
fn gif_frame(
    gif: &mut Vec<u8>,
    left: u16,
    top: u16,
    width: u16,
    height: u16,
    disposal: u8,
    delay: u16,
    transparent: Option<u8>,
    indices: &[u8],
    interlace: bool,
) {
    gif.extend_from_slice(&[
        0x21,
        0xf9,
        4,
        ((disposal & 7) << 2) | u8::from(transparent.is_some()),
    ]);
    gif.extend_from_slice(&delay.to_le_bytes());
    gif.push(transparent.unwrap_or(0));
    gif.push(0);
    gif.push(0x2c);
    gif.extend_from_slice(&left.to_le_bytes());
    gif.extend_from_slice(&top.to_le_bytes());
    gif.extend_from_slice(&width.to_le_bytes());
    gif.extend_from_slice(&height.to_le_bytes());
    gif.push(if interlace { 0x40 } else { 0 });
    gif.push(2);
    let mut packed = Vec::new();
    for &index in indices {
        packed.extend_from_slice(&[4, index]);
    }
    packed.push(5);
    let bit_count = packed.len() * 3;
    let mut stream = vec![0u8; bit_count.div_ceil(8)];
    let mut bit = 0;
    for &value in &packed {
        for shift in 0..3 {
            if value & (1 << shift) != 0 {
                stream[bit / 8] |= 1 << (bit % 8);
            }
            bit += 1;
        }
    }
    for chunk in stream.chunks(255) {
        gif.push(chunk.len() as u8);
        gif.extend_from_slice(chunk);
    }
    gif.push(0);
}

fn gif_fixture() -> Vec<u8> {
    let mut gif = gif_header(2, 1, &[[0, 0, 0], [255, 0, 0], [0, 255, 0], [0, 0, 255]], 0);
    gif_frame(&mut gif, 0, 0, 1, 1, 1, 2, None, &[1], false);
    gif_frame(&mut gif, 1, 0, 1, 1, 1, 3, None, &[2], false);
    gif.push(0x3b);
    gif
}

fn gif_repeated_frames(count: usize) -> Vec<u8> {
    let mut gif = gif_header(1, 1, &[[0, 0, 0], [255, 0, 0]], 0);
    for _ in 0..count {
        gif_frame(&mut gif, 0, 0, 1, 1, 1, 1, None, &[1], false);
    }
    gif.push(0x3b);
    gif
}

fn gif_disposal_fixture(disposal: u8) -> Vec<u8> {
    let mut gif = gif_header(2, 1, &[[0, 0, 0], [255, 0, 0], [0, 255, 0], [0, 0, 255]], 0);
    gif_frame(&mut gif, 0, 0, 2, 1, 1, 1, None, &[1, 3], false);
    gif_frame(&mut gif, 1, 0, 1, 1, disposal, 1, None, &[2], false);
    gif_frame(&mut gif, 0, 0, 1, 1, 1, 1, Some(0), &[0], false);
    gif.push(0x3b);
    gif
}

fn gif_interlaced_fixture() -> Vec<u8> {
    let mut gif = gif_header(1, 4, &[[0, 0, 0], [255, 0, 0], [0, 255, 0], [0, 0, 255]], 0);
    gif_frame(&mut gif, 0, 0, 1, 4, 1, 1, None, &[1, 3, 2, 1], true);
    gif.push(0x3b);
    gif
}

fn gif_transparent_fixture() -> Vec<u8> {
    let mut gif = gif_header(2, 1, &[[0, 0, 0], [255, 0, 0]], 0);
    gif_frame(&mut gif, 0, 0, 1, 1, 1, 1, Some(0), &[1], false);
    gif.push(0x3b);
    gif
}

fn gif_zero_delay_fixture() -> Vec<u8> {
    let mut gif = gif_header(1, 1, &[[0, 0, 0], [255, 0, 0]], 0);
    gif_frame(&mut gif, 0, 0, 1, 1, 0, 0, None, &[1], false);
    gif.push(0x3b);
    gif
}

fn decode_hex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let digit = |value: u8| match value {
                b'0'..=b'9' => value - b'0',
                b'a'..=b'f' => value - b'a' + 10,
                _ => panic!("bad hex"),
            };
            (digit(pair[0]) << 4) | digit(pair[1])
        })
        .collect()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureIndex {
    fixtures: Vec<FixtureIndexEntry>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FixtureIndexEntry {
    name: String,
    expect: String,
    package_dir: String,
    expected_file: Option<String>,
}

#[derive(Deserialize)]
struct FixtureExpected {
    clips: std::collections::BTreeMap<String, ExpectedClip>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExpectedClip {
    frame_count: usize,
    durations_ms: Vec<u64>,
    frames: Vec<ExpectedFrame>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ExpectedFrame {
    width: u32,
    height: u32,
    duration_ms: u64,
    rgba_sha256: String,
    rgba_base64: Option<String>,
}

fn decode_base64(text: &str) -> Vec<u8> {
    fn value(byte: u8) -> u8 {
        match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => panic!("invalid base64 in generated expected pixels"),
        }
    }
    let bytes = text.as_bytes();
    assert_eq!(bytes.len() % 4, 0);
    let mut decoded = Vec::with_capacity(bytes.len() / 4 * 3);
    let (chunks, remainder) = bytes.as_chunks::<4>();
    assert!(remainder.is_empty());
    for chunk in chunks {
        let a = value(chunk[0]) as u32;
        let b = value(chunk[1]) as u32;
        let c = if chunk[2] == b'=' {
            0
        } else {
            value(chunk[2]) as u32
        };
        let d = if chunk[3] == b'=' {
            0
        } else {
            value(chunk[3]) as u32
        };
        let block = (a << 18) | (b << 12) | (c << 6) | d;
        decoded.push((block >> 16) as u8);
        if chunk[2] != b'=' {
            decoded.push((block >> 8) as u8);
        }
        if chunk[3] != b'=' {
            decoded.push(block as u8);
        }
    }
    decoded
}

fn sha256_hex(input: &[u8]) -> String {
    const K: [u32; 64] = [
        0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4,
        0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe,
        0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f,
        0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7,
        0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc,
        0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
        0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070, 0x19a4c116,
        0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
        0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7,
        0xc67178f2,
    ];
    let mut state = [
        0x6a09e667u32,
        0xbb67ae85,
        0x3c6ef372,
        0xa54ff53a,
        0x510e527f,
        0x9b05688c,
        0x1f83d9ab,
        0x5be0cd19,
    ];
    let bit_len = (input.len() as u64).wrapping_mul(8);
    let mut padded = input.to_vec();
    padded.push(0x80);
    while padded.len() % 64 != 56 {
        padded.push(0);
    }
    padded.extend_from_slice(&bit_len.to_be_bytes());
    let (blocks, remainder) = padded.as_chunks::<64>();
    assert!(remainder.is_empty());
    for block in blocks {
        let mut words = [0u32; 64];
        for (index, word) in block.as_chunks::<4>().0.iter().enumerate() {
            words[index] = u32::from_be_bytes(*word);
        }
        for index in 16..64 {
            let a = words[index - 15];
            let b = words[index - 2];
            let s0 = a.rotate_right(7) ^ a.rotate_right(18) ^ (a >> 3);
            let s1 = b.rotate_right(17) ^ b.rotate_right(19) ^ (b >> 10);
            words[index] = words[index - 16]
                .wrapping_add(s0)
                .wrapping_add(words[index - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = state;
        for index in 0..64 {
            let sum1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choose = (e & f) ^ ((!e) & g);
            let t1 = h
                .wrapping_add(sum1)
                .wrapping_add(choose)
                .wrapping_add(K[index])
                .wrapping_add(words[index]);
            let sum0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let t2 = sum0.wrapping_add(majority);
            [h, g, f, e, d, c, b, a] = [g, f, e, d.wrapping_add(t1), c, b, a, t1.wrapping_add(t2)];
        }
        for (slot, value) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *slot = slot.wrapping_add(value);
        }
    }
    state.iter().map(|word| format!("{word:08x}")).collect()
}

#[test]
fn generated_material_fixtures_match_production_decode_and_reject_contract() {
    let Some(root) = std::env::var_os("MOE_MATERIAL_FIXTURES") else {
        return;
    };
    let root = PathBuf::from(root);
    let index: FixtureIndex =
        serde_json::from_slice(&fs::read(root.join("index.json")).unwrap()).unwrap();
    assert_eq!(index.fixtures.len(), 42);
    for fixture in index.fixtures {
        let package = root.join(fixture.package_dir);
        if fixture.expect == "reject" || fixture.name == "valid-sequence-interlaced" {
            assert!(
                MaterialPack::load(package).is_err(),
                "{} must reject",
                fixture.name
            );
            continue;
        }
        let pack = MaterialPack::load(package)
            .unwrap_or_else(|error| panic!("{} rejected: {error}", fixture.name));
        let expected_file = fixture
            .expected_file
            .expect("accepted fixture has an independent pixel expectation");
        let expected: FixtureExpected =
            serde_json::from_slice(&fs::read(root.join(expected_file)).unwrap()).unwrap();
        for (clip_id, expected_clip) in expected.clips {
            let decoded = pack
                .decoded_clip(&clip_id)
                .unwrap_or_else(|| panic!("{} missing clip {clip_id}", fixture.name));
            assert_eq!(
                decoded.frames.len(),
                expected_clip.frame_count,
                "{} {clip_id}",
                fixture.name
            );
            assert_eq!(decoded.frames.len(), expected_clip.durations_ms.len());
            assert_eq!(decoded.frames.len(), expected_clip.frames.len());
            for (index, (actual, expected_frame)) in
                decoded.frames.iter().zip(expected_clip.frames).enumerate()
            {
                assert_eq!(
                    (actual.width, actual.height),
                    (expected_frame.width, expected_frame.height),
                    "{} {clip_id} frame {index}",
                    fixture.name
                );
                assert_eq!(
                    actual.duration_ms, expected_frame.duration_ms,
                    "{} {clip_id} frame {index}",
                    fixture.name
                );
                assert_eq!(actual.duration_ms, expected_clip.durations_ms[index]);
                assert_eq!(
                    sha256_hex(&actual.rgba),
                    expected_frame.rgba_sha256,
                    "{} {clip_id} frame {index}",
                    fixture.name
                );
                if let Some(rgba_base64) = expected_frame.rgba_base64 {
                    assert_eq!(
                        &*actual.rgba,
                        decode_base64(&rgba_base64),
                        "{} {clip_id} frame {index}",
                        fixture.name
                    );
                }
            }
        }
    }
}

#[test]
fn fixture_sha256_matches_standard_vectors() {
    assert_eq!(
        sha256_hex(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        sha256_hex(b"abc"),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}

fn manifest(clips: &str) -> String {
    format!(
        r#"{{"formatVersion":1,"id":"cat","version":7,"label":"Cat","clips":[{clips}],"actions":[{{"id":"idle","label":"Idle","clip":"walk","mode":"loop"}}],"defaultAction":"idle"}}"#
    )
}

fn sequence_manifest(path: &str, duration: u64) -> String {
    manifest(&format!(
        r#"{{"id":"walk","type":"sequence","frames":[{{"path":"{path}","durationMs":{duration}}}]}}"#
    ))
}

#[test]
fn png_decodes_actual_pixels_and_rejects_bad_crc_and_truncation() {
    let bytes = png_rgba(1, 1, &[[11, 22, 33, 44]]);
    let decoded = decode_png(&bytes, 1024).unwrap();
    assert_eq!(decoded.rgba, [11, 22, 33, 44]);
    let mut bad = bytes.clone();
    *bad.last_mut().unwrap() ^= 0xff;
    assert!(decode_png(&bad, 1024).is_err());
    assert!(decode_png(&bytes[..bytes.len() - 4], 1024).is_err());
    assert!(decode_png(&bytes, 3).is_err());
}

#[test]
fn indexed_png_rejects_transparency_length_before_copying_payload() {
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&1u32.to_be_bytes());
    ihdr.extend_from_slice(&1u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 3, 0, 0, 0]);
    png_chunk(&mut bytes, b"IHDR", &ihdr);
    png_chunk(&mut bytes, b"PLTE", &[12, 34, 56]);
    png_chunk(&mut bytes, b"tRNS", &[0, 0]);
    let raw = [0, 0];
    let mut zlib = vec![0x78, 0x01, 0x01, 2, 0, 253, 255];
    zlib.extend_from_slice(&raw);
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());
    png_chunk(&mut bytes, b"IDAT", &zlib);
    png_chunk(&mut bytes, b"IEND", &[]);
    assert!(decode_png(&bytes, 1024).is_err());
}

#[test]
fn png_inflater_decodes_a_dynamic_huffman_stream_and_checks_adler() {
    let stream = decode_hex(
        "789cedcdd501820000054014bbbb5b01136c2c540cec165b71ff291cc39f770b1ca152931aad4e6f309acc16abcdee70badc1eafcf1f0886c291682c9e48a6d2198a66b2b97ca15862b972a55aab379a7cabdde9f684fe60288ec693a9349b2f96abf566bbdb1f8ea7b37cb9deee8fe7ebfd51be0402040810204080000102040810204080e0dfc10f1029fce6",
    );
    let decoded = super::decode::inflate_zlib(&stream, 4096).unwrap();
    assert_eq!(
        decoded,
        (0..4096)
            .map(|value| (value % 97) as u8)
            .collect::<Vec<_>>()
    );
    let mut corrupt = stream;
    *corrupt.last_mut().unwrap() ^= 1;
    assert!(super::decode::inflate_zlib(&corrupt, 4096).is_err());
}

#[test]
fn png_inflater_accepts_literal_only_dynamic_block_without_distance_codes() {
    let zlib = decode_hex("780105e32101000000820080fa7fb305d31d0019000b");
    let mut bytes = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&1u32.to_be_bytes());
    ihdr.extend_from_slice(&1u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    png_chunk(&mut bytes, b"IHDR", &ihdr);
    png_chunk(&mut bytes, b"IDAT", &zlib);
    png_chunk(&mut bytes, b"IEND", &[]);
    let decoded = decode_png(&bytes, 1024).unwrap();
    assert_eq!(decoded.rgba, [1, 2, 3, 4]);
}

#[test]
fn manifest_rejects_unknown_duplicate_fields_and_unknown_versions_before_assets() {
    let dir = Scratch::new();
    dir.write(
        "manifest.json",
        sequence_manifest("missing.png", 3)
            .replace("\"formatVersion\":1", "\"formatVersion\":2")
            .as_bytes(),
    );
    assert!(matches!(
        MaterialPack::load(dir.path()),
        Err(MaterialError::Invalid("unknown manifest format version"))
    ));
    dir.write(
        "manifest.json",
        sequence_manifest("missing.png", 3)
            .replace("\"formatVersion\":1", "\"formatVersion\":1,\"extra\":true")
            .as_bytes(),
    );
    assert!(matches!(
        MaterialPack::load(dir.path()),
        Err(MaterialError::Json(_))
    ));
    dir.write(
        "manifest.json",
        sequence_manifest("missing.png", 3)
            .replace("\"id\":\"cat\"", "\"id\":\"cat\",\"id\":\"dup\"")
            .as_bytes(),
    );
    assert!(matches!(
        MaterialPack::load(dir.path()),
        Err(MaterialError::Json(_))
    ));
}

#[test]
fn path_escape_and_reparse_points_are_rejected_at_open_boundary() {
    let dir = Scratch::new();
    let outside = Scratch::new();
    assert!(MaterialPack::load(Path::new(".")).is_err());
    assert!(read_asset(dir.path(), "../escape", 10).is_err());
    assert!(read_asset(dir.path(), "C:/escape", 10).is_err());
    dir.write("inside", b"ok");
    outside.write("escape", b"outside");
    #[cfg(windows)]
    {
        use std::os::windows::fs::{symlink_dir, symlink_file};
        assert!(super::path::canonical_package_root(Path::new(r"\\server\share\pack")).is_err());
        let link = dir.0.join("link");
        symlink_file(outside.0.join("escape"), &link)
            .expect("Windows reparse fixture must be creatable");
        assert!(read_asset(dir.path(), "link", 10).is_err());
        symlink_dir(outside.path(), dir.0.join("nested"))
            .expect("Windows directory reparse fixture must be creatable");
        assert!(read_asset(dir.path(), "nested/escape", 10).is_err());
        symlink_dir(outside.path(), dir.0.join("root-link"))
            .expect("Windows package-root reparse fixture must be creatable");
        assert!(MaterialPack::load(dir.0.join("root-link")).is_err());
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(outside.0.join("escape"), dir.0.join("link")).unwrap();
        assert!(read_asset(dir.path(), "link", 10).is_err());
        std::os::unix::fs::symlink(outside.path(), dir.0.join("nested")).unwrap();
        assert!(read_asset(dir.path(), "nested/escape", 10).is_err());
        std::os::unix::fs::symlink(outside.path(), dir.0.join("root-link")).unwrap();
        assert!(MaterialPack::load(dir.0.join("root-link")).is_err());
    }
}

#[test]
fn bounded_asset_and_decoder_entry_points_honor_cooperative_cancellation() {
    let dir = Scratch::new();
    let png = png_rgba(1, 1, &[[1, 2, 3, 255]]);
    dir.write("asset", &png);
    let cancelled = std::sync::atomic::AtomicBool::new(true);
    assert!(matches!(
        super::path::read_asset_cancellable(dir.path(), "asset", 1024, Some(&cancelled)),
        Err(MaterialError::Cancelled)
    ));
    assert_eq!(
        super::decode::decode_png_cancellable(&png, 1024, Some(&cancelled))
            .unwrap_err()
            .0,
        "cancelled"
    );
    assert_eq!(
        super::decode::decode_gif_cancellable(&gif_fixture(), 10, 1024, 1000, Some(&cancelled))
            .unwrap_err()
            .0,
        "cancelled"
    );
}

#[test]
#[cfg(windows)]
fn package_root_retains_directory_handles_until_validation_finishes() {
    let dir = Scratch::new();
    let parent = dir.path().join("parent");
    let package = parent.join("pack");
    fs::create_dir_all(&package).unwrap();
    let moved = dir.path().join("moved-parent");
    let guard = super::path::canonical_package_root(&package).unwrap();
    let renamed_while_live = fs::rename(&parent, &moved).is_ok();
    if renamed_while_live {
        fs::rename(&moved, &parent).unwrap();
    }
    drop(guard);
    assert!(
        !renamed_while_live,
        "package ancestors could change during validation"
    );
    fs::rename(&parent, &moved).unwrap();
    fs::rename(&moved, &parent).unwrap();
}

#[test]
fn bounded_read_never_returns_bytes_beyond_the_limit_during_concurrent_replacement() {
    let dir = Scratch::new();
    dir.write("asset", &[7; 8]);
    let root = dir.path().to_path_buf();
    let writer_path = root.join("asset");
    let writer = std::thread::spawn(move || {
        for size in 1..=64 {
            let _ = fs::write(&writer_path, vec![size as u8; size]);
        }
    });
    for _ in 0..64 {
        if let Ok(bytes) = read_asset(&root, "asset", 16) {
            assert!(bytes.len() <= 16);
        }
    }
    writer.join().unwrap();
}

#[test]
fn budgets_are_preflighted_before_any_asset_read() {
    let dir = Scratch::new();
    let frame = r#"{"path":"not-created.png","durationMs":600000}"#;
    let frames = std::iter::repeat_n(frame, 7).collect::<Vec<_>>().join(",");
    let too_long = manifest(&format!(
        r#"{{"id":"walk","type":"sequence","frames":[{frames}]}}"#
    ));
    dir.write("manifest.json", too_long.as_bytes());
    assert!(matches!(
        MaterialPack::load(dir.path()),
        Err(MaterialError::Invalid("clip duration limit exceeded"))
    ));
    dir.write(
        "manifest.json",
        sequence_manifest("not-created.png", 1).as_bytes(),
    );
    assert!(matches!(
        MaterialPack::load(dir.path()),
        Err(MaterialError::Io(_))
    ));
}

#[test]
fn gif_decodes_variable_delay_and_composites_local_rectangles() {
    let decoded = decode_gif(&gif_fixture(), 10, 1024, 3_600_000).unwrap();
    assert_eq!(decoded.len(), 2);
    assert_eq!((decoded[0].duration_ms, decoded[1].duration_ms), (20, 30));
    assert_eq!(&decoded[1].rgba[0..4], &[255, 0, 0, 255]);
    assert_eq!(&decoded[1].rgba[4..8], &[0, 255, 0, 255]);
    assert!(decode_gif(&gif_fixture(), 1, 1024, 3_600_000).is_err());
    assert!(decode_gif(&gif_fixture(), 10, 1024, 20).is_err());
    assert!(decode_gif(&gif_fixture(), 10, 4, 3_600_000).is_err());
    assert!(decode_gif(&gif_zero_delay_fixture(), 10, 1024, 3_600_000).is_err());
    let restored = decode_gif(&gif_disposal_fixture(3), 10, 1024, 3_600_000).unwrap();
    assert_eq!(&restored[2].rgba[0..4], &[255, 0, 0, 255]);
    assert_eq!(&restored[2].rgba[4..8], &[0, 0, 255, 255]);
    let cleared = decode_gif(&gif_disposal_fixture(2), 10, 1024, 3_600_000).unwrap();
    assert_eq!(&cleared[2].rgba[4..8], &[0, 0, 0, 0]);
    let retained = decode_gif(&gif_disposal_fixture(0), 10, 1024, 3_600_000).unwrap();
    assert_eq!(&retained[2].rgba[4..8], &[0, 255, 0, 255]);
    let interlaced = decode_gif(&gif_interlaced_fixture(), 10, 1024, 3_600_000).unwrap();
    assert_eq!(
        interlaced[0]
            .rgba
            .as_chunks::<4>()
            .0
            .iter()
            .map(|px| px.to_vec())
            .collect::<Vec<_>>(),
        vec![
            vec![255, 0, 0, 255],
            vec![0, 255, 0, 255],
            vec![0, 0, 255, 255],
            vec![255, 0, 0, 255]
        ]
    );
    let transparent = decode_gif(&gif_transparent_fixture(), 10, 1024, 3_600_000).unwrap();
    assert_eq!(&transparent[0].rgba[..4], &[255, 0, 0, 255]);
    assert_eq!(&transparent[0].rgba[4..], &[0, 0, 0, 0]);
    let mut truncated = gif_fixture();
    truncated.pop();
    assert!(decode_gif(&truncated, 10, 1024, 3_600_000).is_err());
}

#[test]
fn mixed_gif_frame_budget_rejects_later_sequence_before_opening_its_file() {
    let dir = Scratch::new();
    dir.write("a.gif", &gif_repeated_frames(256));
    dir.write("b.gif", &gif_repeated_frames(256));
    let json = r#"{"formatVersion":1,"id":"cat","version":7,"label":"Cat","clips":[{"id":"a","type":"gif","path":"a.gif"},{"id":"b","type":"gif","path":"b.gif"},{"id":"late","type":"sequence","frames":[{"path":"missing.png","durationMs":1}]}],"actions":[{"id":"idle","label":"Idle","clip":"a","mode":"loop"}],"defaultAction":"idle"}"#;
    dir.write("manifest.json", json.as_bytes());
    let error = MaterialPack::load(dir.path()).unwrap_err();
    assert!(
        matches!(
            error,
            MaterialError::Invalid("package frame limit exceeded")
        ),
        "expected frame budget rejection before opening missing.png, got {error}"
    );
}

#[test]
fn package_player_uses_variable_frame_boundaries_tickets_pause_and_drop() {
    let dir = Scratch::new();
    dir.write("frame.png", &png_rgba(1, 1, &[[1, 2, 3, 255]]));
    dir.write("second.png", &png_rgba(1, 1, &[[9, 8, 7, 255]]));
    let json = manifest(
        r#"{"id":"walk","type":"sequence","frames":[{"path":"frame.png","durationMs":7},{"path":"second.png","durationMs":16}]}"#,
    );
    dir.write("manifest.json", json.as_bytes());
    let pack = MaterialPack::load(dir.path()).unwrap();
    let weak = Arc::downgrade(&pack.decoded_clip("walk").unwrap().frames[0].rgba);
    let clip = AnimationClip::new("walk", 7, 23).unwrap();
    let animator_a = started_animator(SequenceClipPlayer::new(pack.clone()), clip.clone());
    let animator_b = started_animator(SequenceClipPlayer::new(pack.clone()), clip.clone());
    let ticket_a = animator_a.snapshot().ticket.unwrap();
    let ticket_b = animator_b.snapshot().ticket.unwrap();
    assert_ne!(ticket_a, ticket_b);
    let mut player = SequenceClipPlayer::new(pack.clone());
    player.prepare(&clip, PlaybackMode::Loop).unwrap();
    player.start(&clip, PlaybackMode::Loop, &ticket_a).unwrap();
    let reader = player.frame_reader();
    let mut read_only_frame = reader.current_frame(&ticket_a).unwrap();
    assert_eq!(
        (read_only_frame.frame_index, &*read_only_frame.rgba),
        (0, &[1, 2, 3, 255][..])
    );
    assert_eq!(
        Arc::strong_count(&pack.decoded_clip("walk").unwrap().frames[0].rgba),
        8
    );
    assert_eq!(
        player.advance(&ticket_b, 1),
        Err(SequencePlayerError::WrongTicket)
    );
    player.pause(&ticket_a).unwrap();
    assert_eq!(
        player.advance(&ticket_a, 1),
        Err(SequencePlayerError::Paused)
    );
    player.resume(&ticket_a).unwrap();
    player.advance(&ticket_a, 6).unwrap();
    assert_eq!(player.current_frame(&ticket_a).unwrap().frame_index, 0);
    player.advance(&ticket_a, 1).unwrap();
    let frame = player.current_frame(&ticket_a).unwrap();
    assert_eq!((frame.frame_index, frame.rgba), (1, &[9, 8, 7, 255][..]));
    read_only_frame = reader.current_frame(&ticket_a).unwrap();
    assert_eq!(
        (read_only_frame.frame_index, &*read_only_frame.rgba),
        (1, &[9, 8, 7, 255][..])
    );
    let boundary = player.advance(&ticket_a, 16).unwrap();
    assert_eq!(boundary.boundary, Some(PlaybackBoundary::LoopBoundary));
    player.stop(&ticket_a).unwrap();
    assert!(matches!(
        reader.current_frame(&ticket_a),
        Err(SequencePlayerError::WrongTicket)
    ));
    assert_eq!(
        Arc::strong_count(&pack.decoded_clip("walk").unwrap().frames[0].rgba),
        5
    );
    let mut once = SequenceClipPlayer::new(pack.clone());
    once.prepare(&clip, PlaybackMode::Once).unwrap();
    once.start(&clip, PlaybackMode::Once, &ticket_a).unwrap();
    assert_eq!(
        once.advance(&ticket_a, 23).unwrap().boundary,
        Some(PlaybackBoundary::Completed)
    );
    assert_eq!(once.current_frame(&ticket_a).unwrap().frame_index, 1);
    assert_eq!(
        once.advance(&ticket_a, 1),
        Err(SequencePlayerError::PlaybackComplete)
    );
    once.stop(&ticket_a).unwrap();
    drop(read_only_frame);
    drop(once);
    drop(reader);
    drop(player);
    drop(animator_a);
    drop(animator_b);
    drop(pack);
    assert!(weak.upgrade().is_none());
}

#[test]
fn material_host_runs_real_animator_and_rejects_stale_pixel_triples() {
    let dir = Scratch::new();
    dir.write("frame.png", &png_rgba(1, 1, &[[1, 2, 3, 255]]));
    dir.write("second.png", &png_rgba(1, 1, &[[9, 8, 7, 255]]));
    dir.write("manifest.json", manifest(r#"{"id":"walk","type":"sequence","frames":[{"path":"frame.png","durationMs":7},{"path":"second.png","durationMs":16}]}"#).as_bytes());
    let pack = MaterialPack::load(dir.path()).unwrap();
    let pixel_owner = Arc::downgrade(&pack.decoded_clip("walk").unwrap().frames[0].rgba);
    let mut host = super::host::MaterialHost::default();
    host.install(pack).unwrap();
    let initial = host.frame_triple();
    assert_eq!(host.state().source, "package");
    assert_eq!(host.frame_pixels(initial).unwrap(), [1, 2, 3, 255]);

    host.play("idle").unwrap();
    let replay = host.frame_triple();
    assert_ne!(initial.1, replay.1);
    assert!(host.frame_pixels(initial).is_err());
    host.advance(7).unwrap();
    let frame = host.current_frame().unwrap().1;
    assert_eq!((frame.frame_index, &*frame.rgba), (1, &[9, 8, 7, 255][..]));
    let current = host.frame_triple();
    assert_eq!(host.frame_pixels(current).unwrap(), [9, 8, 7, 255]);
    host.pause().unwrap();
    assert_eq!(host.state().status, "paused");
    host.resume().unwrap();
    host.stop().unwrap();
    assert_eq!(host.frame_snapshot().source, "empty");
    assert!(host.frame_pixels(host.frame_triple()).is_err());
    assert!(pixel_owner.upgrade().is_some());
    host.play("idle").unwrap();
    assert_eq!(host.state().status, "playing");
    assert_eq!(
        host.frame_pixels(host.frame_triple()).unwrap(),
        [1, 2, 3, 255]
    );
    host.stop().unwrap();
    host.unload().unwrap();
    assert_eq!(host.state().source, "empty");
    assert!(pixel_owner.upgrade().is_none());
    host.builtin().unwrap();
    assert_eq!(host.state().source, "builtin");
    assert_eq!(host.state().status, "stopped");
    assert_eq!(host.state().action_id, None);
    assert_eq!(host.state().actions[0].id, "idle");
}

#[test]
fn material_revision_exhaustion_rejects_before_playback_or_state_mutation() {
    let dir = Scratch::new();
    dir.write("frame.png", &png_rgba(1, 1, &[[1, 2, 3, 255]]));
    dir.write(
        "manifest.json",
        manifest(
            r#"{"id":"walk","type":"sequence","frames":[{"path":"frame.png","durationMs":7}]}"#,
        )
        .as_bytes(),
    );
    let pack = MaterialPack::load(dir.path()).unwrap();
    let mut host = super::host::MaterialHost::default();
    host.install(pack).unwrap();
    let ticket = host.frame_triple();
    host.set_revision_for_test(9_007_199_254_740_991);
    assert_eq!(host.stop(), Err("material_generation_exhausted".into()));
    assert_eq!(host.state().revision, 9_007_199_254_740_991);
    assert_eq!(host.state().status, "playing");
    assert_eq!(host.frame_triple(), ticket);
    assert_eq!(
        host.set_error("test", "safe"),
        Err("material_generation_exhausted".into())
    );
}

#[test]
fn animator_switch_replay_continuous_nodes_pause_remainder_and_release() {
    use moe_agent_runtime::animation::{
        AnimationAction, AnimationDefinition, AnimationEdge, AnimationNode, AnimationSpec,
        AnimationTarget, EdgeCondition, PreemptionPolicy,
    };

    let dir = Scratch::new();
    dir.write("frame.png", &png_rgba(1, 1, &[[1, 2, 3, 255]]));
    dir.write("second.png", &png_rgba(1, 1, &[[9, 8, 7, 255]]));
    dir.write("manifest.json", manifest(
        r#"{"id":"walk","type":"sequence","frames":[{"path":"frame.png","durationMs":7},{"path":"second.png","durationMs":16}]} , {"id":"blink","type":"sequence","frames":[{"path":"frame.png","durationMs":23}]}"#,
    ).as_bytes());
    let pack = MaterialPack::load(dir.path()).unwrap();
    let weak = Arc::downgrade(&pack.decoded_clip("walk").unwrap().frames[0].rgba);
    let walk = AnimationClip::new("walk", 7, 23).unwrap();
    let blink = AnimationClip::new("blink", 7, 23).unwrap();
    let transition = AnimationEdge {
        condition: EdgeCondition::Completed,
        priority: 0,
        target: AnimationTarget::Node("next".into()),
    };
    let definition = AnimationDefinition::new(AnimationSpec {
        version: 1,
        clips: vec![walk.clone(), blink.clone()],
        actions: vec![
            AnimationAction {
                id: "walk".into(),
                priority: 0,
                preemption: PreemptionPolicy::Immediate,
                entry: "first".into(),
                nodes: vec![
                    AnimationNode {
                        id: "first".into(),
                        clip: "walk".into(),
                        mode: PlaybackMode::Once,
                        edges: vec![transition],
                    },
                    AnimationNode {
                        id: "next".into(),
                        clip: "walk".into(),
                        mode: PlaybackMode::Loop,
                        edges: vec![],
                    },
                ],
                exit: None,
            },
            AnimationAction {
                id: "blink".into(),
                priority: 1,
                preemption: PreemptionPolicy::Immediate,
                entry: "blink".into(),
                nodes: vec![AnimationNode {
                    id: "blink".into(),
                    clip: "blink".into(),
                    mode: PlaybackMode::Loop,
                    edges: vec![],
                }],
                exit: None,
            },
        ],
        bindings: vec![],
        parameters: vec![],
        triggers: vec![],
    })
    .unwrap();
    let mut animator = moe_agent_runtime::animation::Animator::new(
        definition,
        SequenceClipPlayer::new(pack.clone()),
    );

    animator.request_action("walk").unwrap();
    let first_ticket = animator.snapshot().ticket.unwrap();
    animator.advance(28).unwrap();
    assert_eq!(animator.snapshot().ticket.as_ref().unwrap().clip(), "walk");
    assert_ne!(animator.snapshot().ticket.as_ref().unwrap(), &first_ticket);
    assert_eq!(animator.playback_progress().unwrap().unwrap().elapsed_ms, 5);

    animator.pause().unwrap();
    let paused_ticket = animator.snapshot().ticket.unwrap();
    assert!(animator.advance(100).is_err());
    assert_eq!(animator.playback_progress().unwrap().unwrap().elapsed_ms, 5);
    animator.resume().unwrap();
    animator.advance(2).unwrap();
    assert_eq!(animator.playback_progress().unwrap().unwrap().elapsed_ms, 7);
    animator.request_action("blink").unwrap();
    let blink_ticket = animator.snapshot().ticket.unwrap();
    assert_eq!(blink_ticket.clip(), "blink");
    assert_ne!(blink_ticket, paused_ticket);

    animator.stop().unwrap();
    animator.request_action("walk").unwrap();
    let replay_ticket = animator.snapshot().ticket.unwrap();
    assert_eq!(replay_ticket.clip(), "walk");
    assert_ne!(replay_ticket, first_ticket);
    assert_ne!(replay_ticket, paused_ticket);
    assert!(weak.upgrade().is_some());
    drop(animator);
    drop(pack);
    assert!(weak.upgrade().is_none());
}

fn started_animator(
    player: SequenceClipPlayer,
    clip: AnimationClip,
) -> moe_agent_runtime::animation::Animator<SequenceClipPlayer> {
    use moe_agent_runtime::animation::{
        AnimationAction, AnimationDefinition, AnimationNode, AnimationSpec, PreemptionPolicy,
    };
    let node = AnimationNode {
        id: "node".into(),
        clip: clip.id().into(),
        mode: PlaybackMode::Loop,
        edges: vec![],
    };
    let action = AnimationAction {
        id: "idle".into(),
        priority: 0,
        preemption: PreemptionPolicy::Immediate,
        entry: "node".into(),
        nodes: vec![node],
        exit: None,
    };
    let spec = AnimationSpec {
        version: 1,
        clips: vec![clip],
        actions: vec![action],
        bindings: vec![],
        parameters: vec![],
        triggers: vec![],
    };
    let mut animator = moe_agent_runtime::animation::Animator::new(
        AnimationDefinition::new(spec).unwrap(),
        player,
    );
    animator.request_action("idle").unwrap();
    animator
}
