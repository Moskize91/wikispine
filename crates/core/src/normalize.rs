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
}

impl SurfaceNormalizer {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn reset(&mut self) {
        self.emitted_any = false;
        self.pending_space = None;
    }

    pub fn normalize_chunk(&mut self, value: &str) -> Vec<NormalizedChar> {
        let mut result = Vec::new();
        for item in normalize_chars_raw(value) {
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
    }
}

fn normalize_chars_raw(value: &str) -> Vec<NormAtom> {
    let atoms = source_atoms(value);
    let atoms = filter_deleted(atoms);
    let atoms = fold_separators(atoms);
    let atoms = nfkc_atoms(atoms);
    let atoms = case_fold_atoms(atoms);
    let atoms = filter_deleted_or_combining(atoms);
    let atoms = fold_separators(atoms);
    let atoms = nfd_atoms(atoms);
    let atoms = filter_deleted_or_combining(atoms);
    fold_separators(atoms)
}

fn source_atoms(value: &str) -> Vec<NormAtom> {
    let mut result = Vec::new();
    for (byte_index, original) in value.char_indices() {
        let original_start_utf16 = value[..byte_index].encode_utf16().count();
        let original_end_utf16 = original_start_utf16 + original.len_utf16();
        result.push(NormAtom {
            ch: original,
            original_start_utf16,
            original_end_utf16,
        });
    }
    result
}

fn filter_deleted(atoms: Vec<NormAtom>) -> Vec<NormAtom> {
    atoms
        .into_iter()
        .filter(|atom| !is_deleted(atom.ch))
        .collect()
}

fn filter_deleted_or_combining(atoms: Vec<NormAtom>) -> Vec<NormAtom> {
    atoms
        .into_iter()
        .filter(|atom| !is_deleted(atom.ch) && !is_combining_mark(atom.ch))
        .collect()
}

fn fold_separators(atoms: Vec<NormAtom>) -> Vec<NormAtom> {
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
}
