use crate::error::{err, Result};

pub const QID_FLAG_DISAMBIGUATION: u32 = 1;
pub const WIKIDATA_DISAMBIGUATION_QID: u32 = 4_167_410;

pub fn parse_qid(value: &str, line_number: usize) -> Result<u32> {
    let digits = value
        .strip_prefix('Q')
        .ok_or_else(|| err(format!("invalid QID `{value}` at line {line_number}")))?;
    digits.parse::<u32>().map_err(|source| {
        err(format!(
            "QID `{value}` exceeds runtime u32 encoding at line {line_number}: {source}"
        ))
    })
}

pub fn qid_number_from_str(value: &str) -> Option<u32> {
    value.strip_prefix('Q')?.parse::<u32>().ok()
}
