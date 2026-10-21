use mac_device_interface::*;
#[test]
fn device_descriptions_and_dma_records_have_checked_fixed_wire_layouts()
{
    let description = Description { handle:         0x10001,
                                    bus:            ClassId(*b"VMMI"),
                                    key:            [3; 16],
                                    registers:      0x10004000,
                                    register_bytes: 4096,
                                    interrupt:      4,
                                    flags:          COHERENT_DMA | MMIO };
    let bytes = description.encode();
    assert_eq!(Description::decode(&bytes), Some(description));
    assert_eq!(&bytes[..8], b"\x01\x00\x01\x00VMMI");
    assert_eq!(&bytes[24..32], b"\x00\x40\x00\x10\x00\x00\x00\x00");
    for length in 0..DESCRIPTION_BYTES
    {
        assert!(Description::decode(&bytes[..length]).is_none());
    }
    let mut bad = description;
    bad.flags = u32::MAX;
    assert!(Description::decode(&bad.encode()).is_none());
    bad = description;
    bad.registers = u64::MAX;
    assert!(Description::decode(&bad.encode()).is_none());
    let mapping = Mapping { handle:  0x20001,
                            address: 0x8780_1000,
                            length:  4096 };
    let bytes = mapping.encode();
    assert_eq!(Mapping::decode(&bytes), Some(mapping));
    for length in 0..MAPPING_BYTES
    {
        assert!(Mapping::decode(&bytes[..length]).is_none());
    }
    let mut bad = bytes;
    bad[4] = 1;
    assert!(Mapping::decode(&bad).is_none());
    let bad = Mapping { length: u64::MAX,
                        ..mapping };
    assert!(Mapping::decode(&bad.encode()).is_none());
}
