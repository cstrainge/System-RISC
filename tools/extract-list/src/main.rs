//! TEMPORARY list-builder scaffolding — the A1 disc reader, A2 report and A3
//! classifier.
//!
//! Not a shipped tool and not wired into xtask: it exists to produce
//! `extract-list.json` once. See the crate library for what removing it takes.
//!
//! `report` has no disc dependency: it resolves our own resources to classic
//! payload bytes. `build` reads the disc the list names, classifies the package
//! against it, and writes the package's extract list.

use std::{collections::{BTreeMap, BTreeSet},
          env, fs,
          path::{Path, PathBuf},
          process::ExitCode};

use extract_list::{Classification, Form, IsoResource, Package, Report, Verdict, classify,
                   extract_list, human_report, iso, read_fork, read_package, take_all,
                   unreferenced, usage, usage_counts};
use mac_build::Result;

fn usage() -> String
{
    "extract-list — TEMPORARY list-builder scaffolding (spec A1/A2/A3)\n\nextract-list report \
     --package LABEL=PATH ...\n\x20   Resolve every resource in each package to classic payload \
     bytes and\n\x20   report counts by payload form, plus any that do not \
     resolve.\n\nextract-list classify --package LABEL=PATH ... --fork LABEL=PATH ... \
     [--details]\n\x20   Classify our resources against the ISO side per spec §7.2. --fork\n\x20   \
     names a classic resource fork as a stand-in for a disc source file.\n\nextract-list build \
     --package LABEL=PATH --iso NAME --description TEXT\n\x20                 --source 'System \
     Folder/System' --out PATH\n\x20   Read that file's resources from the disc NAME in the \
     repository's iso\n\x20   directory, classify the package against it, and write its extract \
     list.\n\nLabels are the source file the resources belong to: 'system' or 'finder'."
                                                                                        .to_string()
}

fn main() -> ExitCode
{
    match run()
    {
        Ok(code) => code,
        Err(cause) =>
        {
            eprintln!("extract-list: {cause}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode>
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
        "report" => report(&args[1..]),
        "classify" => classify_command(&args[1..]),
        "build" => build_command(&args[1..]),
        "disc" => disc_command(&args[1..]),
        "help" | "--help" | "-h" =>
        {
            print!("{}", usage());
            Ok(ExitCode::SUCCESS)
        }
        other => Err(mac_build::error(format!("unknown command '{other}'; run extract-list help")))
    }
}

/// Split a `LABEL=PATH` argument. The label is deliberately required: one
/// extract list covers both source files, so every entry says which it is from.
fn pair(value: &str) -> Result<(String, PathBuf)>
{
    let Some((label, path)) = value.split_once('=')
    else
    {
        return Err(mac_build::error(format!("'{value}' must be LABEL=PATH, for example \
                                             system=system/resources.json")));
    };
    if label.is_empty() || path.is_empty()
    {
        return Err(mac_build::error(format!("'{value}' must name both a label and a path")));
    }
    Ok((label.to_string(), PathBuf::from(path)))
}

fn packages(args: &[String]) -> Result<Vec<Package>>
{
    let mut packages = Vec::new();
    for (index, argument) in args.iter().enumerate()
    {
        if argument == "--package"
        {
            let (label, path) = pair(args.get(index + 1).map(String::as_str).unwrap_or(""))?;
            packages.push(read_package(&label, &path)?);
        }
    }
    if packages.is_empty()
    {
        return Err(mac_build::error("name at least one --package LABEL=PATH"));
    }
    Ok(packages)
}

fn report(args: &[String]) -> Result<ExitCode>
{
    let packages = packages(args)?;
    let mut total = 0;
    let mut resolved = 0;
    let mut native = 0;
    let mut unresolvable = 0;
    for package in &packages
    {
        println!("{}  {}", package.source, package.path.display());
        let mut forms: BTreeMap<Form, usize> = BTreeMap::new();
        for resource in &package.resources
        {
            *forms.entry(resource.form).or_default() += 1;
        }
        for (form, count) in &forms
        {
            let note = if *form == Form::NativeCode
            {
                "  (ours; never compared or extracted)"
            }
            else
            {
                ""
            };
            println!("  {:<12} {count:>5}{note}", form.as_str());
        }
        for failure in &package.unresolvable
        {
            println!("  unresolvable: {failure}");
        }
        let package_native =
            package.resources.iter().filter(|r| r.form == Form::NativeCode).count();
        let package_resolved = package.resources.len() - package_native;
        println!("  resolved {package_resolved}, native_code {package_native}, unresolvable {}",
                 package.unresolvable.len());
        total += package.resources.len() + package.unresolvable.len();
        resolved += package_resolved;
        native += package_native;
        unresolvable += package.unresolvable.len();
    }
    println!("\ntotal {total}: {resolved} resolved to classic bytes, {native} native_code, \
              {unresolvable} unresolvable");
    Ok(if unresolvable == 0 { ExitCode::SUCCESS } else { ExitCode::FAILURE })
}

fn classify_command(args: &[String]) -> Result<ExitCode>
{
    let packages = packages(args)?;
    let mut iso = Vec::new();
    for (index, argument) in args.iter().enumerate()
    {
        if argument == "--fork"
        {
            let (label, path) = pair(args.get(index + 1).map(String::as_str).unwrap_or(""))?;
            iso.extend(read_fork(&label, &path)?);
        }
    }
    if iso.is_empty()
    {
        return Err(mac_build::error("classify needs the ISO side to compare with; pass --fork \
                                     LABEL=PATH, or use build to read a disc"));
    }
    let report = classify(&packages, &iso);
    print_report(&report);
    if args.iter().any(|argument| argument == "--details")
    {
        details(&report);
    }
    Ok(ExitCode::SUCCESS)
}

/// Read the disc the list names, classify the package against it, and write
/// the package's extract list. Nothing here names System, Finder or a disc: the
/// arguments come from the package, so another package with another disc is the
/// same command.
fn build_command(args: &[String]) -> Result<ExitCode>
{
    let (label, package_path) = pair(required(args, "--package")?)?;
    let name = required(args, "--iso")?.to_string();
    let description = required(args, "--description")?.to_string();
    let requested: Vec<String> = options(args, "--source").into_iter().map(String::from).collect();
    if requested.is_empty()
    {
        return Err(mac_build::error("--source is required: name the disc file the package \
                                     takes resources from, or a folder of such files"));
    }
    let out = PathBuf::from(required(args, "--out")?);
    let disc = iso::require(&name, &description)?;
    let package = read_package(&label, &package_path)?;
    let disc_sources = iso::sources(&disc, &requested)?;
    let held_requested: Vec<String> =
        options(args, "--held").into_iter().map(String::from).collect();
    let held_sources =
        if held_requested.is_empty() { Vec::new() } else { iso::sources(&disc, &held_requested)? };
    let mut iso_side = Vec::new();
    for file in &disc_sources
    {
        for resource in &file.resources
        {
            iso_side.push(IsoResource { source:   file.path.clone(),
                                        held:     false,
                                        resource: resource.clone() });
        }
    }
    for file in &held_sources
    {
        for resource in &file.resources
        {
            iso_side.push(IsoResource { source:   file.path.clone(),
                                        held:     true,
                                        resource: resource.clone() });
        }
    }
    let paths: Vec<String> = disc_sources.iter().map(|file| file.path.clone()).collect();
    let mut report = classify(std::slice::from_ref(&package), &iso_side);
    let take: Vec<String> = options(args, "--take-all").into_iter().map(String::from).collect();
    if !take.is_empty()
    {
        take_all(&mut report, &iso_side, &package, &take, Form::File);
    }
    print_report(&report);
    let keys: Vec<usage::Key> =
        report.classifications
              .iter()
              .map(|entry| {
                  usage::Key { kind:   entry.kind.clone(),
                               id:     entry.id,
                               symbol: entry.symbol.clone(),
                               name:   entry.name.as_deref().map(mac_formats::text::decode) }
              })
              .collect();
    let usage = usage::scan(Path::new("."), &keys)?;
    let counts = usage_counts(&report, &usage, &package);
    println!("usage        {} referenced, {} reachable, {} cannot be ruled out, {} with no \
              reference found",
             counts.named, counts.reachable, counts.unproven, counts.unmatched);

    let excluded = if args.iter().any(|argument| argument == "--exclude-unreferenced")
    {
        unreferenced(&report, &usage, &package)
    }
    else
    {
        BTreeSet::new()
    };
    println!("excluded      {:>5} extracted resources with no reference at all", excluded.len());
    let list = extract_list(&description, &name, &report, &paths, &excluded);
    let mut text = list.to_json()?;
    text.push('\n');
    fs::write(&out, text)?;
    println!("\nwrote {} ({} entries in {} source files)",
             out.display(),
             list.sources.iter().map(|source| source.resources.len()).sum::<usize>(),
             list.sources.len());
    if let Some(path) = option(args, "--report")
    {
        fs::write(path, human_report(&description, &name, &paths, &package, &report, &usage))?;
        println!("wrote {path}");
    }
    Ok(ExitCode::SUCCESS)
}

fn option<'a>(args: &'a [String], name: &str) -> Option<&'a str>
{
    args.iter()
        .position(|argument| argument == name)
        .and_then(|index| args.get(index + 1))
        .map(String::as_str)
}

/// Show a folder or a file on the disc, to establish where it keeps its System
/// file before an extract list names a path. With `--resources`, list a file's
/// resources: type, ID, name, attributes and payload length.
fn disc_command(args: &[String]) -> Result<ExitCode>
{
    let name = required(args, "--iso")?.to_string();
    let description = option(args, "--description").unwrap_or("disc image").to_string();
    let path = option(args, "--path").unwrap_or("");
    let disc = iso::require(&name, &description)?;
    if let Some(spec) = option(args, "--dump")
    {
        let (kind, id) =
            spec.split_once(':')
                .ok_or_else(|| mac_build::error("--dump takes TYPE:ID, for example vers:1"))?;
        let id: i16 = id.parse()?;
        let kind = mac_formats::text::encode(kind)?;
        let kind: [u8; 4] =
            kind.try_into()
                .map_err(|_| mac_build::error("--dump TYPE must be four Mac Roman bytes"))?;
        let Some(resource) =
            iso::read_resources(&disc, path)?.into_iter()
                                             .find(|resource| {
                                                 resource.kind == kind && resource.id == id
                                             })
        else
        {
            return Err(mac_build::error(format!("{path} has no {spec}")));
        };
        println!("{:02x?}", resource.data);
        return Ok(ExitCode::SUCCESS);
    }
    if args.iter().any(|argument| argument == "--resources")
    {
        for entry in iso::resource_listing(&disc, path)?
        {
            println!("{:<5} {:>7}  {:<28} attrs 0x{:02x}  {:>8} bytes",
                     String::from_utf8_lossy(&entry.kind),
                     entry.id,
                     entry.name,
                     entry.attributes,
                     entry.length);
        }
        return Ok(ExitCode::SUCCESS);
    }
    for entry in iso::listing(&disc, path)?
    {
        if entry.directory
        {
            println!("  {}/", entry.name);
        }
        else
        {
            println!("  {:<32} data {:>10}  resources {:>10}",
                     entry.name, entry.data, entry.resources);
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn options<'a>(args: &'a [String], name: &str) -> Vec<&'a str>
{
    args.iter()
        .enumerate()
        .filter(|(_, argument)| *argument == name)
        .filter_map(|(index, _)| args.get(index + 1))
        .map(String::as_str)
        .collect()
}

fn required<'a>(args: &'a [String], name: &str) -> Result<&'a str>
{
    option(args, name).ok_or_else(|| mac_build::error(format!("{name} is required")))
}

fn print_report(report: &Report)
{
    let mut counts: BTreeMap<Verdict, usize> = BTreeMap::new();
    for classification in &report.classifications
    {
        *counts.entry(classification.verdict).or_default() += 1;
    }
    let detail = ReportDetail::of(&report.classifications);
    println!("extract      {:>5}", counts.get(&Verdict::Extract).copied().unwrap_or(0));
    println!("keep ours    {:>5} customised (payload {}, name {}, attributes {})",
             report.classifications
                   .iter()
                   .filter(|entry| matches!(entry.verdict, Verdict::Customised { .. }))
                   .count(),
             detail.payload,
             detail.name,
             detail.attributes);
    println!("keep ours    {:>5} ours-only", counts.get(&Verdict::OursOnly).copied().unwrap_or(0));
    println!("held         {:>5} (identical, but the source is not approved)",
             counts.get(&Verdict::Held).copied().unwrap_or(0));
    println!("native-code  {:>5}", counts.get(&Verdict::NativeCode).copied().unwrap_or(0));
    println!("ignored      {:>5} ISO-side resources we do not use", report.ignored);
}

fn details(report: &Report)
{
    println!();
    for entry in &report.classifications
    {
        let detail = entry.verdict.detail();
        let detail = if detail.is_empty() { String::new() } else { format!(" ({detail})") };
        println!("{:<11} {:<6} {:<5} {} {}{detail}",
                 entry.verdict.reason(),
                 entry.source,
                 entry.kind,
                 entry.id,
                 entry.symbol);
    }
}

struct ReportDetail
{
    payload:    usize,
    name:       usize,
    attributes: usize
}

impl ReportDetail
{
    fn of(classifications: &[Classification]) -> Self
    {
        let mut detail = Self { payload:    0,
                                name:       0,
                                attributes: 0 };
        for entry in classifications
        {
            if let Verdict::Customised { payload,
                                         name,
                                         attributes } = entry.verdict
            {
                detail.payload += usize::from(payload);
                detail.name += usize::from(name);
                detail.attributes += usize::from(attributes);
            }
        }
        detail
    }
}
