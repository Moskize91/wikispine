use std::error::Error;
use std::fmt;

pub type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[derive(Debug)]
pub struct CliError(pub String);

impl fmt::Display for CliError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl Error for CliError {}

pub fn err(message: impl Into<String>) -> Box<dyn Error> {
    Box::new(CliError(message.into()))
}
