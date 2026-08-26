//! Every write the CLI makes, and the four macros the commands write through.

/// Writes to stdout, with a closed pipe treated as the end of the run.
///
/// `println!` panics when the write fails, and a reader that quits early
/// (`i18n-tasks-rs unused | head`, or `| more` and then `q`) closes the pipe
/// under the writer. Rust ignores `SIGPIPE`, so that write comes back as
/// `ErrorKind::BrokenPipe` rather than killing the process, and restoring the
/// default handler needs `libc` and an `unsafe` block the crate forbids. So the
/// output goes through here instead: a closed pipe ends the output quietly and
/// leaves the exit code alone, and `unused | head` still says 1.
pub(crate) fn write_out(args: std::fmt::Arguments) {
    write_to(&mut std::io::stdout().lock(), args, "stdout");
}

pub(crate) fn write_err(args: std::fmt::Arguments) {
    write_to(&mut std::io::stderr().lock(), args, "stderr");
}

fn write_to(w: &mut impl std::io::Write, args: std::fmt::Arguments, name: &str) {
    match w.write_fmt(args) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {}
        // A full disk, say. Say so once, on the other stream, and carry on.
        Err(e) => {
            let _ = std::io::Write::write_fmt(
                &mut std::io::stderr(),
                format_args!("i18n-tasks-rs: cannot write to {name}: {e}\n"),
            );
        }
    }
}

/// `print!`, `println!`, `eprint!` and `eprintln!`, routed through `write_out`
/// and `write_err`. The CLI uses these four and never the standard ones.
///
/// The bodies name `$crate::cli::out` rather than the imported functions, so a
/// caller only has to import the macro it uses.
macro_rules! out {
    ($($arg:tt)*) => { $crate::cli::out::write_out(format_args!($($arg)*)) };
}

macro_rules! outln {
    () => { $crate::cli::out::write_out(format_args!("\n")) };
    ($($arg:tt)*) => {
        $crate::cli::out::write_out(format_args!("{}\n", format_args!($($arg)*)))
    };
}

macro_rules! err {
    ($($arg:tt)*) => { $crate::cli::out::write_err(format_args!($($arg)*)) };
}

macro_rules! errln {
    ($($arg:tt)*) => {
        $crate::cli::out::write_err(format_args!("{}\n", format_args!($($arg)*)))
    };
}

pub(crate) use {err, errln, out, outln};
