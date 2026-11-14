use std::{
    io::{self, BufWriter},
    path::Path,
    process::ExitCode,
};

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    let mut out = BufWriter::new(io::stdout().lock());
    ExitCode::from(ferrindex::query::run(
        &args,
        Path::new("."),
        &mut out,
        &mut io::stderr(),
    ))
}
