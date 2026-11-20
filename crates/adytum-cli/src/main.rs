mod args;
mod commands;

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use adytum::{Drivers, SystemDrivers};
use serde_json::{json, Value};

use args::USAGE;

struct Context {
    drivers: Arc<dyn Drivers>,
    home: PathBuf,
}

struct Failure {
    pub code: String,
    pub message: String,
    pub fix: String,
    pub exit: i32,
}

impl From<adytum::Error> for Failure {
    fn from(e: adytum::Error) -> Self {
        Failure { code: e.code().to_owned(), message: e.to_string(), fix: e.fix().to_owned(), exit: 1 }
    }
}

impl From<args::Usage> for Failure {
    fn from(u: args::Usage) -> Self {
        Failure { code: "usage".to_owned(), message: u.message, fix: u.fix, exit: 2 }
    }
}

struct Done {
    pub json: Value,
    pub text: String,
    /// 1 when a command ran in a cell and failed.
    pub exit: i32,
}

impl Done {
    fn new(json: Value, text: String) -> Done {
        Done { json, text, exit: 0 }
    }
}

fn write_line(w: &mut dyn Write, text: &str) {
    // A closed pipe is the reader's choice, not an error worth reporting.
    let _ = writeln!(w, "{text}");
}

async fn run(tokens: &[String], ctx: &Context, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let json_wanted = args::wants_json(tokens);
    let parsed = match args::parse(tokens) {
        Ok(p) => p,
        Err(usage) => return fail(Failure::from(usage), json_wanted, out, err),
    };
    if parsed.command == args::Command::Help {
        write_line(out, USAGE.trim_end());
        return 0;
    }
    match commands::execute(parsed.command, ctx, out, err, parsed.json).await {
        Ok(done) => {
            if parsed.json {
                write_line(out, &done.json.to_string());
            } else if !done.text.is_empty() {
                write_line(out, &done.text);
            }
            done.exit
        }
        Err(failure) => fail(failure, parsed.json, out, err),
    }
}

fn fail(failure: Failure, json: bool, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    if json {
        write_line(out, &json!({ "error": { "code": failure.code, "message": failure.message, "fix": failure.fix } }).to_string());
    } else {
        write_line(err, format!("adytum: {} {}", failure.message, failure.fix).trim_end());
    }
    failure.exit
}

#[cfg(test)]
mod tests;

fn main() {
    let tokens: Vec<String> = std::env::args().skip(1).collect();
    let mut stdout = std::io::stdout().lock();
    let mut stderr = std::io::stderr().lock();
    let code = match start(&tokens, &mut stdout, &mut stderr) {
        Ok(code) => code,
        Err(message) => {
            let _ = writeln!(stderr, "adytum: {message}");
            1
        }
    };
    let _ = stdout.flush();
    std::process::exit(code);
}

fn start(tokens: &[String], out: &mut dyn Write, err: &mut dyn Write) -> Result<i32, String> {
    let home = adytum::home().ok_or("Could not find a home folder for Adytum. Set ADYTUM_HOME to a short absolute path.")?;
    let ctx = Context { drivers: Arc::new(SystemDrivers::new(adytum::lima::lima_home_in(&home))), home };
    let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(|e| format!("Could not start the async runtime: {e}. Try again."))?;
    Ok(runtime.block_on(run(tokens, &ctx, out, err)))
}
