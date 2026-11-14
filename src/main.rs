use std::{
    io::{self, BufWriter, Write},
    path::Path,
    process::ExitCode,
};

fn main() -> ExitCode {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.len() == 2 && args[0] == "hook" {
        let out = match args[1].to_str() {
            Some("session-start") => Some(ferrindex::hook::session_start(io::stdin())),
            Some("post-tool-use") => Some(ferrindex::hook::post_tool_use(io::stdin())),
            _ => None,
        };
        if let Some(out) = out {
            let _ = io::stdout().write_all(out.as_bytes());
            return ExitCode::SUCCESS;
        }
    }
    let mut out = BufWriter::new(io::stdout().lock());
    ExitCode::from(ferrindex::query::run(
        &args,
        Path::new("."),
        &mut out,
        &mut io::stderr(),
    ))
}
