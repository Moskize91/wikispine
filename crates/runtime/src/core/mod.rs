use crate::{Result, RuntimeError};
use memmap2::{Mmap, MmapOptions};
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::num::NonZeroU32;
use std::path::Path;
use wikispine_core::normalize::{NormalizedChar, SurfaceNormalizer, SURFACE_NORMALIZATION};

const ROOT_STATE_ID: u32 = 0;
const QID_FLAG_DISAMBIGUATION: u32 = 1;
const LEGACY_RUNTIME_MAX_SURFACE_LEN: usize = 388;

#[derive(Debug)]
pub struct RuntimeDataset {
    pub manifest: Manifest,
    shards: Vec<AutomatonShard>,
    surface_qid_index: MmapTable,
    surface_qid_values: MmapTable,
    qid_numbers: MmapTable,
    qid_flags: MmapTable,
}

impl RuntimeDataset {
    pub fn open(root: &Path) -> Result<Self> {
        let manifest_path = root.join("manifest.json");
        let manifest = serde_json::from_reader::<_, Manifest>(File::open(&manifest_path)?)?;
        if manifest.format != "wikispine-runtime-v1" {
            return Err(RuntimeError::new(format!(
                "unsupported runtime format: {}",
                manifest.format
            )));
        }
        if manifest.endian != "little" || manifest.mode != "charwise" {
            return Err(RuntimeError::new("unsupported runtime dataset encoding"));
        }
        if manifest.surface_normalization != SURFACE_NORMALIZATION {
            return Err(RuntimeError::new(format!(
                "unsupported surface normalization: {}",
                manifest.surface_normalization
            )));
        }

        let mut shards = Vec::with_capacity(manifest.automaton_shards.len());
        for shard in &manifest.automaton_shards {
            shards.push(AutomatonShard::open(root, shard)?);
        }

        Ok(Self {
            surface_qid_index: MmapTable::open(
                &root.join(&manifest.files.surface_qid_index),
                8,
                manifest.surface_count,
            )?,
            surface_qid_values: MmapTable::open(
                &root.join(&manifest.files.surface_qid_values),
                4,
                manifest.surface_qid_value_count,
            )?,
            qid_numbers: MmapTable::open(
                &root.join(&manifest.files.qid_numbers),
                4,
                manifest.qid_count,
            )?,
            qid_flags: MmapTable::open(
                &root.join(&manifest.files.qid_flags),
                4,
                manifest.qid_count,
            )?,
            manifest,
            shards,
        })
    }

    pub fn for_each_match<F>(&self, text: &str, options: &MatchOptions, mut on_match: F)
    where
        F: FnMut(TextMatch) -> bool,
    {
        let mut session = MatchSession::new(self.shard_count(), options.clone());
        for events in [session.process_chunk(text, self), session.finish()] {
            for event in events {
                let ServerEvent::Match { r#match } = event else {
                    continue;
                };
                if !on_match(r#match) {
                    return;
                }
            }
        }
    }

    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }

    fn qids_for_surface(&self, surface_id: u32, options: &MatchOptions) -> Vec<QidCandidate> {
        let Some((offset, len)) = self.surface_qid_range(surface_id) else {
            return Vec::new();
        };
        let mut candidates = Vec::with_capacity(len as usize);
        for index in offset..offset + len {
            let Some(qid) = self.surface_qid_values.u32_at(index as usize) else {
                continue;
            };
            let flags = self.flags_for_qid(qid).unwrap_or(0);
            let disambiguation = flags & QID_FLAG_DISAMBIGUATION != 0;
            if !options.include_disambiguation && disambiguation {
                continue;
            }
            candidates.push(QidCandidate {
                qid: format!("Q{qid}"),
                qid_number: qid,
                disambiguation,
            });
            if options
                .max_candidates_per_surface
                .is_some_and(|max| candidates.len() >= max)
            {
                break;
            }
        }
        candidates
    }

    fn surface_qid_range(&self, surface_id: u32) -> Option<(u32, u32)> {
        let index = surface_id as usize * 2;
        Some((
            self.surface_qid_index.u32_at(index)?,
            self.surface_qid_index.u32_at(index + 1)?,
        ))
    }

    fn flags_for_qid(&self, qid: u32) -> Option<u32> {
        let mut low = 0usize;
        let mut high = self.manifest.qid_count;
        while low < high {
            let mid = low + (high - low) / 2;
            let value = self.qid_numbers.u32_at(mid)?;
            match value.cmp(&qid) {
                std::cmp::Ordering::Less => low = mid + 1,
                std::cmp::Ordering::Equal => return self.qid_flags.u32_at(mid),
                std::cmp::Ordering::Greater => high = mid,
            }
        }
        None
    }
}

#[derive(Debug)]
pub struct MatchSession {
    shard_states: Vec<u32>,
    options: MatchOptions,
    normalizer: SurfaceNormalizer,
    pub offset_utf16: usize,
    normalized_offset_utf16: usize,
    normalized_original_starts: Vec<usize>,
    normalized_original_ends: Vec<usize>,
    normalized_chars_start: Vec<Option<char>>,
    normalized_chars_end: Vec<Option<char>>,
    pending_matches: Vec<PendingMatch>,
    pub match_count: usize,
}

impl MatchSession {
    pub fn new(shard_count: usize, options: MatchOptions) -> Self {
        Self {
            shard_states: vec![ROOT_STATE_ID; shard_count],
            options,
            normalizer: SurfaceNormalizer::new(),
            offset_utf16: 0,
            normalized_offset_utf16: 0,
            normalized_original_starts: Vec::new(),
            normalized_original_ends: Vec::new(),
            normalized_chars_start: Vec::new(),
            normalized_chars_end: Vec::new(),
            pending_matches: Vec::new(),
            match_count: 0,
        }
    }

    pub fn reset(&mut self) {
        self.shard_states.fill(ROOT_STATE_ID);
        self.normalizer.reset();
        self.offset_utf16 = 0;
        self.normalized_offset_utf16 = 0;
        self.normalized_original_starts.clear();
        self.normalized_original_ends.clear();
        self.normalized_chars_start.clear();
        self.normalized_chars_end.clear();
        self.pending_matches.clear();
        self.match_count = 0;
    }

    pub fn process_chunk(&mut self, chunk: &str, dataset: &RuntimeDataset) -> Vec<ServerEvent> {
        let normalized = self.normalizer.normalize_chunk(chunk);
        let mut matches = Vec::new();
        if let Some(first) = normalized.first() {
            resolve_pending_matches(&mut self.pending_matches, Some(first.ch), &mut matches);
        }
        let mut chunk_pending_matches = Vec::<PendingMatch>::new();
        let mut context = ShardScanContext {
            normalized_base_offset: self.normalized_offset_utf16,
            normalized_original_starts: &mut self.normalized_original_starts,
            normalized_original_ends: &mut self.normalized_original_ends,
            normalized_chars_start: &mut self.normalized_chars_start,
            normalized_chars_end: &mut self.normalized_chars_end,
            dataset,
            options: &self.options,
            matches: &mut matches,
            pending_matches: &mut chunk_pending_matches,
        };
        for (shard_index, shard) in dataset.shards.iter().enumerate() {
            let state_id = self
                .shard_states
                .get_mut(shard_index)
                .expect("session shard states match runtime shards");
            shard.find_matches_from_state(&normalized, *state_id, &mut context);
            *state_id = shard.advance_state(&normalized, *state_id);
        }
        self.normalized_offset_utf16 += normalized
            .iter()
            .map(|item| item.ch.len_utf16())
            .sum::<usize>();
        self.offset_utf16 += chunk.encode_utf16().count();
        resolve_chunk_pending_matches(
            chunk_pending_matches,
            &self.normalized_chars_start,
            &mut matches,
            &mut self.pending_matches,
        );
        trim_original_end_map(
            &mut self.normalized_original_starts,
            &mut self.normalized_original_ends,
            &mut self.normalized_chars_start,
            &mut self.normalized_chars_end,
            self.normalized_offset_utf16,
        );
        matches.sort_by_key(|matched| (matched.start, matched.end, matched.surface_id));
        self.match_count += matches.len();
        matches
            .into_iter()
            .map(|matched| ServerEvent::Match { r#match: matched })
            .collect()
    }

    pub fn finish(&mut self) -> Vec<ServerEvent> {
        self.normalizer.finish();
        let mut matches = Vec::new();
        resolve_pending_matches(&mut self.pending_matches, None, &mut matches);
        matches.sort_by_key(|matched| (matched.start, matched.end, matched.surface_id));
        self.match_count += matches.len();
        matches
            .into_iter()
            .map(|matched| ServerEvent::Match { r#match: matched })
            .collect()
    }
}

struct ShardScanContext<'a> {
    normalized_base_offset: usize,
    normalized_original_starts: &'a mut Vec<usize>,
    normalized_original_ends: &'a mut Vec<usize>,
    normalized_chars_start: &'a mut Vec<Option<char>>,
    normalized_chars_end: &'a mut Vec<Option<char>>,
    dataset: &'a RuntimeDataset,
    options: &'a MatchOptions,
    matches: &'a mut Vec<TextMatch>,
    pending_matches: &'a mut Vec<PendingMatch>,
}

#[derive(Debug)]
struct PendingMatch {
    normalized_end: usize,
    matched: TextMatch,
}

#[derive(Debug)]
struct AutomatonShard {
    pub shard_id: usize,
    states: MmapTable,
    char_code_map: MmapTable,
    state_outputs: MmapTable,
}

impl AutomatonShard {
    fn open(root: &Path, manifest: &AutomatonShardManifest) -> Result<Self> {
        let files = &manifest.files;
        Ok(Self {
            shard_id: manifest.shard_id,
            states: MmapTable::open(&root.join(&files.states), 16, manifest.states_len)?,
            char_code_map: MmapTable::open(
                &root.join(&files.char_code_map),
                4,
                manifest.mapper_table_len,
            )?,
            state_outputs: MmapTable::open(
                &root.join(&files.state_outputs),
                12,
                manifest.state_output_count,
            )?,
        })
    }

    fn find_matches_from_state(
        &self,
        text: &[NormalizedChar],
        mut state_id: u32,
        context: &mut ShardScanContext,
    ) {
        for (end, item) in NormalizedCharEndIterator::new(text) {
            state_id = self.next_state_id(state_id, item.ch);
            let normalized_end = context.normalized_base_offset + end;
            let normalized_start = normalized_end - item.ch.len_utf16();
            ensure_original_map_len(context.normalized_original_starts, normalized_end);
            ensure_original_map_len(context.normalized_original_ends, normalized_end);
            ensure_char_map_len(context.normalized_chars_start, normalized_end);
            ensure_char_map_len(context.normalized_chars_end, normalized_end);
            let original_start = item.original_start_utf16;
            let original_end = item.original_end_utf16;
            context.normalized_original_starts[normalized_start] = original_start;
            context.normalized_original_ends[normalized_end] = original_end;
            context.normalized_chars_start[normalized_start] = Some(item.ch);
            context.normalized_chars_end[normalized_end] = Some(item.ch);
            self.push_outputs_at_state(state_id, normalized_end, context);
        }
    }

    fn advance_state(&self, text: &[NormalizedChar], mut state_id: u32) -> u32 {
        for item in text {
            state_id = self.next_state_id(state_id, item.ch);
        }
        state_id
    }

    fn push_outputs_at_state(
        &self,
        state_id: u32,
        normalized_end: usize,
        context: &mut ShardScanContext,
    ) {
        let mut output_pos = self.state(state_id).and_then(|state| state.output_pos);
        while let Some(position) = output_pos {
            let Some(output) = self.output(position) else {
                break;
            };
            output_pos = output.parent;
            self.push_match(normalized_end, output, context);
        }
    }

    fn push_match(
        &self,
        normalized_end: usize,
        output: StateOutput,
        context: &mut ShardScanContext,
    ) {
        if let Some(matched) = self.build_match(normalized_end, output, context) {
            let needs_right_boundary = context
                .normalized_chars_end
                .get(normalized_end)
                .and_then(|ch| *ch)
                .is_some_and(is_ascii_word);
            if needs_right_boundary {
                context.pending_matches.push(PendingMatch {
                    normalized_end,
                    matched,
                });
            } else {
                context.matches.push(matched);
            }
        }
    }

    fn build_match(
        &self,
        normalized_end: usize,
        output: StateOutput,
        context: &ShardScanContext,
    ) -> Option<TextMatch> {
        let length = output.utf16_len as usize;
        if length > normalized_end {
            return None;
        }
        let normalized_start = normalized_end - length;
        let start = *context.normalized_original_starts.get(normalized_start)?;
        let end = *context.normalized_original_ends.get(normalized_end)?;
        if !left_boundary_allows_match(
            normalized_start,
            context.normalized_chars_start,
            context.normalized_chars_end,
        ) {
            return None;
        }
        let qids = context
            .dataset
            .qids_for_surface(output.surface_id, context.options);
        if qids.is_empty() {
            return None;
        }
        Some(TextMatch {
            start,
            end,
            surface_id: output.surface_id,
            shard_id: self.shard_id,
            qids,
        })
    }

    fn next_state_id(&self, mut state_id: u32, character: char) -> u32 {
        let Some(mapped) = self.mapped_code(character) else {
            return ROOT_STATE_ID;
        };
        loop {
            if let Some(child) = self.child_index(state_id, mapped) {
                return child;
            }
            if state_id == ROOT_STATE_ID {
                return ROOT_STATE_ID;
            }
            let Some(state) = self.state(state_id) else {
                return ROOT_STATE_ID;
            };
            state_id = state.fail;
        }
    }

    fn child_index(&self, state_id: u32, mapped: u32) -> Option<u32> {
        let base = self.state(state_id)?.base?;
        let child = base.get() ^ mapped;
        let state = self.state(child)?;
        if state.check == state_id {
            Some(child)
        } else {
            None
        }
    }

    fn mapped_code(&self, character: char) -> Option<u32> {
        let codepoint = character as u32 as usize;
        let mapped = self.char_code_map.u32_at(codepoint)?;
        if mapped == u32::MAX {
            None
        } else {
            Some(mapped)
        }
    }

    fn state(&self, state_id: u32) -> Option<StateRecord> {
        let index = state_id as usize * 4;
        Some(StateRecord {
            base: NonZeroU32::new(self.states.u32_at(index)?),
            check: self.states.u32_at(index + 1)?,
            fail: self.states.u32_at(index + 2)?,
            output_pos: NonZeroU32::new(self.states.u32_at(index + 3)?),
        })
    }

    fn output(&self, output_pos: NonZeroU32) -> Option<StateOutput> {
        let index = (output_pos.get() - 1) as usize * 3;
        Some(StateOutput {
            surface_id: self.state_outputs.u32_at(index)?,
            utf16_len: self.state_outputs.u32_at(index + 1)?,
            parent: NonZeroU32::new(self.state_outputs.u32_at(index + 2)?),
        })
    }
}

fn ensure_original_map_len(map: &mut Vec<usize>, normalized_end: usize) {
    if map.len() <= normalized_end {
        map.resize(normalized_end + 1, 0);
    }
}

fn ensure_char_map_len(map: &mut Vec<Option<char>>, normalized_end: usize) {
    if map.len() <= normalized_end {
        map.resize(normalized_end + 1, None);
    }
}

fn trim_original_end_map(
    starts: &mut [usize],
    ends: &mut [usize],
    chars_start: &mut [Option<char>],
    chars_end: &mut [Option<char>],
    normalized_offset_utf16: usize,
) {
    const KEEP_UTF16: usize = 4096;
    if normalized_offset_utf16 > KEEP_UTF16 && ends.len() > KEEP_UTF16 * 2 {
        let remove_until = normalized_offset_utf16 - KEEP_UTF16;
        for index in 0..remove_until.min(ends.len()) {
            if let Some(start) = starts.get_mut(index) {
                *start = 0;
            }
            ends[index] = 0;
            if let Some(ch) = chars_start.get_mut(index) {
                *ch = None;
            }
            if let Some(ch) = chars_end.get_mut(index) {
                *ch = None;
            }
        }
    }
}

fn left_boundary_allows_match(
    normalized_start: usize,
    chars_start: &[Option<char>],
    chars_end: &[Option<char>],
) -> bool {
    let Some(first) = chars_start.get(normalized_start).and_then(|ch| *ch) else {
        return false;
    };
    if !is_ascii_word(first) {
        return true;
    }
    !chars_end
        .get(normalized_start)
        .and_then(|ch| *ch)
        .is_some_and(is_ascii_word)
}

fn resolve_chunk_pending_matches(
    pending: Vec<PendingMatch>,
    chars_start: &[Option<char>],
    matches: &mut Vec<TextMatch>,
    session_pending: &mut Vec<PendingMatch>,
) {
    for item in pending {
        match chars_start.get(item.normalized_end).and_then(|ch| *ch) {
            Some(next) if is_ascii_word(next) => {}
            Some(_) => matches.push(item.matched),
            None => session_pending.push(item),
        }
    }
}

fn resolve_pending_matches(
    pending: &mut Vec<PendingMatch>,
    next_char: Option<char>,
    matches: &mut Vec<TextMatch>,
) {
    let should_emit = !next_char.is_some_and(is_ascii_word);
    for item in pending.drain(..) {
        if should_emit {
            matches.push(item.matched);
        }
    }
}

fn is_ascii_word(ch: char) -> bool {
    ch.is_ascii_alphanumeric()
}

#[derive(Debug)]
struct MmapTable {
    mmap: Mmap,
    record_bytes: usize,
    record_count: usize,
}

impl MmapTable {
    fn open(path: &Path, record_bytes: usize, record_count: usize) -> Result<Self> {
        let file = File::open(path)?;
        let actual_len = file.metadata()?.len() as usize;
        let expected_len = record_bytes
            .checked_mul(record_count)
            .ok_or_else(|| RuntimeError::new(format!("table size overflow: {}", path.display())))?;
        if actual_len != expected_len {
            return Err(RuntimeError::new(format!(
                "unexpected table size for {}: expected {}, got {}",
                path.display(),
                expected_len,
                actual_len
            )));
        }
        let mmap = unsafe { MmapOptions::new().map(&file)? };
        Ok(Self {
            mmap,
            record_bytes,
            record_count,
        })
    }

    fn u32_at(&self, index: usize) -> Option<u32> {
        if index >= self.record_count * (self.record_bytes / 4) {
            return None;
        }
        let offset = index.checked_mul(4)?;
        let bytes = self.mmap.get(offset..offset + 4)?;
        Some(u32::from_le_bytes(bytes.try_into().ok()?))
    }
}

#[derive(Debug, Clone, Copy)]
struct StateRecord {
    base: Option<NonZeroU32>,
    check: u32,
    fail: u32,
    output_pos: Option<NonZeroU32>,
}

#[derive(Debug, Clone, Copy)]
struct StateOutput {
    pub surface_id: u32,
    utf16_len: u32,
    parent: Option<NonZeroU32>,
}

struct NormalizedCharEndIterator<'a> {
    inner: std::slice::Iter<'a, NormalizedChar>,
    utf16_pos: usize,
}

impl<'a> NormalizedCharEndIterator<'a> {
    fn new(text: &'a [NormalizedChar]) -> Self {
        Self {
            inner: text.iter(),
            utf16_pos: 0,
        }
    }
}

impl<'a> Iterator for NormalizedCharEndIterator<'a> {
    type Item = (usize, &'a NormalizedChar);

    fn next(&mut self) -> Option<Self::Item> {
        let item = self.inner.next()?;
        self.utf16_pos += item.ch.len_utf16();
        Some((self.utf16_pos, item))
    }
}

#[derive(Debug, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub surface_normalization: String,
    endian: String,
    mode: String,
    pub surface_count: usize,
    surface_qid_value_count: usize,
    #[serde(default = "legacy_runtime_max_surface_len")]
    pub max_surface_char_len: usize,
    #[serde(default = "legacy_runtime_max_surface_len")]
    pub max_surface_utf16_len: usize,
    pub qid_count: usize,
    pub automaton_shard_count: usize,
    automaton_shards: Vec<AutomatonShardManifest>,
    files: RuntimeFiles,
}

#[derive(Debug, Deserialize)]
struct AutomatonShardManifest {
    pub shard_id: usize,
    states_len: usize,
    mapper_table_len: usize,
    state_output_count: usize,
    files: AutomatonShardFiles,
}

#[derive(Debug, Deserialize)]
struct AutomatonShardFiles {
    char_code_map: String,
    states: String,
    state_outputs: String,
}

#[derive(Debug, Deserialize)]
struct RuntimeFiles {
    surface_qid_index: String,
    surface_qid_values: String,
    qid_numbers: String,
    qid_flags: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MatchOptions {
    #[serde(default = "default_include_disambiguation")]
    pub include_disambiguation: bool,
    pub max_candidates_per_surface: Option<usize>,
}

impl Default for MatchOptions {
    fn default() -> Self {
        Self {
            include_disambiguation: true,
            max_candidates_per_surface: None,
        }
    }
}

fn default_include_disambiguation() -> bool {
    true
}

fn legacy_runtime_max_surface_len() -> usize {
    LEGACY_RUNTIME_MAX_SURFACE_LEN
}

#[derive(Debug, Serialize)]
#[serde(tag = "type")]
pub enum ServerEvent {
    #[serde(rename = "match")]
    Match { r#match: TextMatch },
    #[serde(rename = "done")]
    Done { stats: MatchStats },
    #[serde(rename = "interrupted")]
    Interrupted { reason: String },
}

#[derive(Debug, Serialize)]
pub struct MatchStats {
    pub matches: usize,
}

#[derive(Debug, Serialize)]
pub struct TextMatch {
    pub start: usize,
    pub end: usize,
    pub surface_id: u32,
    pub shard_id: usize,
    pub qids: Vec<QidCandidate>,
}

#[derive(Debug, Serialize)]
pub struct QidCandidate {
    pub qid: String,
    pub qid_number: u32,
    pub disambiguation: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn left_boundary_rejects_ascii_word_continuation() {
        let (starts, ends) = char_maps("pineapple");

        assert!(!left_boundary_allows_match(4, &starts, &ends));
    }

    #[test]
    fn left_boundary_allows_cjk_ascii_boundary() {
        let (starts, ends) = char_maps("苹果apple");

        assert!(left_boundary_allows_match(2, &starts, &ends));
    }

    #[test]
    fn right_boundary_rejects_ascii_word_continuation() {
        let mut pending = vec![pending_match(3)];
        let mut matches = Vec::new();

        resolve_pending_matches(&mut pending, Some('c'), &mut matches);

        assert!(pending.is_empty());
        assert!(matches.is_empty());
    }

    #[test]
    fn right_boundary_allows_cjk_and_eof_boundaries() {
        let mut pending = vec![pending_match(5)];
        let mut matches = Vec::new();
        resolve_pending_matches(&mut pending, Some('壳'), &mut matches);
        assert_eq!(matches.len(), 1);

        pending.push(pending_match(5));
        resolve_pending_matches(&mut pending, None, &mut matches);
        assert_eq!(matches.len(), 2);
    }

    #[test]
    fn manifest_uses_legacy_max_surface_len_when_fields_are_missing() {
        let manifest = serde_json::from_str::<Manifest>(
            r#"{
                "format": "wikispine-runtime-v1",
                "surface_normalization": "wikispine-surface-normalization",
                "endian": "little",
                "mode": "charwise",
                "surface_count": 1,
                "surface_qid_value_count": 1,
                "qid_count": 1,
                "automaton_shard_count": 0,
                "automaton_shards": [],
                "files": {
                    "surface_qid_index": "surfaces/surface_qid_index.bin",
                    "surface_qid_values": "surfaces/surface_qid_values.bin",
                    "qid_numbers": "qids/qid_numbers.bin",
                    "qid_flags": "qids/qid_flags.bin"
                }
            }"#,
        )
        .unwrap();

        assert_eq!(
            manifest.max_surface_char_len,
            LEGACY_RUNTIME_MAX_SURFACE_LEN
        );
        assert_eq!(
            manifest.max_surface_utf16_len,
            LEGACY_RUNTIME_MAX_SURFACE_LEN
        );
    }

    fn char_maps(value: &str) -> (Vec<Option<char>>, Vec<Option<char>>) {
        let mut starts = Vec::new();
        let mut ends = Vec::new();
        let mut position = 0usize;
        for ch in value.chars() {
            let end = position + ch.len_utf16();
            if starts.len() <= end {
                starts.resize(end + 1, None);
                ends.resize(end + 1, None);
            }
            starts[position] = Some(ch);
            ends[end] = Some(ch);
            position = end;
        }
        (starts, ends)
    }

    fn pending_match(normalized_end: usize) -> PendingMatch {
        PendingMatch {
            normalized_end,
            matched: TextMatch {
                start: 0,
                end: 1,
                surface_id: 1,
                shard_id: 0,
                qids: Vec::new(),
            },
        }
    }
}
