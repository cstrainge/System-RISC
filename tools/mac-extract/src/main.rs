//! Command-line form of the extraction step.
//!
//! The build graph runs the same library entry point through `xtask stage
//! extract`, so this binary exists for a developer who wants to run one package
//! by hand — not because the build needs it to.
use std::{env, path::PathBuf, process::ExitCode};

fn main() -> ExitCode
{
    match run()
    {
        Ok(code) => code,
        Err(cause) =>
        {
            eprintln!("mac-extract: {cause}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> &'static str
{
    "mac-extract — build a package's generated resources from its extract list\n\nmac-extract \
     extract --package PATH --list PATH [--check]\n\n\x20 --package  the package's \
     resources.json\n\x20 --list     its extract-list.json; the disc it names is read from \
     iso/\n\x20 --check    compare what would be generated with what is on disk\n\nOutputs go \
     beside the list: extracted_resources.json and extracted_assets/.\nThe build runs this step \
     itself; running it by hand is optional."
}

fn run() -> mac_extract::Result<ExitCode>
{
    let args: Vec<String> = env::args().skip(1).collect();
    let Some(command) = args.first()
    else
    {
        print!("{}", usage());
        return Ok(ExitCode::FAILURE);
    };
    match command.as_str()
    {
        "extract" =>
        {
            let rest = &args[1..];
            let package = option(rest, "--package").ok_or_else(|| {
                                                       mac_extract::error("--package is required")
                                                   })?;
            let list =
                option(rest, "--list").ok_or_else(|| mac_extract::error("--list is required"))?;
            let check = rest.iter().any(|argument| argument == "--check");
            mac_extract::extract(&PathBuf::from(package), &PathBuf::from(list), check)
        }
        "help" | "--help" | "-h" =>
        {
            print!("{}", usage());
            Ok(ExitCode::SUCCESS)
        }
        other => Err(mac_extract::error(format!("unknown command '{other}'; run mac-extract help")))
    }
}

fn option<'a>(args: &'a [String], name: &str) -> Option<&'a str>
{
    args.iter()
        .position(|argument| argument == name)
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
}
