extern crate std;
use alloc::{vec, vec::Vec};
use std::{fs::File,
          io::{Read, Seek, SeekFrom},
          path::PathBuf};

use super::*;
fn put16(b: &mut [u8], p: usize, n: u16)
{
    b[p..p + 2].copy_from_slice(&n.to_be_bytes());
}
fn put32(b: &mut [u8], p: usize, n: u32)
{
    b[p..p + 4].copy_from_slice(&n.to_be_bytes());
}
fn record(parent: u32, name: &[u8], data: &[u8]) -> Vec<u8>
{
    let key = (6 + name.len()) | 1;
    let mut b = vec![0; key + 1];
    b[0] = key as u8;
    put32(&mut b, 2, parent);
    b[6] = name.len() as u8;
    b[7..7 + name.len()].copy_from_slice(name);
    b.extend_from_slice(data);
    b
}
fn folder(id: u32) -> Vec<u8>
{
    let mut b = vec![0; 70];
    b[0] = 1;
    put32(&mut b, 6, id);
    put16(&mut b, 4, 1);
    put32(&mut b, 10, 12345);
    b[22..54].copy_from_slice(&[0xa7; 32]);
    b
}
fn node(records: &[Vec<u8>]) -> Vec<u8>
{
    let mut b = vec![0; 512];
    b[8] = 0xff;
    put16(&mut b, 10, records.len() as u16);
    let mut offset = 14;
    for (i, r) in records.iter().enumerate()
    {
        put16(&mut b, 510 - i * 2, offset as u16);
        b[offset..offset + r.len()].copy_from_slice(r);
        offset += r.len();
    }
    put16(&mut b, 510 - records.len() * 2, offset as u16);
    b
}
fn disk() -> Vec<u8>
{
    let mut b = vec![0; 64 * 512];
    let m = &mut b[1024..1536];
    m[..2].copy_from_slice(b"BD");
    put32(m, 2, 42);
    put32(m, 6, 99);
    put16(m, 18, 58);
    put32(m, 20, 512);
    put16(m, 28, 4);
    put16(m, 34, 20);
    m[36] = 4;
    m[37..41].copy_from_slice(b"Test");
    put32(m, 92, 16);
    put32(m, 146, 1024);
    put16(m, 150, 0);
    put16(m, 152, 2);
    let h = &mut b[2048..2560];
    h[8] = 1;
    put32(h, 24, 1);
    put16(h, 32, 512);
    put32(h, 36, 2);
    let mut f = vec![0; 102];
    f[0] = 2;
    f[2] = 1;
    f[4..12].copy_from_slice(b"TEXTTEST");
    put32(&mut f, 20, 19);
    put32(&mut f, 26, 600);
    put32(&mut f, 36, 7);
    put32(&mut f, 44, 555);
    f[56..72].copy_from_slice(&[0x31; 16]);
    put16(&mut f, 74, 6);
    put16(&mut f, 76, 2);
    put16(&mut f, 86, 10);
    put16(&mut f, 88, 1);
    b[2560..3072].copy_from_slice(&node(&[record(1, b"Test", &folder(2)),
                                          record(2, b"Folder", &folder(16)),
                                          record(16, b"Caf\x8e", &f)]));
    for i in 0..600
    {
        b[10 * 512 + i] = (i % 251) as u8;
    }
    b[14 * 512..14 * 512 + 7].copy_from_slice(b"RESFORK");
    b
}
struct Context
{
    disks: Vec<Vec<u8>>
}
unsafe extern "C" fn info(context: *mut (), index: u32, out: *mut DiskInfo) -> Status
{
    let context = unsafe { &mut *context.cast::<Context>() };
    let Some(d) = context.disks.get(index as usize)
    else
    {
        return NOT_FOUND;
    };
    unsafe {
        *out = DiskInfo { sector_count: d.len() as u64 / 512,
                          read_only:    1,
                          icon_kind:    *b"ICN#",
                          icon_id:      24000,
                          reserved:     0 }
    };
    OK
}
unsafe extern "C" fn read(context: *mut (), index: u32, sector: u64, out: *mut u8) -> Status
{
    let context = unsafe { &mut *context.cast::<Context>() };
    let Some(d) = context.disks.get(index as usize)
    else
    {
        return NOT_FOUND;
    };
    let Some(bytes) = d.get(sector as usize * 512..sector as usize * 512 + 512)
    else
    {
        return BAD_ARGUMENT;
    };
    unsafe { core::ptr::copy_nonoverlapping(bytes.as_ptr(), out, 512) };
    OK
}
fn manager(context: &mut Context) -> Manager
{
    Manager::new(BootStorage { context:                 (context as *mut Context).cast(),
                               disk_info:               info,
                               read_sector:             read,
                               write_sector:            None,
                               flush_disk:              None,
                               startup_disk:            1,
                               startup_partition_start: 8 }).unwrap()
}
fn volume_id(manager: &Manager, index: usize) -> u32
{
    manager.volumes.iter().nth(index).unwrap().0
}
/// Unit fixtures address their generated volumes by one-based ordinal here;
/// actual requests always carry the generation-tagged ID issued by the manager.
fn call(manager: &mut Manager, operation: u32, ordinal: u32, id: u32) -> FileCall
{
    let volume = if ordinal == 0 { 0 } else { volume_id(manager, (ordinal - 1) as usize) };
    let mut c = FileCall { operation,
                           volume,
                           cnid: id,
                           ..Default::default() };
    manager.request(&mut c, "", &mut []).unwrap();
    c
}
#[test]
fn discovers_every_disk_partition_and_preserves_raw_metadata()
{
    let original = disk();
    let mut apm = vec![0; 140 * 512];
    for (i, start) in [8, 76].into_iter().enumerate()
    {
        let p = &mut apm[(i + 1) * 512..(i + 2) * 512];
        p[..2].copy_from_slice(b"PM");
        put32(p, 4, 2);
        put32(p, 8, start as u32);
        put32(p, 12, 64);
        p[48..57].copy_from_slice(b"Apple_HFS");
        apm[start * 512..(start + 64) * 512].copy_from_slice(&original);
    }
    let mut context = Context { disks: vec![original, apm, vec![0; 4096]] };
    let mut m = manager(&mut context);
    assert_eq!(call(&mut m, VOLUME_COUNT, 0, 0).index, 3);
    let v = call(&mut m, VOLUME_INFO, 2, 0).volume_info;
    assert_eq!(v.name(), "Test");
    assert_eq!(v.startup, 1);
    assert_eq!(v.icon_kind, *b"ICN#");
    assert_eq!(v.icon_id, 24000);
    assert_eq!(v.disk, 1);
    assert_eq!(v.partition_start, 8);
    assert_eq!(v.total_bytes, 58 * 512);
    assert_eq!(v.free_bytes, 20 * 512);
    assert_eq!(v.created, 42_000_000);
    let d = call(&mut m, CATALOG, 1, 16).entry;
    assert!(d.is_directory());
    assert_eq!(d.finder_info, [0xa7; 32]);
    assert_eq!(d.created, 12_345_000_000);
    let f = call(&mut m, CATALOG, 1, 19).entry;
    assert_eq!(f.name(), "Café");
    assert_eq!(f.file_type, *b"TEXT");
    assert_eq!(f.creator, *b"TEST");
    assert_eq!(f.flags, 1);
    assert_eq!(f.created, 555_000_000);
    assert_eq!(&f.finder_info[16..], &[0x31; 16]);
    assert_eq!((f.data_length, f.resource_length), (600, 7));
}
#[test]
fn traverses_parent_utf8_lookup_paths_and_rejects_bad_inputs()
{
    let mut context = Context { disks: vec![disk()] };
    let mut m = manager(&mut context);
    let mut c = FileCall { operation: LOOKUP,
                           volume: volume_id(&m, 0),
                           ..Default::default() };
    m.request(&mut c, "/folder/./CAFÉ", &mut []).unwrap();
    assert_eq!(c.entry.id, 19);
    assert_eq!(call(&mut m, PARENT, 1, 19).entry.id, 16);
    assert_eq!(call(&mut m, PARENT, 1, 2).entry.id, 2);
    let mut path = FileCall { operation: PATH,
                              volume: volume_id(&m, 0),
                              cnid: 19,
                              ..Default::default() };
    let mut output = [0xa5; 80];
    m.request(&mut path, "", &mut output).unwrap();
    assert_eq!(core::str::from_utf8(&output[..path.transferred]).unwrap(), "/Folder/Café");
    assert_eq!(output[path.transferred], 0xa5);
    assert_eq!(m.request(&mut path, "", &mut [0; 2]), Err(NO_MEMORY));
    assert_eq!(path.length, 13);
    c.cnid = 16;
    m.request(&mut c, "../Folder/../Folder/Café", &mut []).unwrap();
    assert_eq!(c.entry.id, 19);
    for path in ["Café/child", "Café/", "Café/.."]
    {
        assert_eq!(m.request(&mut c, path, &mut []), Err(BAD_ARGUMENT));
    }
    assert_eq!(m.request(&mut c, "absent", &mut []), Err(NOT_FOUND));
    assert_eq!(m.request(&mut c, "🦀", &mut []), Err(BAD_ARGUMENT));
    let mut child = FileCall { operation: DIRECTORY_ENTRY,
                               volume: volume_id(&m, 0),
                               cnid: 16,
                               index: 1,
                               ..Default::default() };
    assert_eq!(m.request(&mut child, "", &mut []), Err(NOT_FOUND));
    child.index = 0;
    m.request(&mut child, "", &mut []).unwrap();
    assert_eq!(child.entry.id, 19);
}
#[test]
fn independent_fork_marks_seek_eof_close_stale_handles_and_capacity()
{
    let mut context = Context { disks: vec![disk()] };
    let mut m = manager(&mut context);
    let a = call(&mut m, OPEN, 1, 19).handle;
    let b = call(&mut m, OPEN, 1, 19).handle;
    let mut c = FileCall { operation: READ,
                           handle: a,
                           ..Default::default() };
    let mut bytes = [0xa5; 530];
    m.request(&mut c, "", &mut bytes).unwrap();
    assert_eq!(c.transferred, 530);
    assert_eq!(bytes, (0..530).map(|n| (n % 251) as u8).collect::<Vec<_>>().as_slice());
    assert_eq!(call_handle(&mut m, TELL, b).position, 0);
    c.operation = SEEK;
    c.whence = SEEK_END;
    c.offset = -4;
    m.request(&mut c, "", &mut []).unwrap();
    assert_eq!(c.position, 596);
    c.operation = READ;
    bytes.fill(0xa5);
    m.request(&mut c, "", &mut bytes).unwrap();
    assert_eq!(c.transferred, 4);
    assert_eq!(bytes[4], 0xa5);
    m.request(&mut c, "", &mut bytes).unwrap();
    assert_eq!(c.transferred, 0);
    c.operation = SEEK;
    c.offset = 1;
    c.whence = SEEK_END;
    assert_eq!(m.request(&mut c, "", &mut []), Err(BAD_ARGUMENT));
    assert_eq!(call_handle(&mut m, TELL, a).position, 600);
    call_handle(&mut m, CLOSE, a);
    assert_eq!(m.request(&mut c, "", &mut []), Err(NOT_FOUND));
    let newer = call(&mut m, OPEN, 1, 19).handle;
    assert_ne!(newer, a);
    let mut resource = FileCall { operation: OPEN,
                                  volume: volume_id(&m, 0),
                                  cnid: 19,
                                  fork: RESOURCE_FORK,
                                  ..Default::default() };
    m.request(&mut resource, "", &mut []).unwrap();
    resource.operation = READ;
    m.request(&mut resource, "", &mut bytes).unwrap();
    assert_eq!(&bytes[..resource.transferred], b"RESFORK");
    while m.handles.iter().count() < MAX_OPEN_FILES
    {
        call(&mut m, OPEN, 1, 19);
    }
    let mut overflow = FileCall { operation: OPEN,
                                  volume: volume_id(&m, 0),
                                  cnid: 19,
                                  ..Default::default() };
    assert_eq!(m.request(&mut overflow, "", &mut []), Err(NO_MEMORY));
    overflow.cnid = 16;
    assert_eq!(m.request(&mut overflow, "", &mut []), Err(BAD_ARGUMENT));
}
fn call_handle(m: &mut Manager, operation: u32, handle: u32) -> FileCall
{
    let mut c = FileCall { operation,
                           handle,
                           ..Default::default() };
    m.request(&mut c, "", &mut []).unwrap();
    c
}
struct HostDisk(File, u64);
impl BlockDevice for HostDisk
{
    fn sector_count(&self) -> u64
    {
        self.1 / 512
    }

    fn read_sector(&mut self, sector: u64, out: &mut [u8; 512]) -> mac_hfs::Result<()>
    {
        self.0.seek(SeekFrom::Start(sector * 512)).map_err(|_| mac_hfs::Error("seek"))?;
        self.0.read_exact(out).map_err(|_| mac_hfs::Error("read"))
    }
}
/// Optional actual image test; enabled explicitly so clean checkouts remain fixture driven.
#[test]
#[ignore = "set SYSTEM7_TEST_HFS to a generated native or original HFS image"]
fn actual_disk_catalog_and_both_forks()
{
    let path = PathBuf::from(std::env::var_os("SYSTEM7_TEST_HFS").expect("SYSTEM7_TEST_HFS"));
    let f = File::open(path).unwrap();
    let n = f.metadata().unwrap().len();
    let mut d = HostDisk(f, n);
    let p = mac_hfs::partitions(&mut d).unwrap()[0];
    let mut v = Volume::mount(&mut d, p).unwrap();
    let folder = v.blessed_folder().unwrap();
    let entry = v.lookup(folder.id, b"System").unwrap().unwrap();
    assert!(!entry.directory);
    assert_eq!(entry.parent, folder.id);
    assert!(entry.resources.as_ref().unwrap().length > 1024);
    for mut fork in [entry.data.unwrap(), entry.resources.unwrap()]
    {
        let mut b = [0; 4096];
        let mut offset = 0;
        loop
        {
            let n = v.read(&mut fork, offset, &mut b).unwrap();
            if n == 0
            {
                break;
            }
            offset += n as u64;
        }
        assert_eq!(offset, fork.length);
    }
    assert_eq!(v.entry(entry.id).unwrap().unwrap().id, entry.id);
    // Repeat through the resident manager, not just the HFS parser. Sector reads
    // stay streamed from this immutable host file; no 500 MiB disk copy is made.
    let storage = BootStorage { context:                 (&mut d as *mut HostDisk).cast(),
                                disk_info:               host_info,
                                read_sector:             host_read,
                                write_sector:            None,
                                flush_disk:              None,
                                startup_disk:            0,
                                startup_partition_start: p.start };
    let mut manager = Manager::new(storage).unwrap();
    assert_eq!(call(&mut manager, VOLUME_COUNT, 0, 0).index, 1);
    assert_eq!(call(&mut manager, VOLUME_INFO, 1, 0).volume_info.startup, 1);
    let mut open = FileCall { operation: OPEN,
                              volume: volume_id(&manager, 0),
                              cnid: entry.id,
                              fork: RESOURCE_FORK,
                              ..Default::default() };
    manager.request(&mut open, "", &mut []).unwrap();
    open.operation = READ;
    let mut resident_bytes = Vec::new();
    let mut chunk = [0; 4096];
    loop
    {
        manager.request(&mut open, "", &mut chunk).unwrap();
        if open.transferred == 0
        {
            break;
        }
        resident_bytes.extend_from_slice(&chunk[..open.transferred]);
    }
    assert_eq!(resident_bytes.len() as u64, open.length);
    if let Some(expected) = std::env::var_os("SYSTEM7_TEST_SYSTEM_RSRC")
    {
        assert_eq!(resident_bytes, std::fs::read(expected).unwrap());
    }
    open.operation = CLOSE;
    manager.request(&mut open, "", &mut []).unwrap();
}

struct ApiFixture
{
    // Keep callback context at a stable address until after the manager is dropped.
    manager:  Manager,
    _context: alloc::boxed::Box<Context>
}
std::thread_local! {
    static API: core::cell::RefCell<ApiFixture> = core::cell::RefCell::new({
        let mut context = alloc::boxed::Box::new(Context {disks:vec![disk()]});
        let manager = manager(&mut context);
        ApiFixture {manager,_context:context}
    });
}
unsafe extern "C" fn file_gate(number: u32, request: *mut (), size: usize) -> i64
{
    if number != mac_abi::traps::FILE || size != core::mem::size_of::<FileCall>()
    {
        return BAD_ARGUMENT as i64;
    }
    let call = unsafe { &mut *request.cast::<FileCall>() };
    if call.name_length > 4096 || call.capacity > 1024 * 1024
    {
        return BAD_ARGUMENT as i64;
    }
    let name = unsafe { core::slice::from_raw_parts(call.name, call.name_length) };
    let Ok(name) = core::str::from_utf8(name)
    else
    {
        return BAD_ARGUMENT as i64;
    };
    let output = unsafe { core::slice::from_raw_parts_mut(call.output, call.capacity) };
    API.with(|fixture| fixture.borrow_mut().manager.request(call, name, output))
       .map_or_else(|s| s as i64, |_| 0)
}
#[test]
fn public_safe_client_crosses_actual_filecall_and_drop_releases_handles()
{
    use toolbox_files::{Files, Fork, SeekFrom};
    let services = SystemServices { header:       Header::for_type::<SystemServices>(),
                                    trap:         file_gate,
                                    capabilities: 0 };
    let client = mac_traps::Client::new(&services).unwrap();
    let files = Files::new(client);
    assert_eq!(files.volume_count().unwrap(), 1);
    let volume = files.volume_at(0).unwrap();
    assert_eq!(volume.name(), "Test");
    assert_eq!(files.volume(volume.id).unwrap(), volume);
    assert_eq!(files.entry(volume.id, 2, 0).unwrap().name(), "Folder");
    let entry = files.lookup(volume.id, 2, "Folder/Café").unwrap();
    assert_eq!(files.catalog(volume.id, entry.id).unwrap(), entry);
    assert_eq!(files.parent(volume.id, entry.id).unwrap().id, 16);
    let mut path = alloc::string::String::new();
    files.path(volume.id, entry.id, &mut path).unwrap();
    assert_eq!(path, "/Folder/Café");
    {
        let mut data = files.open(volume.id, entry.id, Fork::Data).unwrap();
        let mut resource = files.open(volume.id, entry.id, Fork::Resource).unwrap();
        let mut bytes = [0xa5; 16];
        assert_eq!(data.len().unwrap(), 600);
        assert!(!data.is_empty().unwrap());
        assert_eq!(resource.read(&mut bytes).unwrap(), 7);
        assert_eq!(&bytes[..7], b"RESFORK");
        assert_eq!(bytes[7], 0xa5);
        data.seek(SeekFrom::Start(510)).unwrap();
        assert_eq!(data.read(&mut bytes).unwrap(), 16);
        assert_eq!(bytes, (510..526).map(|n| (n % 251) as u8).collect::<Vec<_>>().as_slice());
        assert_eq!(data.seek(SeekFrom::Current(-6)).unwrap(), 520);
        assert_eq!(data.seek(SeekFrom::End(-1)).unwrap(), 599);
        assert_eq!(data.position().unwrap(), 599);
        resource.close().unwrap();
    }
    API.with(|fixture| assert!(fixture.borrow().manager.handles.iter().next().is_none()));
    // Untrusted scalar fields do not let safe callers supply unrelated raw pointers.
    let mut raw = FileCall { operation: LOOKUP,
                             volume: volume.id,
                             cnid: 2,
                             name: core::ptr::dangling(),
                             name_length: usize::MAX,
                             output: core::ptr::dangling_mut(),
                             capacity: usize::MAX,
                             ..Default::default() };
    client.file(&mut raw, "Folder/Café", &mut []).unwrap();
    assert_eq!(raw.entry.id, 19);
    assert!(raw.name.is_null());
    assert!(raw.output.is_null());
    assert_eq!((raw.name_length, raw.capacity), (0, 0));
    raw.operation = LOOKUP;
    assert_eq!(client.file(&mut raw, "absent", &mut []), Err(mac_traps::Error(NOT_FOUND)));
    assert!(raw.name.is_null());
    assert!(raw.output.is_null());
}

unsafe extern "C" fn host_info(context: *mut (), index: u32, output: *mut DiskInfo) -> Status
{
    if index != 0
    {
        return NOT_FOUND;
    }
    let disk = unsafe { &*context.cast::<HostDisk>() };
    unsafe {
        *output = DiskInfo { sector_count: disk.sector_count(),
                             read_only: 1,
                             reserved: 0,
                             ..Default::default() };
    }
    OK
}
unsafe extern "C" fn host_read(context: *mut (), index: u32, sector: u64, output: *mut u8)
                               -> Status
{
    if index != 0
    {
        return NOT_FOUND;
    }
    let disk = unsafe { &mut *context.cast::<HostDisk>() };
    let buffer = unsafe { &mut *output.cast::<[u8; 512]>() };
    disk.read_sector(sector, buffer).map_or(NOT_SUPPORTED, |_| OK)
}

#[test]
fn bounded_discovery_empty_system_invalid_ids_and_parent_cycles_are_explicit()
{
    let mut empty = Manager::default();
    assert_eq!(call(&mut empty, VOLUME_COUNT, 0, 0).index, 0);
    let mut query = FileCall { operation: CATALOG,
                               volume: 0,
                               cnid: 2,
                               ..Default::default() };
    assert_eq!(empty.request(&mut query, "", &mut []), Err(NOT_FOUND));
    query.operation = u32::MAX;
    assert_eq!(empty.request(&mut query, "", &mut []), Err(NOT_SUPPORTED));
    let mut context = Context { disks: vec![disk(); MAX_DISKS as usize + 1] };
    let storage = BootStorage { context:                 (&mut context as *mut Context).cast(),
                                disk_info:               info,
                                read_sector:             read,
                                write_sector:            None,
                                flush_disk:              None,
                                startup_disk:            0,
                                startup_partition_start: 0 };
    assert!(matches!(Manager::new(storage), Err(NO_MEMORY)));
    let mut cyclic = disk();
    // Second catalog record is the folder; point its parent key back to itself.
    let second_record = 2560 + 14 + record(1, b"Test", &folder(2)).len();
    put32(&mut cyclic, second_record + 2, 16);
    let mut context = Context { disks: vec![cyclic] };
    let mut manager = manager(&mut context);
    let mut path = FileCall { operation: PATH,
                              volume: volume_id(&manager, 0),
                              cnid: 16,
                              ..Default::default() };
    assert_eq!(manager.request(&mut path, "", &mut [0; 128]), Err(NOT_SUPPORTED));
    path.operation = LOOKUP;
    path.cnid = 2;
    assert_eq!(manager.request(&mut path, &"a".repeat(MAX_PATH + 1), &mut []), Err(BAD_ARGUMENT));
}
