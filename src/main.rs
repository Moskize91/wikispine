extern crate alloc;

#[allow(dead_code, unused_imports)]
mod compact_ac;

mod ac_compile;
mod cli;
mod download;
mod error;
mod postprocess;
mod preprocess;
mod qid;
mod tsv;
mod wiki_sql;

use crate::error::Result;

fn main() -> Result<()> {
    cli::run(std::env::args().skip(1).collect())
}
