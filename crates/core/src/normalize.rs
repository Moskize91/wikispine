use unicode_casefold::UnicodeCaseFold;
use unicode_general_category::{get_general_category, GeneralCategory};
use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};

pub const SURFACE_NORMALIZATION: &str = "wikispine-surface-normalization";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedChar {
    pub ch: char,
    pub original_start_utf16: usize,
    pub original_end_utf16: usize,
}

type NormAtom = NormalizedChar;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AtomKind {
    Char(char),
    LineBreak,
    ParagraphBreak,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LayoutAtom {
    kind: AtomKind,
    original_start_utf16: usize,
    original_end_utf16: usize,
}

impl NormalizedChar {
    fn replace(mut self, ch: char) -> Self {
        self.ch = ch;
        self
    }
}

pub fn normalize_surface_key(value: &str) -> Option<String> {
    let normalized = normalize_chars(value)
        .into_iter()
        .map(|item| item.ch)
        .collect::<String>();
    if normalized.is_empty() {
        None
    } else {
        Some(normalized)
    }
}

pub fn normalize_chars(value: &str) -> Vec<NormalizedChar> {
    let mut normalizer = SurfaceNormalizer::new();
    let result = normalizer.normalize_chunk(value);
    normalizer.finish();
    result
}

#[derive(Debug, Clone, Default)]
pub struct SurfaceNormalizer {
    emitted_any: bool,
    pending_space: Option<NormalizedChar>,
    source_offset_utf16: usize,
    pending_cr: Option<LayoutAtom>,
    pending_horizontal_space: Vec<LayoutAtom>,
    pending_line_break: Option<LayoutAtom>,
    at_line_start: bool,
    dehyphen_pending: Vec<LayoutAtom>,
    dehyphen_last_emitted_latin: bool,
}

impl SurfaceNormalizer {
    pub fn new() -> Self {
        Self {
            at_line_start: true,
            ..Self::default()
        }
    }

    pub fn reset(&mut self) {
        self.emitted_any = false;
        self.pending_space = None;
        self.source_offset_utf16 = 0;
        self.pending_cr = None;
        self.pending_horizontal_space.clear();
        self.pending_line_break = None;
        self.at_line_start = true;
        self.dehyphen_pending.clear();
        self.dehyphen_last_emitted_latin = false;
    }

    pub fn normalize_chunk(&mut self, value: &str) -> Vec<NormalizedChar> {
        let mut result = Vec::new();
        for item in self.normalize_chars_raw(value) {
            if item.ch == ' ' {
                if self.emitted_any {
                    self.pending_space = Some(match self.pending_space.take() {
                        Some(previous) => merge_atoms(' ', &[previous, item]).unwrap(),
                        None => item,
                    });
                }
                continue;
            }
            if let Some(space) = self.pending_space.take() {
                result.push(space);
            }
            result.push(item);
            self.emitted_any = true;
        }
        result
    }

    pub fn finish(&mut self) {
        self.pending_space = None;
        self.pending_cr = None;
        self.pending_horizontal_space.clear();
        self.pending_line_break = None;
        self.dehyphen_pending.clear();
    }

    fn normalize_chars_raw(&mut self, value: &str) -> Vec<NormAtom> {
        let atoms = self.source_atoms(value);
        let atoms = filter_deleted(atoms);
        let atoms = self.normalize_line_layout(atoms);
        let atoms = self.dehyphenate(atoms);
        normalize_layout_atoms(atoms)
    }

    fn source_atoms(&mut self, value: &str) -> Vec<LayoutAtom> {
        let base_offset = self.source_offset_utf16;
        self.source_offset_utf16 += value.encode_utf16().count();

        let mut result = Vec::new();
        let mut chars = value.chars().peekable();
        let mut original_start_utf16 = base_offset;

        if let Some(pending_cr) = self.pending_cr.take() {
            if chars.peek() == Some(&'\n') {
                chars.next();
                let original_end_utf16 = base_offset + '\n'.len_utf16();
                result.push(LayoutAtom {
                    kind: AtomKind::Char('\n'),
                    original_start_utf16: pending_cr.original_start_utf16,
                    original_end_utf16,
                });
                original_start_utf16 = original_end_utf16;
            } else {
                result.push(pending_cr);
            }
        }

        while let Some(original) = chars.next() {
            let mut original_end_utf16 = original_start_utf16 + original.len_utf16();
            let mut ch = original;
            if original == '\r' && chars.peek() == Some(&'\n') {
                chars.next();
                original_end_utf16 += '\n'.len_utf16();
                ch = '\n';
            } else if original == '\r' && chars.peek().is_none() {
                self.pending_cr = Some(LayoutAtom {
                    kind: AtomKind::Char('\r'),
                    original_start_utf16,
                    original_end_utf16,
                });
                original_start_utf16 = original_end_utf16;
                continue;
            }
            result.push(LayoutAtom {
                kind: AtomKind::Char(ch),
                original_start_utf16,
                original_end_utf16,
            });
            original_start_utf16 = original_end_utf16;
        }
        result
    }

    fn normalize_line_layout(&mut self, atoms: Vec<LayoutAtom>) -> Vec<LayoutAtom> {
        let mut result = Vec::new();

        for atom in atoms {
            match atom.kind {
                AtomKind::Char(ch) if is_line_break_char(ch) => {
                    self.pending_horizontal_space.clear();
                    let line_break = LayoutAtom {
                        kind: AtomKind::LineBreak,
                        original_start_utf16: atom.original_start_utf16,
                        original_end_utf16: atom.original_end_utf16,
                    };
                    self.pending_line_break = Some(match self.pending_line_break.take() {
                        Some(previous) => {
                            merge_layout_atoms(AtomKind::ParagraphBreak, &[previous, line_break])
                                .unwrap()
                        }
                        None => line_break,
                    });
                    self.at_line_start = true;
                }
                AtomKind::Char(ch) if is_horizontal_space_like(ch) => {
                    if !self.at_line_start {
                        self.pending_horizontal_space.push(atom);
                    }
                }
                _ => {
                    if let Some(line_break) = self.pending_line_break.take() {
                        result.push(line_break);
                    }
                    if !self.pending_horizontal_space.is_empty() {
                        result.push(
                            merge_layout_atoms(AtomKind::Char(' '), &self.pending_horizontal_space)
                                .unwrap(),
                        );
                        self.pending_horizontal_space.clear();
                    }
                    result.push(atom);
                    self.at_line_start = false;
                }
            }
        }

        result
    }

    fn dehyphenate(&mut self, atoms: Vec<LayoutAtom>) -> Vec<LayoutAtom> {
        let mut result = Vec::new();
        for atom in atoms {
            self.push_dehyphen_atom(atom, &mut result);
        }
        result
    }

    fn push_dehyphen_atom(&mut self, atom: LayoutAtom, result: &mut Vec<LayoutAtom>) {
        if self.dehyphen_pending.is_empty() {
            if self.dehyphen_last_emitted_latin && atom.kind.is_hyphen() {
                self.dehyphen_pending.push(atom);
            } else {
                self.emit_dehyphen_atom(atom, result);
            }
            return;
        }

        self.dehyphen_pending.push(atom);
        match self.dehyphen_pending.as_slice() {
            [hyphen, line_break]
                if hyphen.kind.is_hyphen() && line_break.kind == AtomKind::LineBreak => {}
            [hyphen, line_break, next]
                if hyphen.kind.is_hyphen()
                    && line_break.kind == AtomKind::LineBreak
                    && next.kind.is_latin_letter() =>
            {
                let next = next.clone();
                self.dehyphen_pending.clear();
                self.emit_dehyphen_atom(next, result);
            }
            _ => {
                let pending = std::mem::take(&mut self.dehyphen_pending);
                for atom in pending {
                    self.emit_dehyphen_atom(atom, result);
                }
            }
        }
    }

    fn emit_dehyphen_atom(&mut self, atom: LayoutAtom, result: &mut Vec<LayoutAtom>) {
        self.dehyphen_last_emitted_latin = atom.kind.is_latin_letter();
        result.push(atom);
    }
}

fn normalize_layout_atoms(atoms: Vec<LayoutAtom>) -> Vec<NormAtom> {
    let atoms = fold_separators(atoms);
    let atoms = nfkc_atoms(atoms);
    let atoms = case_fold_atoms(atoms);
    let atoms = filter_deleted_or_combining(atoms);
    let atoms = fold_normalized_separators(atoms);
    let atoms = nfd_atoms(atoms);
    let atoms = filter_deleted_or_combining(atoms);
    fold_normalized_separators(atoms)
}

fn filter_deleted(atoms: Vec<LayoutAtom>) -> Vec<LayoutAtom> {
    atoms
        .into_iter()
        .filter(|atom| !matches!(atom.kind, AtomKind::Char(ch) if is_deleted(ch)))
        .collect()
}

fn fold_separators(atoms: Vec<LayoutAtom>) -> Vec<NormAtom> {
    atoms
        .into_iter()
        .map(|atom| match atom.kind {
            AtomKind::Char(ch) if is_space_like(ch) || is_separator_like(ch) => NormAtom {
                ch: ' ',
                original_start_utf16: atom.original_start_utf16,
                original_end_utf16: atom.original_end_utf16,
            },
            AtomKind::Char(ch) => NormAtom {
                ch,
                original_start_utf16: atom.original_start_utf16,
                original_end_utf16: atom.original_end_utf16,
            },
            AtomKind::LineBreak | AtomKind::ParagraphBreak => NormAtom {
                ch: ' ',
                original_start_utf16: atom.original_start_utf16,
                original_end_utf16: atom.original_end_utf16,
            },
        })
        .collect()
}

fn filter_deleted_or_combining(atoms: Vec<NormAtom>) -> Vec<NormAtom> {
    atoms
        .into_iter()
        .filter(|atom| !is_deleted(atom.ch) && !is_combining_mark(atom.ch))
        .collect()
}

fn fold_normalized_separators(atoms: Vec<NormAtom>) -> Vec<NormAtom> {
    atoms
        .into_iter()
        .map(|atom| {
            if is_space_like(atom.ch) || is_separator_like(atom.ch) {
                atom.replace(' ')
            } else {
                atom
            }
        })
        .collect()
}

fn nfkc_atoms(atoms: Vec<NormAtom>) -> Vec<NormAtom> {
    atoms
        .into_iter()
        .flat_map(|atom| {
            let normalized = atom.ch.to_string().nfkc().collect::<Vec<_>>();
            fork_atom(atom, normalized)
        })
        .collect()
}

fn case_fold_atoms(atoms: Vec<NormAtom>) -> Vec<NormAtom> {
    atoms
        .into_iter()
        .flat_map(|atom| {
            let folded = atom.ch.to_string().case_fold().collect::<Vec<_>>();
            fork_atom(atom, folded)
        })
        .collect()
}

fn nfd_atoms(atoms: Vec<NormAtom>) -> Vec<NormAtom> {
    atoms
        .into_iter()
        .flat_map(|atom| {
            let decomposed = atom.ch.to_string().nfd().collect::<Vec<_>>();
            fork_atom(atom, decomposed)
        })
        .collect()
}

fn fork_atom<I>(atom: NormAtom, chars: I) -> Vec<NormAtom>
where
    I: IntoIterator<Item = char>,
{
    chars
        .into_iter()
        .map(|ch| NormAtom {
            ch,
            original_start_utf16: atom.original_start_utf16,
            original_end_utf16: atom.original_end_utf16,
        })
        .collect()
}

fn merge_atoms(ch: char, atoms: &[NormAtom]) -> Option<NormAtom> {
    let first = atoms.first()?;
    let last = atoms.last()?;
    Some(NormAtom {
        ch,
        original_start_utf16: first.original_start_utf16,
        original_end_utf16: last.original_end_utf16,
    })
}

fn merge_layout_atoms(kind: AtomKind, atoms: &[LayoutAtom]) -> Option<LayoutAtom> {
    let first = atoms.first()?;
    let last = atoms.last()?;
    Some(LayoutAtom {
        kind,
        original_start_utf16: first.original_start_utf16,
        original_end_utf16: last.original_end_utf16,
    })
}

impl AtomKind {
    fn is_latin_letter(self) -> bool {
        matches!(self, AtomKind::Char(ch) if ch.is_ascii_alphabetic())
    }

    fn is_hyphen(self) -> bool {
        matches!(
            self,
            AtomKind::Char('-')
                | AtomKind::Char('\u{2010}')
                | AtomKind::Char('\u{2011}')
                | AtomKind::Char('\u{2012}')
                | AtomKind::Char('\u{2013}')
                | AtomKind::Char('\u{2014}')
                | AtomKind::Char('\u{2212}')
        )
    }
}

fn is_line_break_char(ch: char) -> bool {
    matches!(ch, '\n' | '\r' | '\u{2028}' | '\u{2029}')
}

fn is_horizontal_space_like(ch: char) -> bool {
    is_space_like(ch) && !is_line_break_char(ch)
}

fn is_space_like(ch: char) -> bool {
    ch.is_whitespace()
        || matches!(
            ch,
            '\u{00a0}' | '\u{1680}' | '\u{180e}' | '\u{2000}'
                ..='\u{200a}' | '\u{2028}' | '\u{2029}' | '\u{202f}' | '\u{205f}' | '\u{3000}'
        )
}

fn is_deleted(ch: char) -> bool {
    matches!(
        ch,
        '\u{00ad}'
            | '\u{034f}'
            | '\u{061c}'
            | '\u{115f}'..='\u{1160}'
            | '\u{17b4}'..='\u{17b5}'
            | '\u{180b}'..='\u{180f}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{206f}'
            | '\u{3164}'
            | '\u{fe00}'..='\u{fe0f}'
            | '\u{feff}'
            | '\u{ffa0}'
            | '\u{1bca0}'..='\u{1bca3}'
            | '\u{1d173}'..='\u{1d17a}'
            | '\u{e0100}'..='\u{e01ef}'
    )
}

fn is_separator_like(ch: char) -> bool {
    if matches!(ch, '+' | '#' | '&') {
        return false;
    }
    if matches!(
        ch,
        '_' | '-'
            | '/'
            | '\\'
            | '|'
            | '.'
            | ','
            | ':'
            | ';'
            | '!'
            | '?'
            | '"'
            | '\''
            | '`'
            | '~'
            | '*'
            | '='
            | '<'
            | '>'
            | '('
            | ')'
            | '['
            | ']'
            | '{'
            | '}'
            | '·'
            | '•'
            | '・'
            | '。'
            | '，'
            | '、'
            | '：'
            | '；'
            | '！'
            | '？'
            | '「'
            | '」'
            | '『'
            | '』'
            | '《'
            | '》'
            | '〈'
            | '〉'
            | '（'
            | '）'
            | '【'
            | '】'
            | '［'
            | '］'
            | '｛'
            | '｝'
    ) {
        return true;
    }
    matches!(
        get_general_category(ch),
        GeneralCategory::ConnectorPunctuation
            | GeneralCategory::DashPunctuation
            | GeneralCategory::OpenPunctuation
            | GeneralCategory::ClosePunctuation
            | GeneralCategory::InitialPunctuation
            | GeneralCategory::FinalPunctuation
            | GeneralCategory::OtherPunctuation
            | GeneralCategory::SpaceSeparator
            | GeneralCategory::LineSeparator
            | GeneralCategory::ParagraphSeparator
            | GeneralCategory::Control
            | GeneralCategory::Format
            | GeneralCategory::Surrogate
            | GeneralCategory::PrivateUse
            | GeneralCategory::Unassigned
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_surface_keys_aggressively() {
        assert_eq!(
            normalize_surface_key(" Ａlàn＿Turing "),
            Some("alan turing".to_string())
        );
        assert_eq!(
            normalize_surface_key("Jean‑Paul Sartre"),
            Some("jean paul sartre".to_string())
        );
        assert_eq!(
            normalize_surface_key("西格蒙德·弗洛伊德"),
            Some("西格蒙德 弗洛伊德".to_string())
        );
        assert_eq!(normalize_surface_key("Café"), Some("cafe".to_string()));
        assert_eq!(normalize_surface_key("Straße"), Some("strasse".to_string()));
        assert_eq!(normalize_surface_key("C++"), Some("c++".to_string()));
        assert_eq!(normalize_surface_key("C#"), Some("c#".to_string()));
        assert_eq!(normalize_surface_key("R&B"), Some("r&b".to_string()));
        assert_eq!(
            normalize_surface_key("《北京大学》"),
            Some("北京大学".to_string())
        );
        assert_eq!(normalize_surface_key("\u{200b}\u{feff}"), None);
    }

    #[test]
    fn exposes_original_offsets() {
        let chars = normalize_chars("Ａ\u{200b}B");
        assert_eq!(
            chars,
            vec![
                NormalizedChar {
                    ch: 'a',
                    original_start_utf16: 0,
                    original_end_utf16: 1
                },
                NormalizedChar {
                    ch: 'b',
                    original_start_utf16: 2,
                    original_end_utf16: 3
                }
            ]
        );
    }

    #[test]
    fn preserves_expanded_atom_offsets() {
        let chars = normalize_chars("Straße");
        assert_eq!(
            chars.iter().map(|item| item.ch).collect::<String>(),
            "strasse"
        );
        assert_eq!(chars[4].ch, 's');
        assert_eq!(chars[4].original_start_utf16, 4);
        assert_eq!(chars[4].original_end_utf16, 5);
        assert_eq!(chars[5].ch, 's');
        assert_eq!(chars[5].original_start_utf16, 4);
        assert_eq!(chars[5].original_end_utf16, 5);
    }

    #[test]
    fn merged_space_covers_original_space_run() {
        let chars = normalize_chars("Apple   Pencil");
        assert_eq!(
            chars.iter().map(|item| item.ch).collect::<String>(),
            "apple pencil"
        );
        let space = &chars[5];
        assert_eq!(space.ch, ' ');
        assert_eq!(space.original_start_utf16, 5);
        assert_eq!(space.original_end_utf16, 8);
    }

    #[test]
    fn joins_line_break_hyphenated_latin_words() {
        let chars = normalize_chars("Pen-\ncil");
        assert_eq!(
            chars.iter().map(|item| item.ch).collect::<String>(),
            "pencil"
        );
        assert_eq!(chars.first().unwrap().original_start_utf16, 0);
        assert_eq!(chars.last().unwrap().original_end_utf16, 8);
        assert_eq!(chars[3].ch, 'c');
        assert_eq!(chars[3].original_start_utf16, 5);
    }

    #[test]
    fn keeps_inline_hyphen_as_separator() {
        assert_eq!(
            normalize_surface_key("well-known"),
            Some("well known".to_string())
        );
    }

    #[test]
    fn does_not_join_across_paragraph_breaks() {
        assert_eq!(
            normalize_surface_key("appl-\n\ne"),
            Some("appl e".to_string())
        );
        assert_eq!(
            normalize_surface_key("appl-\n   \ne"),
            Some("appl e".to_string())
        );
    }

    #[test]
    fn trims_physical_lines_before_folding_line_breaks() {
        let chars = normalize_chars("Apple   \n   Pencil");
        assert_eq!(
            chars.iter().map(|item| item.ch).collect::<String>(),
            "apple pencil"
        );
        let space = &chars[5];
        assert_eq!(space.ch, ' ');
        assert_eq!(space.original_start_utf16, 8);
        assert_eq!(space.original_end_utf16, 9);
    }

    #[test]
    fn treats_crlf_as_one_line_break_for_dehyphenation() {
        assert_eq!(
            normalize_surface_key("Pen-\r\ncil"),
            Some("pencil".to_string())
        );
    }

    #[test]
    fn preserves_separator_across_chunks() {
        let mut normalizer = SurfaceNormalizer::new();
        let first = normalizer.normalize_chunk("Alan-");
        let second = normalizer.normalize_chunk("Turing");
        assert_eq!(
            first
                .into_iter()
                .chain(second)
                .map(|item| item.ch)
                .collect::<String>(),
            "alan turing"
        );
    }

    #[test]
    fn preserves_chunk_leading_space_after_previous_word() {
        let mut normalizer = SurfaceNormalizer::new();
        let first = normalizer.normalize_chunk("Apple");
        let second = normalizer.normalize_chunk("  Pencil");
        let chars = first.into_iter().chain(second).collect::<Vec<_>>();
        assert_eq!(
            chars.iter().map(|item| item.ch).collect::<String>(),
            "apple pencil"
        );
        let space = &chars[5];
        assert_eq!(space.ch, ' ');
        assert_eq!(space.original_start_utf16, 5);
        assert_eq!(space.original_end_utf16, 7);
    }

    #[test]
    fn preserves_chunk_trailing_space_until_next_word() {
        let mut normalizer = SurfaceNormalizer::new();
        let first = normalizer.normalize_chunk("Apple  ");
        let second = normalizer.normalize_chunk("Pencil");
        let chars = first.into_iter().chain(second).collect::<Vec<_>>();
        assert_eq!(
            chars.iter().map(|item| item.ch).collect::<String>(),
            "apple pencil"
        );
        let space = &chars[5];
        assert_eq!(space.ch, ' ');
        assert_eq!(space.original_start_utf16, 5);
        assert_eq!(space.original_end_utf16, 7);
    }

    #[test]
    fn preserves_chunk_trailing_line_break_until_next_word() {
        let mut normalizer = SurfaceNormalizer::new();
        let first = normalizer.normalize_chunk("Apple\n");
        let second = normalizer.normalize_chunk("Pencil");
        let chars = first.into_iter().chain(second).collect::<Vec<_>>();
        assert_eq!(
            chars.iter().map(|item| item.ch).collect::<String>(),
            "apple pencil"
        );
        let space = &chars[5];
        assert_eq!(space.ch, ' ');
        assert_eq!(space.original_start_utf16, 5);
        assert_eq!(space.original_end_utf16, 6);
    }

    #[test]
    fn dehyphenates_across_chunks() {
        let mut normalizer = SurfaceNormalizer::new();
        let first = normalizer.normalize_chunk("Pen-");
        let second = normalizer.normalize_chunk("\ncil");
        let chars = first.into_iter().chain(second).collect::<Vec<_>>();
        assert_eq!(
            chars.iter().map(|item| item.ch).collect::<String>(),
            "pencil"
        );
        assert_eq!(chars[3].ch, 'c');
        assert_eq!(chars[3].original_start_utf16, 5);
    }

    #[test]
    fn treats_split_crlf_as_one_line_break_across_chunks() {
        let mut normalizer = SurfaceNormalizer::new();
        let first = normalizer.normalize_chunk("Pen-\r");
        let second = normalizer.normalize_chunk("\ncil");
        let chars = first.into_iter().chain(second).collect::<Vec<_>>();
        assert_eq!(
            chars.iter().map(|item| item.ch).collect::<String>(),
            "pencil"
        );
        assert_eq!(chars[3].ch, 'c');
        assert_eq!(chars[3].original_start_utf16, 6);
    }
}
