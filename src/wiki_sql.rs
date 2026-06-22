use crate::error::{err, Result};
use std::fs::File;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Command, Stdio};

pub fn for_insert_values<F>(
    path: &Path,
    table_name: &str,
    limit: Option<usize>,
    mut handle: F,
) -> Result<()>
where
    F: FnMut(Vec<String>) -> Result<()>,
{
    if !path.exists() {
        return Err(err(format!("missing dump file: {}", path.display())));
    }

    let mut child = Command::new("gzip")
        .arg("-dc")
        .arg(path)
        .stdout(Stdio::piped())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| err("failed to read gzip stdout"))?;
    let reader = BufReader::new(stdout);
    let insert_prefix = format!("INSERT INTO `{table_name}` VALUES ");
    let mut handled = 0usize;

    for line in reader.split(b'\n') {
        let line = line?;
        let line = String::from_utf8_lossy(&line);
        if !line.starts_with(&insert_prefix) {
            continue;
        }
        let values = line
            .strip_prefix(&insert_prefix)
            .unwrap_or(&line)
            .trim_end_matches(';');

        parse_insert_tuples(values, |fields| {
            if let Some(limit) = limit {
                if handled >= limit {
                    return Ok(());
                }
            }
            handle(fields)?;
            handled += 1;
            Ok(())
        })?;
    }

    let status = child.wait()?;
    if !status.success() {
        return Err(err(format!("gzip failed for {}", path.display())));
    }

    Ok(())
}

pub fn open_bzip2_or_plain_reader(path: &Path) -> Result<Box<dyn BufRead>> {
    if !path.exists() {
        return Err(err(format!("missing dump file: {}", path.display())));
    }
    if path.extension().and_then(|ext| ext.to_str()) == Some("bz2") {
        let mut child = Command::new("bzip2")
            .arg("-dc")
            .arg(path)
            .stdout(Stdio::piped())
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| err("failed to read bzip2 stdout"))?;
        return Ok(Box::new(BufReader::new(stdout)));
    }
    Ok(Box::new(BufReader::new(File::open(path)?)))
}

pub fn parse_insert_tuples<F>(input: &str, mut handle: F) -> Result<()>
where
    F: FnMut(Vec<String>) -> Result<()>,
{
    let bytes = input.as_bytes();
    let mut index = 0usize;

    while index < bytes.len() {
        while index < bytes.len() && (bytes[index] == b',' || bytes[index].is_ascii_whitespace()) {
            index += 1;
        }
        if index >= bytes.len() {
            break;
        }
        if bytes[index] != b'(' {
            return Err(err(format!("expected tuple at byte {index}")));
        }
        index += 1;

        let mut fields = Vec::new();
        let mut current = Vec::<u8>::new();
        let mut in_string = false;
        let mut is_null = false;

        while index < bytes.len() {
            let byte = bytes[index];
            if in_string {
                match byte {
                    b'\\' => {
                        index += 1;
                        if index >= bytes.len() {
                            break;
                        }
                        current.push(mysql_unescape_byte(bytes[index]));
                    }
                    b'\'' => in_string = false,
                    _ => current.push(byte),
                }
                index += 1;
                continue;
            }

            match byte {
                b'\'' => {
                    in_string = true;
                    index += 1;
                }
                b',' => {
                    fields.push(if is_null {
                        String::new()
                    } else {
                        field_to_string(&current)
                    });
                    current.clear();
                    is_null = false;
                    index += 1;
                }
                b')' => {
                    fields.push(if is_null {
                        String::new()
                    } else {
                        field_to_string(&current)
                    });
                    handle(fields)?;
                    index += 1;
                    break;
                }
                b'N' if input[index..].starts_with("NULL") => {
                    is_null = true;
                    index += 4;
                }
                _ => {
                    current.push(byte);
                    index += 1;
                }
            }
        }
    }

    Ok(())
}

fn mysql_unescape_byte(byte: u8) -> u8 {
    match byte {
        b'0' => b'\0',
        b'\'' => b'\'',
        b'"' => b'"',
        b'b' => 0x08,
        b'n' => b'\n',
        b'r' => b'\r',
        b't' => b'\t',
        b'Z' => 0x1a,
        b'\\' => b'\\',
        other => other,
    }
}

fn field_to_string(value: &[u8]) -> String {
    let trimmed = trim_ascii(value);
    String::from_utf8_lossy(trimmed).into_owned()
}

fn trim_ascii(value: &[u8]) -> &[u8] {
    let mut start = 0;
    let mut end = value.len();
    while start < end && value[start].is_ascii_whitespace() {
        start += 1;
    }
    while end > start && value[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    &value[start..end]
}

pub fn parse_u64(value: &str) -> Result<u64> {
    value
        .parse::<u64>()
        .map_err(|source| err(format!("expected u64 `{value}`: {source}")))
}

pub fn parse_i32(value: &str) -> Result<i32> {
    value
        .parse::<i32>()
        .map_err(|source| err(format!("expected i32 `{value}`: {source}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_mysql_insert_tuples() {
        let mut rows = Vec::new();
        parse_insert_tuples("(1,0,'A_B'),(2,0,'Tom\\'s'),(3,NULL,'x\\ny')", |fields| {
            rows.push(fields);
            Ok(())
        })
        .unwrap();

        assert_eq!(rows[0], vec!["1", "0", "A_B"]);
        assert_eq!(rows[1], vec!["2", "0", "Tom's"]);
        assert_eq!(rows[2], vec!["3", "", "x\ny"]);
    }
}
