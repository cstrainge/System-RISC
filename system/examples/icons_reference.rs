//! Exercise every bundled original icon and reproduce the original Icon Utilities probe.
use std::{collections::BTreeSet,
          env, fs,
          io::{BufWriter, Write}};

use mac_formats::{macbinary,
                  resource::{self, Resource}};
use mac_system::toolbox::quickdraw::{Canvas, Rect,
                                     icons::{IconFamily, IconFlags, decode_bundle, decode_fref}};

fn load(source: &str, bundled: bool) -> Vec<Resource>
{
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).unwrap();
    let dist = mac_build::paths::dist_dir(root);
    let mut resources = if source == "Synthetic"
    {
        let mut result = Vec::new();
        for (id, depth, kind) in [(30000, 8, *b"icl8"), (30001, 4, *b"icl4")]
        {
            let mut mono = vec![0; 256];
            mono[128..].fill(255);
            result.push(Resource { kind: *b"ICN#",
                                   id,
                                   name: None,
                                   attributes: 0,
                                   data: mono });
            let mut data = vec![0; 32 * 32 * depth / 8];
            for y in 0..32
            {
                for x in 0..32
                {
                    let index = if depth == 8 { (y / 2) * 16 + x / 2 } else { x / 2 };
                    let bit = x * depth;
                    data[y * 32 * depth / 8 + bit / 8] |= (index as u8) << (8 - depth - (bit & 7));
                }
            }
            result.push(Resource { kind,
                                   id,
                                   name: None,
                                   attributes: 0,
                                   data });
        }
        result
    }
    else if bundled
    {
        resource::decode(&fs::read(dist.join(format!("{source}.rsrc"))).unwrap()).unwrap()
    }
    else
    {
        let file = macbinary::decode(&fs::read(format!("working/icon-reference/{source}.bin")).unwrap()).unwrap();
        resource::decode(&file.resources).unwrap()
    };
    let bundled_system = if bundled
    {
        resource::decode(&fs::read(dist.join("System.rsrc")).unwrap()).unwrap()
    }
    else
    {
        Vec::new()
    };
    for id in [4, 8]
    {
        let data = if bundled
        {
            bundled_system.iter()
                          .find(|r| r.kind == *b"clut" && r.id == id)
                          .expect("bundled ROM palette")
                          .data
                          .clone()
        }
        else
        {
            fs::read(format!("System7/system/assets/visual/636c7574-{id}.bin")).unwrap()
        };
        resources.push(Resource { kind: *b"clut",
                                  id,
                                  name: None,
                                  attributes: 0,
                                  data });
    }
    resources
}
fn main()
{
    let args: Vec<_> = env::args().collect();
    let bundled = args.iter().any(|a| a == "--bundled");
    if args.get(1).is_some_and(|s| s == "--validate")
    {
        let mut total = 0;
        for source in ["System", "Finder"]
        {
            let resources = load(source, bundled);
            let ids: BTreeSet<_> = resources.iter()
                                            .filter(|r| {
                                                matches!(&r.kind,
                                                         b"ICN#"
                                                         | b"ics#"
                                                         | b"ICON"
                                                         | b"SICN"
                                                         | b"cicn"
                                                         | b"icl4"
                                                         | b"icl8"
                                                         | b"ics4"
                                                         | b"ics8")
                                            })
                                            .map(|r| r.id)
                                            .collect();
            for resource in &resources
            {
                match &resource.kind
                {
                    b"BNDL" =>
                    {
                        decode_bundle(&resource.data).unwrap_or_else(|e| {
                                                         panic!("{source} BNDL {}: {e:?}",
                                                                resource.id)
                                                     });
                    }
                    b"FREF" =>
                    {
                        decode_fref(&resource.data).unwrap_or_else(|e| {
                                                       panic!("{source} FREF {}: {e:?}",
                                                              resource.id)
                                                   });
                    }
                    _ =>
                    {}
                }
            }
            let borrowed: Vec<_> = resources.iter()
                                            .map(|r| {
                                                resource::ResourceRef { kind:       r.kind,
                                                                        id:         r.id,
                                                                        name:       r.name
                                                                                     .as_deref(),
                                                                        attributes: r.attributes,
                                                                        data:       &r.data }
                                            })
                                            .collect();
            let keyboard_ids: BTreeSet<_> =
                resources.iter().filter(|r| r.kind == *b"kcs#").map(|r| r.id).collect();
            let mut keyboard_images = 0;
            for id in &keyboard_ids
            {
                keyboard_images +=
                    IconFamily::keyboard_from_resource_refs(&borrowed, *id).unwrap().images.len();
            }
            let mut images = 0;
            for id in &ids
            {
                let family = IconFamily::from_resources(&resources, *id).unwrap_or_else(|e| {
                                                                            panic!("{source} icon \
                                                                                    {id}: {e:?}")
                                                                        });
                images += family.images.len();
            }
            total += images + keyboard_images;
            println!("{source}: {} ordinary families, {images} images; {} keyboard families, \
                      {keyboard_images} images",
                     ids.len(),
                     keyboard_ids.len());
        }
        println!("Total {total} decoded images");
        return;
    }
    let source = args.get(1).expect("SOURCE OUTPUT");
    let resources = load(source, bundled);
    let ids: &[i16] = match source.as_str()
    {
        "System" => &[-3984, -3983, -3982, -3998, -3996, 3],
        "Finder" => &[12500, 14000, 14500],
        "Synthetic" => &[30000, 30001],
        _ => panic!("unknown source")
    };
    let mut pixels = vec![0xffffff; 640 * 460];
    for (y, n) in [(455, 1), (456, 1), (457, 2), (458, 3), (459, 5)]
    {
        pixels[y * 640..y * 640 + n].fill(0);
        pixels[y * 640 + 640 - n..(y + 1) * 640].fill(0);
    }
    let mut canvas = Canvas::new(&mut pixels, 640, 460);
    let mut cell = 0;
    for id in ids
    {
        let family = IconFamily::from_resources(&resources, *id).unwrap();
        for size in [16, 32, 48]
        {
            for transform in match args.get(3).map(String::as_str)
            {
                Some("combined") => &[0x4001][..],
                Some("open") => &[3][..],
                Some("selected-open") => &[0x4003][..],
                _ => &[0, 0x4000, 1][..]
            }
            {
                let phase = if source == "System"
                               && matches!(args.get(3).map(String::as_str),
                                           Some("open" | "selected-open"))
                {
                    cell % 4
                }
                else
                {
                    0
                };
                let x = 12 + (cell % 8) * 72 + (48 - size) / 2 + phase;
                let y = 12 + (cell / 8) * 60 + (48 - size) / 2 + phase;
                let flags = IconFlags::from_transform(*transform).unwrap();
                // Unsupported transformations are left blank until original behavior is
                // implemented.
                if let Err(error) =
                    family.draw(&mut canvas, Rect::new(x, y, x + size, y + size), flags)
                {
                    eprintln!("{source} {id} {size} {transform}: {error:?}");
                }
                cell += 1;
            }
        }
    }
    let mut file = BufWriter::new(fs::File::create(&args[2]).unwrap());
    writeln!(file, "P6\n640 460\n255").unwrap();
    for p in pixels
    {
        file.write_all(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]).unwrap();
    }
}
