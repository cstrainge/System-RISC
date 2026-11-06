//! Scratch: check the record against a machine's own tree, without building anything.
use mac_build::{devicetree, platform};

fn main()
{
    let mut args = std::env::args().skip(1);
    let name = args.next().expect("usage: cross_check <platform> <dtb>");
    let path = args.next().expect("usage: cross_check <platform> <dtb>");
    let platform = platform::load(&name).expect("record loads");
    let blob = std::fs::read(&path).expect("read");
    let check = devicetree::compare(&platform, &blob).expect("compare");
    println!("expected ({}):", check.expected.len());
    for finding in &check.expected
    {
        println!("  {}: {}", finding.model, finding.detail);
    }
    println!("warnings ({}):", check.warnings.len());
    for finding in &check.warnings
    {
        println!("  {}: {}", finding.model, finding.detail);
    }
}
