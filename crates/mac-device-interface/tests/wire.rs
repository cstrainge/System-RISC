use mac_device_interface::*;
#[test]
fn device_descriptions_and_dma_records_have_checked_fixed_wire_layouts()
{
    let description = Description { handle:         0x1_0001_0001,
                                    bus:            ClassId(*b"VMMI"),
                                    key:            [3; 16],
                                    registers:      0x10004000,
                                    register_bytes: 4096,
                                    interrupt:      Some(4),
                                    flags:          COHERENT_DMA | MMIO };
    let bytes = description.encode();
    assert_eq!(Description::decode(&bytes), Some(description));
    assert_eq!(&bytes[..8], &description.handle.to_le_bytes());
    assert_eq!(&bytes[8..12], b"VMMI");
    assert_eq!(&bytes[28..36], b"\x00\x40\x00\x10\x00\x00\x00\x00");
    assert_eq!(&bytes[48..52], &(COHERENT_DMA | MMIO | INTERRUPT_STATED).to_le_bytes());
    // Absence is a value, not a number: a device the machine states no interrupt
    // for decodes as `None`, and interrupt 0 stays distinguishable from it.
    let silent = Description { interrupt: None,
                               ..description };
    assert_eq!(Description::decode(&silent.encode()), Some(silent));
    let zero = Description { interrupt: Some(0),
                             ..description };
    assert_eq!(Description::decode(&zero.encode()), Some(zero));
    assert_ne!(Description::decode(&silent.encode()), Description::decode(&zero.encode()));
    // A nonzero word without the flag states an interrupt the encoder did not
    // claim, so the encoding disagrees with itself and is refused rather than
    // guessed at. Clearing the flag on a zero word is consistent: it is a device
    // with no interrupt, which is what `silent` already asserts.
    let mut unstated = description.encode();
    unstated[48..52].copy_from_slice(&(COHERENT_DMA | MMIO).to_le_bytes());
    assert_eq!(Description::decode(&unstated), None);
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
    let mapping = Mapping { handle:  0x1_0002_0001,
                            address: 0x8780_1000,
                            length:  4096 };
    let bytes = mapping.encode();
    assert_eq!(Mapping::decode(&bytes), Some(mapping));
    for length in 0..MAPPING_BYTES
    {
        assert!(Mapping::decode(&bytes[..length]).is_none());
    }
    let mut bad = bytes;
    bad[..8].copy_from_slice(&0x20001u64.to_le_bytes());
    assert!(Mapping::decode(&bad).is_none());
    let bad = Mapping { length: u64::MAX,
                        ..mapping };
    assert!(Mapping::decode(&bad.encode()).is_none());
}
