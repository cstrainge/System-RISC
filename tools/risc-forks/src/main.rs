use std::{env, path::Path, process::ExitCode};

use risc_forks::{Kind, Result, Volume, error, io::Slice};
fn run() -> Result<()>
{
    let args: Vec<_> = env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("help");
    if command == "help"
    {
        println!("risc-forks init ROOT NAME\nrisc-forks import ROOT HOST_PATH \
                  [GUEST_PARENT]\nrisc-forks import-macbinary ROOT MACBINARY \
                  [GUEST_PARENT]\nrisc-forks export ROOT GUEST_PATH DESTINATION\nrisc-forks list \
                  ROOT [GUEST_PATH]\nrisc-forks mkdir ROOT GUEST_PARENT NAME\nrisc-forks check \
                  ROOT");
        return Ok(());
    }
    let arg = |index| {
        args.get(index)
            .map(String::as_str)
            .ok_or_else(|| error("missing argument; run risc-forks help"))
    };
    if command == "init"
    {
        let volume = Volume::initialize(Path::new(arg(1)?), arg(2)?)?;
        let root = volume.root()?;
        println!("{}", risc_forks::json(&root.metadata, root.layout));
        return Ok(());
    }
    let volume = Volume::open(Path::new(arg(1)?))?;
    match command
    {
        "check" =>
        {
            println!("Validated {} entries; both forks, metadata, identities and canonical names \
                      agree.",
                     volume.check()?)
        }
        "list" =>
        {
            for e in volume.children(args.get(2).map(String::as_str).unwrap_or("/"))?
            {
                println!("{}", risc_forks::json(&e.metadata, e.layout));
            }
        }
        "import" | "import-macbinary" =>
        {
            let parent = args.get(3).map(String::as_str).unwrap_or("/");
            let source = Path::new(arg(2)?);
            let entry = if command == "import"
            {
                volume.import(parent, source)?
            }
            else
            {
                volume.import_macbinary(parent, source)?
            };
            println!("{}", risc_forks::json(&entry.metadata, entry.layout));
        }
        "export" => volume.export(arg(2)?, Path::new(arg(3)?))?,
        "mkdir" =>
        {
            let entry = volume.create(arg(2)?,
                                      arg(3)?,
                                      Kind::Directory,
                                      &mut Slice(&[]),
                                      &mut Slice(&[]),
                                      None)?;
            println!("{}", risc_forks::json(&entry.metadata, entry.layout));
        }
        _ => return Err(error("unknown command; run risc-forks help"))
    }
    Ok(())
}
fn main() -> ExitCode
{
    match run()
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) =>
        {
            eprintln!("risc-forks: {error}");
            ExitCode::FAILURE
        }
    }
}
