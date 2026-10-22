//! Linux-side diagnostic over the same bounded parser used by the driver.
use std::{fs::File,
          io::{Read, Seek, SeekFrom}};

use mac_sit::{Contents, IndexBuilder, Limits};
fn main() -> Result<(), Box<dyn std::error::Error>>
{
    let mut arguments = std::env::args_os().skip(1);
    let path = arguments.next().ok_or("usage: inspect ARCHIVE.sit")?;
    if arguments.next().is_some()
    {
        return Err("usage: inspect ARCHIVE.sit".into());
    }
    let mut file = File::open(&path)?;
    let mut builder =
        IndexBuilder::new(file.metadata()?.len(), Limits::default()).map_err(|e| {
                                                                        format!("Archive header: \
                                                                                 {e:?}")
                                                                    })?;
    let mut read = 0;
    while let Some(request) = builder.request().map_err(|e| format!("Catalog request: {e:?}"))?
    {
        file.seek(SeekFrom::Start(request.offset))?;
        let mut bytes = vec![0; request.length];
        file.read_exact(&mut bytes)?;
        builder.feed(&bytes)
               .map_err(|e| format!("Archive catalog at {:#x}: {e:?}", request.offset))?;
        read += bytes.len();
    }
    let index = builder.finish().map_err(|e| format!("Archive catalog validation: {e:?}"))?;
    println!("{:?}: {} entries, {} catalog bytes read",
             index.header().format,
             index.nodes().len(),
             read);
    let mut utf8 = [0; 765];
    for node in index.nodes()
    {
        println!("id={} parent={} name={:?}",
                 node.id,
                 node.parent,
                 node.name().utf8(&mut utf8).map_err(|e| format!("Archive filename: {e:?}"))?);
        if let Contents::File(header) = node.contents
        {
            for (kind, fork) in [("resource", header.resource), ("data", header.data)]
            {
                println!("  {kind}: {} -> {} bytes, method {}, at {:#x}",
                         fork.packed_length, fork.length, fork.method_byte, fork.offset);
                let mut decoder = mac_sit::Decoder::new(fork).map_err(|e| {
                                                                 format!("{kind} fork in entry \
                                                                          {}: {e:?}",
                                                                         node.id)
                                                             })?;
                file.seek(SeekFrom::Start(fork.offset))?;
                let mut input = [0; 4096];
                let mut output_buffer = [0; 4096];
                let (mut at, mut end, mut remaining, mut produced, mut slices) =
                    (0, 0, fork.packed_length, 0u64, 0u64);
                loop
                {
                    let progress = decoder.step(&input[at..end], &mut output_buffer)
                                          .map_err(|e| {
                                              format!("{kind} fork in entry {} after {produced} \
                                                       output bytes: {e:?}",
                                                      node.id)
                                          })?;
                    at += progress.consumed;
                    produced += progress.written as u64;
                    slices += 1;
                    if progress.complete
                    {
                        break;
                    }
                    if progress.needs_input && at == end
                    {
                        end = remaining.min(input.len() as u64) as usize;
                        if end == 0
                        {
                            return Err(format!("{kind} fork in entry {}: compressed source \
                                                ended before verification",
                                               node.id).into());
                        }
                        file.read_exact(&mut input[..end])?;
                        remaining -= end as u64;
                        at = 0;
                    }
                    if slices
                       > fork.length
                             .saturating_add(fork.packed_length)
                             .saturating_mul(128)
                             .saturating_add(100_000)
                    {
                        return Err("decoder exceeded the work bound".into());
                    }
                }
                println!("    verified {produced} bytes in {slices} bounded steps");
            }
        }
    }
    Ok(())
}
