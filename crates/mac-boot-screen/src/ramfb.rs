//! Audited QEMU virt fw_cfg DMA boundary, shared by ROM and System bootstrap.
use core::{arch::asm,
           ptr::{addr_of_mut, read_volatile, write_volatile}};
const FW_CFG: usize = 0x10100000;
unsafe fn select(selector: u16)
{
    unsafe {
        write_volatile((FW_CFG + 8) as *mut u16, selector.to_be());
    }
}
fn cfg_byte() -> u8
{
    unsafe { read_volatile(FW_CFG as *const u8) }
}
fn cfg_u16() -> u16
{
    u16::from_be_bytes([cfg_byte(), cfg_byte()])
}
fn cfg_u32() -> u32
{
    u32::from_be_bytes([cfg_byte(), cfg_byte(), cfg_byte(), cfg_byte()])
}

fn find_file(name: &[u8]) -> Option<(u16, u32)>
{
    unsafe {
        select(0x19);
    }
    let count = cfg_u32();
    if count > 4096
    {
        return None;
    }
    for _ in 0..count
    {
        let length = cfg_u32();
        let selector = cfg_u16();
        let _reserved = cfg_u16();
        let mut filename = [0u8; 56];
        for byte in &mut filename
        {
            *byte = cfg_byte();
        }
        if filename.split(|b| *b == 0).next() == Some(name)
        {
            return Some((selector, length));
        }
    }
    None
}

#[repr(C, align(8))]
struct Dma
{
    control: u32,
    length:  u32,
    address: u64
}

/// # Safety
/// The address names pinned guest RAM, writable for width*height*4 bytes for the
/// display lifetime. Caller serializes fw_cfg access during single-hart boot.
pub unsafe fn configure(address: u64, width: usize, height: usize) -> bool
{
    unsafe {
        select(0);
    }
    assert_eq!([cfg_byte(), cfg_byte(), cfg_byte(), cfg_byte()], *b"QEMU");
    assert_eq!(u64::from_be(unsafe { read_volatile((FW_CFG + 16) as *const u64) }),
               0x5145_4d55_2043_4647);
    let Some((selector, length)) = find_file(b"etc/ramfb")
    else
    {
        return false;
    };
    assert_eq!(length, 28);
    // RAMFBCfg is packed, big endian. XR24 = XRGB8888, pixels in little-endian RAM.
    let mut config = [0u8; 28];
    config[0..8].copy_from_slice(&address.to_be_bytes());
    config[8..12].copy_from_slice(&u32::from_le_bytes(*b"XR24").to_be_bytes());
    config[16..20].copy_from_slice(&(width as u32).to_be_bytes());
    config[20..24].copy_from_slice(&(height as u32).to_be_bytes());
    config[24..28].copy_from_slice(&((width * 4) as u32).to_be_bytes());
    let mut dma = Dma { control: ((u32::from(selector) << 16) | 8 | 16).to_be(),
                        length:  28u32.to_be(),
                        address: (config.as_ptr() as u64).to_be() };
    unsafe {
        asm!("fence iorw, iorw", options(nostack));
        write_volatile((FW_CFG + 16) as *mut u64, (addr_of_mut!(dma) as u64).to_be());
        let mut pending = 100_000;
        loop
        {
            let control = u32::from_be(read_volatile(addr_of_mut!(dma.control)));
            assert_eq!(control & 1, 0, "framebuffer DMA failed");
            if pending == 0
            {
                return false;
            }
            pending -= 1;
            if control == 0
            {
                break;
            }
        }
        asm!("fence iorw, iorw", options(nostack));
    }
    true
}
