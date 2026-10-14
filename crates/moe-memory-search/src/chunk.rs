//! Conservative character-window chunking for embedding views.
//!
//! R2 slicing primitive: explicit Unicode character windows plus an explicit
//! UTF-8 byte budget. This is not a tokenizer integration: sizes are counted
//! in characters and UTF-8 bytes only, and no 384/48-token or model-window
//! claim is made. Hosts calibrate real token limits per embedding profile.
//!
//! Contract:
//! - Deterministic: identical text and config always produce identical
//!   slices; no randomness, clock, or environment input.
//! - Preferred boundaries: paragraph, heading, list, and sentence-end cut
//!   positions are preferred in that priority order (latest position within
//!   one kind); otherwise the window is cut at its hard limit. Paragraph
//!   positions are line starts after a blank line; headings are ATX `#`
//!   lines; list items are `- `/`* `/`+ ` and bounded `123.`/`123)` lines;
//!   sentence ends follow `。！？…` and whitespace-delimited `.!?`.
//! - Complete coverage: slices are ordered, non-empty, char-boundary aligned
//!   character intervals; adjacent slices overlap by the configured amount,
//!   degenerating only when a slice cannot fill it, and the tail is never
//!   dropped. A single character larger than the byte budget is rejected,
//!   never split or skipped. Grapheme clusters may still be divided: the
//!   guarantee is per `char` (Unicode scalar value), matching the budget.
//! - Bounded: slice size comes from the config; slice count is capped, and
//!   inputs needing more slices are rejected instead of silently degraded.
//!
//! This module performs no authorization: callers must bind produced slices
//! to sealed tickets, record versions, and index keys per MEMORY R.4/R.6.

use std::fmt;

use crate::SearchError;

/// Hard cap on slices produced from one text; exceeding it is an explicit
/// `SearchError::InputTooLong`, never a silent truncation or unbounded work.
const MAX_CHUNKS: usize = 1024;

/// Deterministic slicing configuration bound into index keys (MEMORY R.6).
///
/// Budgets are explicit character and UTF-8 byte limits, not tokenizer
/// tokens; `version` is assigned by the trusted host whenever a chunking
/// configuration change requires index rebuilds.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ChunkConfig {
    version: u64,
    max_chars: usize,
    overlap_chars: usize,
    max_utf8_bytes: usize,
}

impl ChunkConfig {
    /// Validates the configuration. Version 0, empty windows, overlaps that
    /// leave no forward progress, and zero byte budgets are rejected.
    pub fn new(
        version: u64,
        max_chars: usize,
        overlap_chars: usize,
        max_utf8_bytes: usize,
    ) -> Result<Self, SearchError> {
        if version == 0 || max_chars == 0 || overlap_chars >= max_chars || max_utf8_bytes == 0 {
            return Err(SearchError::InvalidConfig("invalid chunk limits"));
        }
        Ok(Self {
            version,
            max_chars,
            overlap_chars,
            max_utf8_bytes,
        })
    }

    /// Host-assigned configuration version for index keys and caches.
    pub fn version(&self) -> u64 {
        self.version
    }

    /// Maximum characters per slice.
    pub fn max_chars(&self) -> usize {
        self.max_chars
    }

    /// Characters shared between adjacent slices.
    pub fn overlap_chars(&self) -> usize {
        self.overlap_chars
    }

    /// Maximum UTF-8 encoded bytes per slice.
    pub fn max_utf8_bytes(&self) -> usize {
        self.max_utf8_bytes
    }
}

/// One deterministic slice of the source text.
///
/// `start_char`/`end_char` are character offsets into the source with an
/// exclusive end; `text` always equals exactly that slice of the source.
/// The body is redacted from [`fmt::Debug`] like other memory-derived
/// content.
#[derive(Clone, PartialEq, Eq)]
pub struct TextChunk {
    ordinal: usize,
    start_char: usize,
    end_char: usize,
    text: String,
}

impl TextChunk {
    /// Stable zero-based position of this slice within the split.
    pub fn ordinal(&self) -> usize {
        self.ordinal
    }

    /// First character offset of the slice in the source text.
    pub fn start_char(&self) -> usize {
        self.start_char
    }

    /// Exclusive end character offset of the slice in the source text.
    pub fn end_char(&self) -> usize {
        self.end_char
    }

    /// The slice body: exactly the source characters in
    /// `[start_char, end_char)`.
    pub fn text(&self) -> &str {
        &self.text
    }
}

impl fmt::Debug for TextChunk {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "TextChunk {{ ordinal: {}, chars: {}..{}, text: [redacted] }}",
            self.ordinal, self.start_char, self.end_char
        )
    }
}

#[derive(Clone, Copy)]
pub(crate) struct ChunkView<'text> {
    pub(crate) ordinal: usize,
    pub(crate) start_char: usize,
    pub(crate) end_char: usize,
    text: &'text str,
}

impl ChunkView<'_> {
    pub(crate) fn text(&self) -> &str {
        self.text
    }

    pub(crate) fn to_owned(self) -> TextChunk {
        TextChunk {
            ordinal: self.ordinal,
            start_char: self.start_char,
            end_char: self.end_char,
            text: self.text.to_owned(),
        }
    }
}

pub(crate) struct ChunkIter<'text, 'config> {
    text: &'text str,
    config: &'config ChunkConfig,
    start_byte: usize,
    start_char: usize,
    ordinal: usize,
    done: bool,
}

pub(crate) fn chunk_views<'text, 'config>(
    text: &'text str,
    config: &'config ChunkConfig,
) -> ChunkIter<'text, 'config> {
    ChunkIter {
        text,
        config,
        start_byte: 0,
        start_char: 0,
        ordinal: 0,
        done: false,
    }
}

impl<'text> Iterator for ChunkIter<'text, '_> {
    type Item = Result<ChunkView<'text>, SearchError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.done || self.start_byte >= self.text.len() {
            return None;
        }
        if self.ordinal >= MAX_CHUNKS {
            self.done = true;
            return Some(Err(SearchError::InputTooLong));
        }
        let window = match measure_window(self.text, self.start_byte, self.config) {
            Ok(window) => window,
            Err(error) => {
                self.done = true;
                return Some(Err(error));
            }
        };
        let (cut_byte, end_char) = if window.byte_end == self.text.len() {
            (window.byte_end, self.start_char + window.chars)
        } else {
            let cut_byte = find_boundary(
                self.text,
                self.start_byte,
                window.byte_end,
                self.config.overlap_chars(),
            )
            .unwrap_or(window.byte_end);
            (
                cut_byte,
                self.start_char + self.text[self.start_byte..cut_byte].chars().count(),
            )
        };
        let view = ChunkView {
            ordinal: self.ordinal,
            start_char: self.start_char,
            end_char,
            text: &self.text[self.start_byte..cut_byte],
        };
        self.ordinal += 1;
        if cut_byte == self.text.len() {
            self.done = true;
        } else {
            let next_char = if end_char - self.start_char > self.config.overlap_chars() {
                end_char - self.config.overlap_chars()
            } else {
                self.start_char + 1
            };
            self.start_byte =
                advance_chars(self.text, self.start_byte, next_char - self.start_char);
            self.start_char = next_char;
        }
        Some(Ok(view))
    }
}

/// Splits `text` into deterministic, overlap-bearing character slices under
/// the configured character, byte, and count budgets. Empty text yields no
/// slices. Fails with `SearchError::InputTooLong` when a single character
/// exceeds the byte budget or the input would need more than the capped
/// slice count; it never truncates or drops text.
pub fn split_chunks(text: &str, config: &ChunkConfig) -> Result<Vec<TextChunk>, SearchError> {
    chunk_views(text, config)
        .map(|chunk| chunk.map(ChunkView::to_owned))
        .collect()
}

struct Window {
    byte_end: usize,
    chars: usize,
}

/// Char- and byte-capped window end; errors when even one character cannot
/// fit into the byte budget.
fn measure_window(text: &str, start: usize, config: &ChunkConfig) -> Result<Window, SearchError> {
    let mut byte_end = start;
    let mut chars = 0usize;
    for current in text[start..].chars() {
        if chars >= config.max_chars() {
            break;
        }
        let byte_len = current.len_utf8();
        if byte_end - start + byte_len > config.max_utf8_bytes() {
            if chars == 0 {
                return Err(SearchError::InputTooLong);
            }
            break;
        }
        byte_end += byte_len;
        chars += 1;
    }
    Ok(Window { byte_end, chars })
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum BoundaryKind {
    Paragraph,
    Heading,
    ListItem,
    Sentence,
}

/// Latest highest-priority cut position whose slice keeps the configured
/// overlap and still advances; `None` falls back to the hard window end.
fn find_boundary(text: &str, start: usize, hard_end: usize, overlap_chars: usize) -> Option<usize> {
    let mut best: Option<(BoundaryKind, usize)> = None;
    let mut previous: Option<char> = None;
    for (window_chars, (offset, current)) in text[start..hard_end].char_indices().enumerate() {
        if window_chars > overlap_chars {
            let position = start + offset;
            if let Some(previous_char) = previous {
                if previous_char == '\n'
                    && let Some(kind) = line_boundary_kind(text, position)
                {
                    consider_boundary(&mut best, kind, position);
                }
                if is_sentence_end(previous_char, current) {
                    consider_boundary(&mut best, BoundaryKind::Sentence, position);
                }
            }
        }
        previous = Some(current);
    }
    best.map(|(_, position)| position)
}

fn consider_boundary(
    best: &mut Option<(BoundaryKind, usize)>,
    kind: BoundaryKind,
    position: usize,
) {
    let replace = match *best {
        Some((best_kind, best_position)) => {
            kind < best_kind || (kind == best_kind && position > best_position)
        }
        None => true,
    };
    if replace {
        *best = Some((kind, position));
    }
}

fn line_boundary_kind(text: &str, position: usize) -> Option<BoundaryKind> {
    if is_blank_line_before(text, position) {
        Some(BoundaryKind::Paragraph)
    } else if is_atx_heading(text, position) {
        Some(BoundaryKind::Heading)
    } else if is_list_item(text, position) {
        Some(BoundaryKind::ListItem)
    } else {
        None
    }
}

fn is_blank_line_before(text: &str, position: usize) -> bool {
    let bytes = text.as_bytes();
    let mut index = position - 1;
    while index > 0 {
        index -= 1;
        match bytes[index] {
            b' ' | b'\t' | b'\r' => {}
            b'\n' => return true,
            _ => return false,
        }
    }
    true
}

fn is_atx_heading(text: &str, position: usize) -> bool {
    let bytes = text.as_bytes();
    let mut hashes = 0usize;
    while position + hashes < bytes.len() && bytes[position + hashes] == b'#' && hashes < 6 {
        hashes += 1;
    }
    hashes > 0
        && position + hashes < bytes.len()
        && matches!(bytes[position + hashes], b' ' | b'\t' | b'\n' | b'\r')
}

fn is_list_item(text: &str, position: usize) -> bool {
    let bytes = text.as_bytes();
    if bytes.len() - position < 2 {
        return false;
    }
    if matches!(bytes[position], b'-' | b'*' | b'+') && bytes[position + 1] == b' ' {
        return true;
    }
    let mut digits = 0usize;
    while position + digits < bytes.len() && bytes[position + digits].is_ascii_digit() {
        digits += 1;
        if digits > 9 {
            return false;
        }
    }
    digits > 0
        && position + digits + 1 < bytes.len()
        && matches!(bytes[position + digits], b'.' | b')')
        && bytes[position + digits + 1] == b' '
}

fn is_sentence_end(previous: char, current: char) -> bool {
    match previous {
        '。' | '！' | '？' | '…' => true,
        '.' | '!' | '?' => current.is_whitespace(),
        _ => false,
    }
}

fn advance_chars(text: &str, start: usize, count: usize) -> usize {
    let mut byte_end = start;
    for current in text[start..].chars().take(count) {
        byte_end += current.len_utf8();
    }
    byte_end
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(max_chars: usize, overlap_chars: usize, max_utf8_bytes: usize) -> ChunkConfig {
        ChunkConfig::new(1, max_chars, overlap_chars, max_utf8_bytes).unwrap()
    }

    fn char_starts(text: &str) -> Vec<usize> {
        text.char_indices()
            .map(|(index, _)| index)
            .chain(std::iter::once(text.len()))
            .collect()
    }

    /// Ordered, bounded, non-empty char slices with no gaps, covering the
    /// whole text, each body matching its source interval.
    fn assert_complete_coverage(text: &str, config: &ChunkConfig, chunks: &[TextChunk]) {
        assert!(!chunks.is_empty());
        let starts = char_starts(text);
        for (index, chunk) in chunks.iter().enumerate() {
            assert_eq!(chunk.ordinal(), index);
            assert!(chunk.start_char() < chunk.end_char());
            assert!(chunk.end_char() - chunk.start_char() <= config.max_chars());
            assert!(chunk.text().len() <= config.max_utf8_bytes());
            assert_eq!(
                &text[starts[chunk.start_char()]..starts[chunk.end_char()]],
                chunk.text()
            );
        }
        assert_eq!(chunks[0].start_char(), 0);
        assert_eq!(chunks.last().unwrap().end_char(), text.chars().count());
        for pair in chunks.windows(2) {
            assert!(pair[1].start_char() > pair[0].start_char());
            assert!(pair[1].start_char() <= pair[0].end_char());
        }
    }

    fn assert_exact_overlap(chunks: &[TextChunk], overlap_chars: usize) {
        for pair in chunks.windows(2) {
            assert_eq!(pair[1].start_char(), pair[0].end_char() - overlap_chars);
            assert!(pair[1].start_char() > pair[0].start_char());
        }
    }

    #[test]
    fn config_rejects_malicious_limits() {
        for (version, max_chars, overlap, bytes) in [
            (0, 16, 4, 64),
            (1, 0, 0, 64),
            (1, 16, 16, 64),
            (1, 16, 17, 64),
            (1, 16, 4, 0),
        ] {
            let error = ChunkConfig::new(version, max_chars, overlap, bytes).unwrap_err();
            assert_eq!(error, SearchError::InvalidConfig("invalid chunk limits"));
        }
        assert!(ChunkConfig::new(1, 1, 0, 1).is_ok());
        assert!(ChunkConfig::new(u64::MAX, usize::MAX - 1, usize::MAX - 2, usize::MAX).is_ok());
    }

    #[test]
    fn config_getters_and_order() {
        let base = config(384, 48, 1536);
        assert_eq!(base.version(), 1);
        assert_eq!(base.max_chars(), 384);
        assert_eq!(base.overlap_chars(), 48);
        assert_eq!(base.max_utf8_bytes(), 1536);
        let newer = ChunkConfig::new(2, 384, 48, 1536).unwrap();
        assert!(base < newer);
        let larger = config(385, 48, 1536);
        assert!(base < larger);
    }

    #[test]
    fn extreme_overlap_with_tiny_byte_windows_never_overflows_or_loses_tail() {
        let text = "abcdef";
        let config = config(usize::MAX - 1, usize::MAX - 2, 1);
        let chunks = split_chunks(text, &config).unwrap();
        assert_complete_coverage(text, &config, &chunks);
        assert_eq!(
            chunks.iter().map(TextChunk::text).collect::<Vec<_>>(),
            ["a", "b", "c", "d", "e", "f"]
        );
        assert_eq!(chunks.last().unwrap().end_char(), 6);
    }

    #[test]
    fn empty_text_yields_no_chunks() {
        assert!(split_chunks("", &config(8, 2, 24)).unwrap().is_empty());
    }

    #[test]
    fn ascii_hard_cut_keeps_exact_overlap_and_tail() {
        let text = "abcdefghijklmnopqrstuvwxyz";
        let config = config(10, 3, 64);
        let chunks = split_chunks(text, &config).unwrap();
        assert_complete_coverage(text, &config, &chunks);
        assert_exact_overlap(&chunks, 3);
        let ranges: Vec<(usize, usize)> = chunks
            .iter()
            .map(|chunk| (chunk.start_char(), chunk.end_char()))
            .collect();
        assert_eq!(ranges, vec![(0, 10), (7, 17), (14, 24), (21, 26)]);
        assert_eq!(chunks.last().unwrap().text(), "vwxyz");
    }

    #[test]
    fn sentence_boundaries_preferred_over_hard_cut() {
        let text = "甲乙丙。丁戊己。庚辛壬。癸";
        let config = config(6, 0, 64);
        let chunks = split_chunks(text, &config).unwrap();
        assert_complete_coverage(text, &config, &chunks);
        let bodies: Vec<&str> = chunks.iter().map(|chunk| chunk.text()).collect();
        assert_eq!(bodies, vec!["甲乙丙。", "丁戊己。", "庚辛壬。癸"]);
    }

    #[test]
    fn paragraph_boundary_wins_over_later_list_boundaries() {
        let text = "第一段。\n第二段。\n\n## 标题行\n- 列表甲\n- 列表乙\n尾段内容。";
        let config = config(20, 0, 256);
        let chunks = split_chunks(text, &config).unwrap();
        assert_complete_coverage(text, &config, &chunks);
        let bodies: Vec<&str> = chunks.iter().map(|chunk| chunk.text()).collect();
        assert_eq!(
            bodies,
            vec![
                "第一段。\n第二段。\n\n",
                "## 标题行\n- 列表甲\n",
                "- 列表乙\n尾段内容。",
            ]
        );
    }

    #[test]
    fn heading_boundary_wins_over_later_list_boundaries() {
        let text = "介绍文字\n## 小节\n正文一行\n- 项目甲\n- 项目乙\n结束";
        let config = config(25, 0, 256);
        let chunks = split_chunks(text, &config).unwrap();
        assert_complete_coverage(text, &config, &chunks);
        let bodies: Vec<&str> = chunks.iter().map(|chunk| chunk.text()).collect();
        assert_eq!(
            bodies,
            vec!["介绍文字\n", "## 小节\n正文一行\n- 项目甲\n- 项目乙\n结束"]
        );
    }

    #[test]
    fn list_boundary_wins_over_earlier_sentence_boundaries() {
        let text = "一句。两句。\n- 甲项\n- 乙项";
        let config = config(12, 0, 64);
        let chunks = split_chunks(text, &config).unwrap();
        assert_complete_coverage(text, &config, &chunks);
        let bodies: Vec<&str> = chunks.iter().map(|chunk| chunk.text()).collect();
        assert_eq!(bodies, vec!["一句。两句。\n", "- 甲项\n- 乙项"]);
    }

    #[test]
    fn crlf_paragraph_boundary_is_recognized() {
        let text = "one\r\ntwo\r\n\r\nthree";
        let config = config(12, 0, 64);
        let chunks = split_chunks(text, &config).unwrap();
        assert_complete_coverage(text, &config, &chunks);
        let bodies: Vec<&str> = chunks.iter().map(|chunk| chunk.text()).collect();
        assert_eq!(bodies, vec!["one\r\ntwo\r\n\r\n", "three"]);
    }

    #[test]
    fn chinese_multibyte_respects_byte_budget_without_losing_tail() {
        let text = "中文编码与向量检索。切片必须完整。戊己庚辛。";
        let config = config(8, 2, 20);
        let chunks = split_chunks(text, &config).unwrap();
        assert_complete_coverage(text, &config, &chunks);
        assert_exact_overlap(&chunks, 2);
        assert!(chunks.iter().all(|chunk| chunk.text().len() <= 20));
        assert_eq!(chunks.last().unwrap().text(), "庚辛。");
    }

    #[test]
    fn single_char_over_byte_budget_is_rejected() {
        let error = split_chunks("中", &config(10, 2, 2)).unwrap_err();
        assert_eq!(error, SearchError::InputTooLong);
        let error = split_chunks("a中", &config(10, 2, 2)).unwrap_err();
        assert_eq!(error, SearchError::InputTooLong);
    }

    #[test]
    fn byte_bound_windows_degenerate_overlap_without_losing_text() {
        let text = "中文英";
        let config = config(2, 1, 3);
        let chunks = split_chunks(text, &config).unwrap();
        assert_complete_coverage(text, &config, &chunks);
        let bodies: Vec<&str> = chunks.iter().map(|chunk| chunk.text()).collect();
        assert_eq!(bodies, vec!["中", "文", "英"]);
    }

    #[test]
    fn multibyte_mixed_script_slices_stay_on_char_boundaries() {
        let text = "Hello world! 你好，世界。🚀👍🏽 mild text";
        let config = config(7, 2, 24);
        let chunks = split_chunks(text, &config).unwrap();
        assert_complete_coverage(text, &config, &chunks);
    }

    #[test]
    fn overlap_spans_slice_boundaries_for_ascii_and_cjk() {
        let text = "段落甲。段落乙。段落丙。段落丁。";
        let config = config(7, 3, 64);
        let chunks = split_chunks(text, &config).unwrap();
        assert_complete_coverage(text, &config, &chunks);
        assert_exact_overlap(&chunks, 3);
        for pair in chunks.windows(2) {
            let mut shared: Vec<char> = pair[0].text().chars().rev().take(3).collect();
            shared.reverse();
            let head: Vec<char> = pair[1].text().chars().take(3).collect();
            assert_eq!(shared, head);
        }
    }

    #[test]
    fn repeated_calls_are_deterministic() {
        let text = "确定性要求相同输入产生相同切片。\n\n第二段落在这里。";
        let config = config(10, 3, 30);
        let first = split_chunks(text, &config).unwrap();
        let second = split_chunks(text, &config).unwrap();
        assert_eq!(first, second);
        assert_complete_coverage(text, &config, &first);
    }

    #[test]
    fn borrowed_chunk_views_point_into_the_parent_text() {
        let text = "abcdefgh";
        let config = ChunkConfig::new(1, 4, 3, 64).unwrap();
        let mut chunks = chunk_views(text, &config);
        let first = chunks.next().unwrap().unwrap();
        let second = chunks.next().unwrap().unwrap();

        assert_eq!(first.text(), "abcd");
        assert_eq!(first.text().as_ptr(), text.as_ptr());
        assert_eq!(second.text(), "bcde");
        assert_eq!(second.text().as_ptr(), text[1..].as_ptr());
    }

    #[test]
    fn chunk_count_cap_rejects_unbounded_output() {
        let text = "a".repeat(1100);
        let error = split_chunks(&text, &config(2, 1, 64)).unwrap_err();
        assert_eq!(error, SearchError::InputTooLong);
    }

    #[test]
    fn debug_redacts_chunk_body() {
        let chunks = split_chunks("秘密内容", &config(8, 2, 64)).unwrap();
        let rendered = format!("{chunks:?}");
        assert!(!rendered.contains("秘密内容"));
        assert!(rendered.contains("[redacted]"));
    }
}
