use std::{env, fs::File, path::Path, process::ExitCode};

use hfs_b::{HostResult, *};
use mac_hfs_b::{time::Timestamp, *};
use serde_json::json;
fn arg(args: &[String], n: usize) -> HostResult<&str>
{
    args.get(n).map(String::as_str).ok_or_else(|| "missing argument; run hfs-b help".into())
}
fn number(args: &[String], n: usize) -> HostResult<u64>
{
    Ok(arg(args, n)?.parse()?)
}
fn entry_json(e: &Entry) -> serde_json::Value
{
    json!({"format":"system-risc-forks","version":1,"id":e.id,"parent":e.parent,"name":e.name,"directory":e.directory,"flags":e.flags,
        "children":e.children,"created_mac_microseconds":e.created.0,"modified_mac_microseconds":e.modified.0,
        "backup_mac_microseconds":e.backup.map(|t|t.0),"finder_info":e.finder_info.to_vec(),
        "data_length":e.data.length,"resource_length":e.resources.length})
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
                  PARENT_ID MACBINARY\nhfs-b put IMAGE PARENT_ID NAME DATA_FILE \
                  RESOURCE_FILE\nhfs-b export IMAGE FILE_ID OUTPUT_DIRECTORY\nhfs-b restore \
                  IMAGE PARENT_ID EXPORTED_DIRECTORY\nhfs-b bless IMAGE DIRECTORY_ID\nhfs-b \
                  rename IMAGE ID PARENT_ID NAME\nhfs-b remove IMAGE ID");
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
        File::open("/dev/urandom")?.read_exact(&mut uuid)?;
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
    let writable = !matches!(command, "check" | "list" | "export");
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
            let directory = if args.len() > 2 { u32::try_from(number(&args, 2)?)? } else { 2 };
            let mut after = None;
            while let Some(e) = volume.next_child(directory, after.as_deref())?
            {
                println!("{}", entry_json(&e));
                after = Some(e.name);
            }
        }
        "mkdir" =>
        {
            let e = volume.create(u32::try_from(number(&args, 2)?)?, arg(&args, 3)?, true, clock)?;
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
            let e = import_macbinary(&mut volume, u32::try_from(number(&args, 2)?)?, &file)?;
            println!("{}", entry_json(&e));
        }
        "put" =>
        {
            let e = volume.create(u32::try_from(number(&args, 2)?)?, arg(&args, 3)?, false, clock)?;
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
            let id = u32::try_from(number(&args, 2)?)?;
            let e = volume.entry(id)?;
            if e.directory
            {
                return Err("export expects a file ID".into());
            }
            let directory = Path::new(arg(&args, 3)?);
            std::fs::create_dir(directory)?;
            for (kind, name) in [(ForkKind::Data, "data"), (ForkKind::Resource, "resources")]
            {
                read_stream(&mut volume, id, kind, File::create(directory.join(name))?)?;
            }
            let mut metadata = entry_json(&e);
            metadata["comment"] = json!(volume.comment(id)?);
            std::fs::write(directory.join("metadata.json"), serde_json::to_vec_pretty(&metadata)?)?;
        }
        "restore" =>
        {
            let directory = Path::new(arg(&args, 3)?);
            let metadata_path = directory.join("metadata.json");
            if std::fs::metadata(&metadata_path)?.len() > 16 * 1024
            {
                return Err("fork metadata budget exceeded".into());
            }
            let bytes = std::fs::read(metadata_path)?;
            let m: serde_json::Value = serde_json::from_slice(&bytes)?;
            if m["format"] != "system-risc-forks" || m["version"] != 1 || m["directory"] != false
            {
                return Err("incompatible exported metadata; recreate export".into());
            }
            let created =
                Timestamp(m["created_mac_microseconds"].as_u64().ok_or("missing created date")?);
            let modified = Timestamp(m["modified_mac_microseconds"].as_u64()
                                                                   .ok_or("missing modified \
                                                                           date")?);
            let backup = if m["backup_mac_microseconds"].is_null()
            {
                None
            }
            else
            {
                Some(Timestamp(m["backup_mac_microseconds"].as_u64()
                                                           .ok_or("invalid backup date")?))
            };
            let flags = u16::try_from(m["flags"].as_u64().ok_or("missing flags")?)?;
            let info = m["finder_info"].as_array().ok_or("missing Finder metadata")?;
            if info.len() != 32
            {
                return Err("invalid Finder metadata size".into());
            }
            let mut finder_info = [0; 32];
            for (i, v) in info.iter().enumerate()
            {
                finder_info[i] = u8::try_from(v.as_u64().ok_or("invalid Finder metadata")?)?;
            }
            let name = m["name"].as_str().ok_or("missing filename")?;
            let e = volume.create(u32::try_from(number(&args, 2)?)?, name, false, created)?;
            for (kind, path, key) in [(ForkKind::Data, "data", "data_length"),
                                      (ForkKind::Resource, "resources", "resource_length")]
            {
                let file = File::open(directory.join(path))?;
                if Some(file.metadata()?.len()) != m[key].as_u64()
                {
                    return Err("exported fork length mismatch".into());
                }
                write_stream(&mut volume, e.id, kind, file, modified)?;
            }
            volume.set_comment(e.id, m["comment"].as_str().ok_or("missing comment")?, modified)?;
            let e = volume.set_metadata(e.id, flags, finder_info, created, modified, backup)?;
            println!("{}", entry_json(&e));
        }
        "bless" => volume.bless(u32::try_from(number(&args, 2)?)?, clock)?,
        "rename" =>
        {
            volume.rename(u32::try_from(number(&args, 2)?)?,
                          u32::try_from(number(&args, 3)?)?,
                          arg(&args, 4)?,
                          clock)?;
        }
        "remove" => volume.remove(u32::try_from(number(&args, 2)?)?, clock)?,
        _ => return Err("unknown command; run hfs-b help".into())
    }
    Ok(())
}
use std::io::Read;
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
