use std::{env, fs::File, path::Path, process::ExitCode};

use hfs_b::{HostResult, *};
use mac_hfs_b::{time::Timestamp, *};
fn arg(args: &[String], n: usize) -> HostResult<&str>
{
    args.get(n).map(String::as_str).ok_or_else(|| "missing argument; run hfs-b help".into())
}
fn number(args: &[String], n: usize) -> HostResult<u64>
{
    Ok(arg(args, n)?.parse()?)
}
use hfs_b::copy::entry_json;
fn identifier(volume: &mut Volume<'_, Image>, args: &[String], n: usize) -> HostResult<u32>
{
    let value = arg(args, n)?;
    if value.starts_with('/') { Ok(copy::resolve(volume, value)?.id) } else { Ok(value.parse()?) }
}
fn run() -> HostResult<()>
{
    let args: Vec<String> = env::args().skip(1).collect();
    let command = args.first().map(String::as_str).unwrap_or("help");
    if command == "help"
    {
        println!("hfs-b create IMAGE SIZE_BYTES NAME [MAC_MICROSECONDS]\nhfs-b format IMAGE \
                  START_SECTOR SECTORS NAME [MAC_MICROSECONDS]\nhfs-b check IMAGE\nhfs-b list \
                  IMAGE [DIRECTORY_ID]\nhfs-b mkdir IMAGE PARENT_ID NAME\nhfs-b import IMAGE \
                  PARENT_OR_PATH MACBINARY [NAME]\nhfs-b put IMAGE PARENT_ID NAME DATA_FILE \
                  RESOURCE_FILE\nhfs-b export IMAGE FILE_ID OUTPUT_DIRECTORY\nhfs-b restore \
                  IMAGE PARENT_ID EXPORTED_DIRECTORY\nhfs-b bless IMAGE DIRECTORY_ID\nhfs-b \
                  rename IMAGE ID PARENT_ID NAME\nhfs-b remove IMAGE ID\nhfs-b copy-in IMAGE \
                  HOST_PATH [GUEST_DIRECTORY] [--replace]\nhfs-b copy-out IMAGE GUEST_PATH \
                  HOST_DIRECTORY");
        return Ok(());
    }
    if command == "create" || command == "format"
    {
        let mut image = if command == "create"
        {
            Image::create(Path::new(arg(&args, 1)?), number(&args, 2)?)?
        }
        else
        {
            Image::open(Path::new(arg(&args, 1)?), true)?
        };
        let (partition, name_index) = if command == "create"
        {
            (image.partition(), 3)
        }
        else
        {
            (Partition { start:   number(&args, 2)?,
                         sectors: number(&args, 3)? },
             4)
        };
        let created = if args.len() > name_index + 1
        {
            Timestamp(number(&args, name_index + 1)?)
        }
        else
        {
            now()?
        };
        let mut uuid = [0; 16];
        getrandom::fill(&mut uuid).map_err(|e| format!("cannot create filesystem UUID: {e}"))?;
        format(&mut image,
               partition,
               FormatOptions { name: arg(&args, name_index)?,
                               created,
                               uuid },
               Budget::default())?;
        let mut volume = Volume::mount(&mut image, partition, Budget::default())?;
        println!("Formatted HFS-B v1: {:?}", volume.check()?);
        return Ok(());
    }
    let writable =
        !matches!(command, "check" | "list" | "stat" | "export" | "copy-out" | "extract");
    let mut image = Image::open(Path::new(arg(&args, 1)?), writable)?;
    let partition = image.partition();
    let mut volume = Volume::mount(&mut image,
                                   partition,
                                   Budget { io_bytes: 64 * 1024 * 1024,
                                            ..Budget::default() })?;
    let clock = now()?;
    match command
    {
        "check" => println!("HFS-B v1 consistent: {:?}", volume.check()?),
        "list" =>
        {
            let directory = if args.len() > 2 { identifier(&mut volume, &args, 2)? } else { 2 };
            let mut after = None;
            while let Some(e) = volume.next_child(directory, after.as_deref())?
            {
                println!("{}", entry_json(&e));
                after = Some(e.name);
            }
        }
        "mkdir" =>
        {
            let parent = identifier(&mut volume, &args, 2)?;
            let e = volume.create(parent, arg(&args, 3)?, true, clock)?;
            println!("{}", entry_json(&e));
        }
        "import" =>
        {
            let path = Path::new(arg(&args, 3)?);
            if std::fs::metadata(path)?.len() > 64 * 1024 * 1024
            {
                return Err("MacBinary import budget exceeded; use streaming put for large forks".into());
            }
            let file = mac_formats::macbinary::decode(&std::fs::read(path)?)?;
            let parent = identifier(&mut volume, &args, 2)?;
            let e = import_macbinary_named(&mut volume,
                                           parent,
                                           &file,
                                           args.get(4)
                                               .map(String::as_str)
                                               .unwrap_or(&mac_formats::text::decode(&file.name)))?;
            println!("{}", entry_json(&e));
        }
        "put" =>
        {
            let parent = identifier(&mut volume, &args, 2)?;
            let e = volume.create(parent, arg(&args, 3)?, false, clock)?;
            write_stream(&mut volume, e.id, ForkKind::Data, File::open(arg(&args, 4)?)?, clock)?;
            write_stream(&mut volume,
                         e.id,
                         ForkKind::Resource,
                         File::open(arg(&args, 5)?)?,
                         clock)?;
            println!("{}", entry_json(&volume.entry(e.id)?));
        }
        "export" =>
        {
            let id = identifier(&mut volume, &args, 2)?;
            copy::copy_out(&mut volume, id, Path::new(arg(&args, 3)?))?;
        }
        "restore" =>
        {
            let parent = identifier(&mut volume, &args, 2)?;
            let e = copy::copy_in(&mut volume, parent, Path::new(arg(&args, 3)?), false)?;
            println!("{}", entry_json(&e));
        }
        "stat" =>
        {
            let id = identifier(&mut volume, &args, 2)?;
            println!("{}", entry_json(&volume.entry(id)?));
        }
        "extract" =>
        {
            let id = identifier(&mut volume, &args, 2)?;
            let kind = match args.get(4).map(String::as_str).unwrap_or("data")
            {
                "data" => ForkKind::Data,
                "resource" => ForkKind::Resource,
                _ => return Err("fork must be data or resource".into())
            };
            let out =
                std::fs::OpenOptions::new().write(true).create_new(true).open(arg(&args, 3)?)?;
            read_stream(&mut volume, id, kind, out)?;
        }
        "copy-out" =>
        {
            let e = copy::resolve(&mut volume, arg(&args, 2)?)?;
            copy::copy_out(&mut volume, e.id, Path::new(arg(&args, 3)?))?;
        }
        "copy-in" =>
        {
            let parent = copy::resolve(&mut volume,
                                       args.get(3)
                                           .filter(|s| s.as_str() != "--replace")
                                           .map(String::as_str)
                                           .unwrap_or("/"))?;
            if !parent.directory
            {
                return Err("copy-in destination must be a guest directory".into());
            }
            let e = copy::copy_in(&mut volume,
                                  parent.id,
                                  Path::new(arg(&args, 2)?),
                                  args.iter().any(|s| s == "--replace"))?;
            println!("{}", entry_json(&e));
        }
        "bless" =>
        {
            let id = identifier(&mut volume, &args, 2)?;
            volume.bless(id, clock)?;
        }
        "rename" =>
        {
            let id = identifier(&mut volume, &args, 2)?;
            let parent = identifier(&mut volume, &args, 3)?;
            volume.rename(id, parent, arg(&args, 4)?, clock)?;
        }
        "remove" =>
        {
            let id = identifier(&mut volume, &args, 2)?;
            volume.remove(id, clock)?;
        }
        _ => return Err("unknown command; run hfs-b help".into())
    }
    Ok(())
}
fn main() -> ExitCode
{
    match run()
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) =>
        {
            eprintln!("hfs-b: {e}");
            ExitCode::FAILURE
        }
    }
}
