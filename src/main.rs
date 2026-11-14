use std::{
    io::{self, BufWriter, Write},
    path::Path,
    process::ExitCode,
};

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 2 && args[0] == "hook" && args[1] == "session-start" {
        let _ = io::stdout().write_all(ferrindex::hook::session_start(io::stdin()).as_bytes());
        return ExitCode::SUCCESS;
    }
    let mut out = BufWriter::new(io::stdout().lock());
    ExitCode::from(ferrindex::query::run(
        &args,
        Path::new("."),
        &mut out,
        &mut io::stderr(),
    ))
}
