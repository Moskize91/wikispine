mod cli;
mod core;
mod server;

use std::env;

pub type Result<T> = std::result::Result<T, RuntimeError>;

#[derive(Debug)]
pub struct RuntimeError(String);

impl RuntimeError {
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for RuntimeError {}

impl From<std::io::Error> for RuntimeError {
    fn from(source: std::io::Error) -> Self {
        Self(source.to_string())
    }
}

impl From<serde_json::Error> for RuntimeError {
    fn from(source: serde_json::Error) -> Self {
        Self(source.to_string())
    }
}

#[tokio::main]
async fn main() {
    if let Err(error) = cli::run(env::args().skip(1).collect()).await {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}
