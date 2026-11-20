use std::fmt;

#[derive(Debug, PartialEq, Eq)]
pub struct Usage {
    pub message: String,
    pub fix: String,
}

impl Usage {
    fn new(message: impl Into<String>, fix: impl Into<String>) -> Usage {
        Usage { message: message.into(), fix: fix.into() }
    }
}

impl fmt::Display for Usage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.message, self.fix)
    }
}

#[derive(Debug, PartialEq, Eq, Default)]
pub struct CreateArgs {
    pub name: String,
    /// `lima` unless given.
    pub kind: Option<String>,
    pub parent: Option<String>,
    pub sealed: bool,
    pub containers: bool,
    pub nested_virtualization: bool,
    pub wall: Option<String>,
    pub cpus: Option<u8>,
    pub memory_mib: Option<u32>,
    pub image: Option<String>,
    /// `(host, guest)`
    pub ports: Vec<(u16, u16)>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Help,
    Version,
    Create(CreateArgs),
    Start(String),
    Stop(String),
    Rm(String),
    Clone { from: String, to: String, wall: Option<String>, ports: Vec<(u16, u16)> },
    Doctor,
    Exec { name: String, argv: Vec<String> },
    Cp { src: String, dst: String, owner: Option<String> },
    Ls,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Parsed {
    pub json: bool,
    pub command: Command,
}

pub const USAGE: &str = "\
adytum: cells on this machine

Usage:
  adytum create <name> [--sealed] [--wall wall_x] [--cpus N] [--memory MiB]
                       [--image ubuntu-24.04] [--port host:guest]...
                       [--containers] [--nested-virtualization]
                                         (a Lima VM; --containers lets it hold
                                          container cells)
  adytum create <name> --kind container --image <ref>@sha256:<digest>
                       [--parent <cell>] [--sealed] [--cpus N] [--memory MiB]
                                         (a container, in <cell> or through
                                          docker here; in a cell it keeps the
                                          cell's wall, and is sealed if the
                                          cell is)
  adytum start <name>
  adytum stop <name>
  adytum rm <name>
  adytum clone <from> <to> [--wall wall_x] [--port host:guest]...
                                         (<from> must be stopped; the copy
                                          keeps its wall unless you give one)
  adytum exec <name> -- <command> [args...]
                                         (stdin that is not a terminal goes to
                                          the command; exits with its code)
  adytum cp [--owner user] <source> <destination>
                                         (one side is <name>:<absolute path>;
                                          --owner writes a copy into a cell
                                          as that guest user, such as root)
  adytum ls                              (cells inside a cell are indented under it)
  adytum doctor                          (what is wrong with the setup; exit 1 on a failure)
  adytum --version

Every command takes --json: one JSON object on stdout, and errors as
{\"error\": {\"code\", \"message\", \"fix\"}}.
Exit codes: 0 done, 1 refused or failed, 2 usage error; exec exits with the
command's own code.
Cells are named adytum-<lowercase letters, digits, dashes>.
";

const VALUE_FLAGS: [&str; 8] = ["wall", "cpus", "memory", "image", "port", "owner", "kind", "parent"];
const SWITCHES: [&str; 3] = ["sealed", "containers", "nested-virtualization"];

#[derive(Default)]
struct Raw {
    json: bool,
    help: bool,
    switches: Vec<String>,
    positional: Vec<String>,
    values: Vec<(String, String)>,
    rest: Option<Vec<String>>,
}

fn tokenize(tokens: &[String]) -> Result<Raw, Usage> {
    let mut raw = Raw::default();
    let mut iter = tokens.iter();
    while let Some(token) = iter.next() {
        if token == "--" {
            raw.rest = Some(iter.by_ref().cloned().collect());
            break;
        }
        if token == "-h" {
            raw.help = true;
        } else if let Some(flag) = token.strip_prefix("--") {
            let (name, inline) = match flag.split_once('=') {
                Some((n, v)) => (n, Some(v.to_owned())),
                None => (flag, None),
            };
            match name {
                "json" => raw.json = true,
                "help" => raw.help = true,
                n if SWITCHES.contains(&n) => raw.switches.push(n.to_owned()),
                n if VALUE_FLAGS.contains(&n) => {
                    let value = match inline {
                        Some(v) => v,
                        None => iter.next().cloned().ok_or_else(|| Usage::new(format!("--{n} needs a value."), format!("Write --{n} <value>.")))?,
                    };
                    raw.values.push((n.to_owned(), value));
                }
                _ => return Err(Usage::new(format!("{token} is not an option."), "Run `adytum --help` to see them.")),
            }
        } else {
            raw.positional.push(token.clone());
        }
    }
    Ok(raw)
}

fn number<T: std::str::FromStr>(flag: &str, value: &str) -> Result<T, Usage> {
    value.parse().map_err(|_| Usage::new(format!("--{flag} wants a number, not {value:?}."), format!("Write --{flag} <number>.")))
}

fn port_pair(value: &str) -> Result<(u16, u16), Usage> {
    let bad = || Usage::new(format!("--port wants host:guest, such as 18080:80, not {value:?}."), "Write --port <host port>:<guest port>.");
    let (host, guest) = value.split_once(':').ok_or_else(bad)?;
    Ok((host.parse().map_err(|_| bad())?, guest.parse().map_err(|_| bad())?))
}

/// Looks for `--json` before the first `--`, so an error can be shaped right
/// even when the rest of the line is wrong.
pub fn wants_json(tokens: &[String]) -> bool {
    tokens.iter().take_while(|t| *t != "--").any(|t| t == "--json")
}

fn one_name(command: &str, raw: &Raw) -> Result<String, Usage> {
    match raw.positional.as_slice() {
        [name] => Ok(name.clone()),
        [] => Err(Usage::new(format!("`adytum {command}` needs a cell name."), format!("Write adytum {command} <name>."))),
        _ => Err(Usage::new(format!("`adytum {command}` takes one cell name."), format!("Write adytum {command} <name>."))),
    }
}

fn only_json(command: &str, raw: &Raw) -> Result<(), Usage> {
    if !raw.switches.is_empty() || !raw.values.is_empty() || raw.rest.is_some() {
        return Err(Usage::new(format!("`adytum {command}` does not take that option."), "Run `adytum --help` to see which commands take which."));
    }
    Ok(())
}

pub fn parse(tokens: &[String]) -> Result<Parsed, Usage> {
    let Some(first) = tokens.first() else {
        return Ok(Parsed { json: false, command: Command::Help });
    };
    if matches!(first.as_str(), "-h" | "--help" | "help") {
        return Ok(Parsed { json: wants_json(tokens), command: Command::Help });
    }
    if matches!(first.as_str(), "-V" | "--version") {
        return Ok(Parsed { json: wants_json(tokens), command: Command::Version });
    }
    let raw = tokenize(&tokens[1..])?;
    let json = raw.json;
    if raw.help {
        return Ok(Parsed { json, command: Command::Help });
    }
    let command = match first.as_str() {
        "create" => {
            if raw.rest.is_some() {
                return Err(Usage::new("`adytum create` takes no command after --.", "Use `adytum exec` to run something."));
            }
            let has = |s: &str| raw.switches.iter().any(|w| w == s);
            let mut args = CreateArgs {
                name: one_name("create", &raw)?,
                sealed: has("sealed"),
                containers: has("containers"),
                nested_virtualization: has("nested-virtualization"),
                ..CreateArgs::default()
            };
            for (flag, value) in &raw.values {
                match flag.as_str() {
                    "kind" => args.kind = Some(value.clone()),
                    "parent" => args.parent = Some(value.clone()),
                    "wall" => args.wall = Some(value.clone()),
                    "image" => args.image = Some(value.clone()),
                    "cpus" => args.cpus = Some(number(flag, value)?),
                    "memory" => args.memory_mib = Some(number(flag, value)?),
                    "port" => args.ports.push(port_pair(value)?),
                    other => return Err(Usage::new(format!("`adytum create` does not take --{other}."), "Run `adytum --help` to see which commands take which.")),
                }
            }
            Command::Create(args)
        }
        "start" | "stop" | "rm" => {
            only_json(first, &raw)?;
            let name = one_name(first, &raw)?;
            match first.as_str() {
                "start" => Command::Start(name),
                "stop" => Command::Stop(name),
                _ => Command::Rm(name),
            }
        }
        "clone" => {
            if !raw.switches.is_empty() || raw.rest.is_some() || raw.values.iter().any(|(f, _)| f != "wall" && f != "port") {
                return Err(Usage::new("`adytum clone` takes only --wall and --port.", "Write adytum clone <from> <to> [--wall wall_x] [--port host:guest]."));
            }
            let [from, to] = raw.positional.as_slice() else {
                return Err(Usage::new("`adytum clone` needs the cell to copy and a name for the copy.", "Write adytum clone <from> <to>."));
            };
            let mut wall = None;
            let mut ports = Vec::new();
            for (flag, value) in &raw.values {
                if flag == "wall" {
                    wall = Some(value.clone());
                } else {
                    ports.push(port_pair(value)?);
                }
            }
            Command::Clone { from: from.clone(), to: to.clone(), wall, ports }
        }
        "doctor" => {
            only_json("doctor", &raw)?;
            if !raw.positional.is_empty() {
                return Err(Usage::new("`adytum doctor` takes no arguments.", "Write adytum doctor."));
            }
            Command::Doctor
        }
        "exec" => {
            if !raw.switches.is_empty() || !raw.values.is_empty() {
                return Err(Usage::new("`adytum exec` does not take that option.", "Write adytum exec <name> -- <command> [args...]."));
            }
            let name = one_name("exec", &raw)?;
            match raw.rest {
                Some(argv) if !argv.is_empty() => Command::Exec { name, argv },
                _ => return Err(Usage::new("`adytum exec` needs a command after --.", "Write adytum exec <name> -- <command> [args...].")),
            }
        }
        "cp" => {
            let owner = match (!raw.switches.is_empty() || raw.rest.is_some(), raw.values.as_slice()) {
                (false, []) => None,
                (false, [(flag, user)]) if flag == "owner" => Some(user.clone()),
                _ => return Err(Usage::new("`adytum cp` takes only --owner, once.", "Write adytum cp [--owner user] <source> <destination>.")),
            };
            match raw.positional.as_slice() {
                [src, dst] => Command::Cp { src: src.clone(), dst: dst.clone(), owner },
                _ => return Err(Usage::new("`adytum cp` needs a source and a destination.", "Write adytum cp <source> <destination>, one of them <name>:<absolute path>.")),
            }
        }
        "ls" => {
            only_json("ls", &raw)?;
            if !raw.positional.is_empty() {
                return Err(Usage::new("`adytum ls` takes no arguments.", "Write adytum ls."));
            }
            Command::Ls
        }
        other => return Err(Usage::new(format!("There is no command {other:?}."), "Run `adytum --help` to see them.")),
    };
    Ok(Parsed { json, command })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(line: &str) -> Result<Parsed, Usage> {
        let tokens: Vec<String> = line.split_whitespace().map(str::to_owned).collect();
        parse(&tokens)
    }

    #[test]
    fn create_reads_every_option() {
        let parsed = p("create adytum-a --sealed --wall wall_x --cpus 4 --memory=4096 --image ubuntu-24.04 --port 18080:80 --port 18443:443 --json").unwrap();
        assert!(parsed.json);
        let Command::Create(a) = parsed.command else { panic!("not create") };
        assert_eq!(a.name, "adytum-a");
        assert!(a.sealed);
        assert_eq!(a.wall.as_deref(), Some("wall_x"));
        assert_eq!((a.cpus, a.memory_mib), (Some(4), Some(4096)));
        assert_eq!(a.ports, [(18080, 80), (18443, 443)]);
        assert!(!a.containers && !a.nested_virtualization && a.kind.is_none() && a.parent.is_none());
        let Command::Create(a) = p("create adytum-v --containers --nested-virtualization").unwrap().command else { panic!("not create") };
        assert!(a.containers && a.nested_virtualization);
        let Command::Create(a) = p("create adytum-c --kind container --parent adytum-v --image alpine@sha256:00").unwrap().command else { panic!("not create") };
        assert_eq!((a.kind.as_deref(), a.parent.as_deref(), a.image.as_deref()), (Some("container"), Some("adytum-v"), Some("alpine@sha256:00")));
    }

    #[test]
    fn exec_keeps_everything_after_the_dashes_as_it_is() {
        let tokens: Vec<String> = ["exec", "adytum-a", "--json", "--", "echo", "--json", "$(x)", "a b"].iter().map(|s| (*s).to_owned()).collect();
        let parsed = parse(&tokens).unwrap();
        assert!(parsed.json);
        assert_eq!(parsed.command, Command::Exec { name: "adytum-a".into(), argv: vec!["echo".into(), "--json".into(), "$(x)".into(), "a b".into()] });
        // --json after the dashes belongs to the command, not to adytum.
        assert!(!wants_json(&["exec".into(), "a".into(), "--".into(), "--json".into()]));
    }

    #[test]
    fn mistakes_are_usage_errors_that_say_what_to_type() {
        for bad in [
            "frobnicate",
            "start",
            "start a b",
            "exec adytum-a",
            "exec adytum-a --",
            "cp onlyone",
            "cp --owner root --owner cell ./a adytum-a:/a",
            "cp --wall wall_x ./a adytum-a:/a",
            "ls extra",
            "create adytum-a --cpus many",
            "create adytum-a --port 8080",
            "create adytum-a --port 99999:80",
            "create adytum-a --wall",
            "create adytum-a --bogus",
            "create adytum-a --owner root",
            "start adytum-a --sealed",
            "start adytum-a --containers",
            "ls --parent adytum-a",
            "ls --wall wall_x",
            "clone adytum-a",
            "clone adytum-a adytum-b --sealed",
            "clone adytum-a adytum-b --cpus 2",
            "doctor extra",
        ] {
            let err = p(bad).unwrap_err();
            assert!(!err.message.is_empty() && !err.fix.is_empty(), "{bad}");
        }
    }

    #[test]
    fn clone_doctor_and_cp_read() {
        let Command::Clone { from, to, wall, ports } = p("clone adytum-a adytum-b --wall wall_x --port 1:2").unwrap().command else { panic!("not clone") };
        assert_eq!((from.as_str(), to.as_str(), wall.as_deref(), ports.as_slice()), ("adytum-a", "adytum-b", Some("wall_x"), &[(1, 2)][..]));
        assert_eq!(p("doctor --json").unwrap().command, Command::Doctor);
        assert_eq!(p("cp --owner root ./a adytum-a:/opt/a").unwrap().command, Command::Cp { src: "./a".into(), dst: "adytum-a:/opt/a".into(), owner: Some("root".into()) });
    }

    #[test]
    fn help_is_a_command() {
        assert_eq!(p("").unwrap().command, Command::Help);
        assert_eq!(p("--help").unwrap().command, Command::Help);
        assert_eq!(p("--version").unwrap().command, Command::Version);
        assert_eq!(p("create --help").unwrap().command, Command::Help);
    }
}
