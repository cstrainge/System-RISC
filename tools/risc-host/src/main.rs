use std::{env,
          io::{self, Write},
          net::{SocketAddr, TcpListener},
          path::Path,
          process::ExitCode,
          sync::Arc};

use risc_host::Export;
fn main() -> ExitCode
{
    match run()
    {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) =>
        {
            eprintln!("risc-host: {e}");
            ExitCode::FAILURE
        }
    }
}
fn run() -> Result<(), Box<dyn std::error::Error>>
{
    let mut address: SocketAddr = "127.0.0.1:0".parse()?;
    let mut exports = Vec::new();
    let mut args = env::args().skip(1);
    while let Some(arg) = args.next()
    {
        match arg.as_str()
        {
            "--listen" => address = args.next().ok_or("--listen needs ADDRESS")?.parse()?,
            "--export" | "--read-only-export" =>
            {
                let value = args.next().ok_or("export needs NAME=PATH")?;
                let (name, path) = value.split_once('=').ok_or("export needs NAME=PATH")?;
                if name.is_empty() || path.is_empty()
                {
                    return Err("empty export name/path".into());
                }
                exports.push(Export::open(name, Path::new(path), arg == "--export")
                    .map_err(|e| io::Error::other(format!("export {name}: status {}", e.0)))?);
            }
            "--help" =>
            {
                println!("risc-host [--listen 127.0.0.1:PORT] --export NAME=PATH \
                          [--read-only-export NAME=PATH] ...");
                return Ok(());
            }
            _ => return Err(format!("unknown argument {arg}").into())
        }
    }
    if !address.ip().is_loopback()
    {
        return Err("host bridge listener must use a loopback address".into());
    }
    if exports.is_empty() || exports.len() > 16
    {
        return Err("select between one and sixteen explicit exports".into());
    }
    let listener = TcpListener::bind(address)?;
    let exports = Arc::new(exports);
    println!("HOST_BRIDGE_READY {}", listener.local_addr()?);
    io::stdout().flush()?;
    // A companion belongs to one VM. Reconnect creates fresh capability IDs and
    // drops every file, lock, enumeration and unpublished stage from the old one.
    for stream in listener.incoming()
    {
        let mut stream = stream?;
        stream.set_nodelay(true)?;
        if let Err(e) = risc_host::serve_connection(&mut stream, exports.clone())
        {
            eprintln!("host bridge connection: {e}");
        }
    }
    Ok(())
}
