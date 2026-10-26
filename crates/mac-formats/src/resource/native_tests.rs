use alloc::string::ToString;

use super::{index::{Index, MapLayout},
            loader::Loader,
            stream::Budget,
            *};

fn sample(id: i16, bytes: usize) -> Resource
{
    Resource { kind: *b"DATA",
               id,
               name: Some(vec![0x8e]),
               attributes: 0x28,
               data: vec![id as u8; bytes] }
}
fn layout(bytes: &[u8]) -> MapLayout
{
    MapLayout::decode(bytes[..16].try_into().unwrap(), bytes.len() as u64,
        Budget::default()).unwrap()
}

#[test]
fn native_offsets_cross_24_bits_in_borrowed_indexed_and_both_streamed_readers()
{
    let resources = [sample(0, 1 << 24), sample(1, 7)];
    assert!(matches!(encode(&resources), Err(Error::Malformed(message)) if message.contains("24 bits")));
    let bytes = encode_native(&resources).unwrap();
    assert_eq!(encoding(&bytes, bytes.len() as u64).unwrap(), Encoding::Native);
    let borrowed = decode_borrowed(&bytes).unwrap();
    assert_eq!(borrowed[1].data, resources[1].data);
    assert_eq!(borrowed[1].name, Some(&[0x8e][..]));
    let map = layout(&bytes);
    let index = Index::decode(map, bytes[map.map_offset() as usize..].to_vec()).unwrap();
    assert_eq!(index.encoding(), Encoding::Native);
    assert_eq!(index.get(*b"DATA", 1).unwrap().offset, 256 + (1 << 24) + 4);
    let mut transferred = 0;
    let read = stream::read_selected(bytes.len() as u64,
                                     Budget::default(),
                                     |_, id| id == 1,
                                     |offset, output| {
                                         transferred += output.len();
                                         output.copy_from_slice(&bytes[offset as usize..]
                                                                    [..output.len()]);
                                         Ok(())
                                     }).unwrap();
    assert_eq!(read, [resources[1].clone()]);
    assert!(transferred < 1024, "unselected large payload was read");
    let mut loader =
        Loader::selected(bytes.len() as u64, Budget::default(), |_, id| id == 1).unwrap();
    let mut transferred = 0;
    while let Some((offset, count)) = loader.request()
    {
        transferred += count;
        loader.received(&bytes[offset as usize..][..count]).unwrap();
    }
    assert_eq!(loader.finish().unwrap(), read);
    assert!(transferred < 1024);
    let error = stream::read_selected(bytes.len() as u64,
                                      Budget::default(),
                                      |_, _| true,
                                      |offset, output| {
                                          output.copy_from_slice(&bytes[offset as usize..]
                                                                     [..output.len()]);
                                          Ok(())
                                      }).unwrap_err();
    assert!(matches!(error, Error::BudgetExceeded(_)));
    assert!(error.to_string().contains("payload exceeds budget"));
}

#[test]
fn classic_handle_bytes_are_never_part_of_the_data_offset()
{
    let resource = sample(-1, 8);
    let mut bytes = encode(&[resource.clone()]).unwrap();
    let map = layout(&bytes);
    let at = map.map_offset() as usize;
    let types = at + usize::from(crate::be16(&bytes, at + 24).unwrap());
    let refs = types + usize::from(crate::be16(&bytes, types + 8).unwrap());
    bytes[refs + 8..refs + 12].copy_from_slice(&[0xff, 0xde, 0xad, 0xbe]);
    assert_eq!(encoding(&bytes, bytes.len() as u64).unwrap(), Encoding::Classic);
    assert_eq!(decode(&bytes).unwrap(), [resource]);
    let index = Index::decode(map, bytes[at..].to_vec()).unwrap();
    assert_eq!(index.get(*b"DATA", -1).unwrap().offset, 256);
}

#[test]
fn native_marker_lengths_and_full_offsets_are_validated_before_payload_access()
{
    let bytes = encode_native(&[sample(7, 4)]).unwrap();
    let mut bad = bytes.clone();
    bad[7] = b'2';
    let error = decode_borrowed(&bad).unwrap_err();
    assert!(matches!(error, Error::Malformed(_)));
    assert!(error.to_string().contains("rebuild or recreate"));
    let mut extra = bytes.clone();
    extra.push(0);
    assert!(decode_borrowed(&extra).unwrap_err().to_string().contains("length mismatch"));
    let map = layout(&bytes);
    let at = map.map_offset() as usize;
    let types = at + usize::from(crate::be16(&bytes, at + 24).unwrap());
    let refs = types + usize::from(crate::be16(&bytes, types + 8).unwrap());
    let mut bad = bytes.clone();
    bad[refs + 5..refs + 9].copy_from_slice(&u32::MAX.to_be_bytes());
    assert!(decode_borrowed(&bad).unwrap_err().to_string().contains("offset outside data region"));
    for reserved in 9..12
    {
        let mut bad = bytes.clone();
        bad[refs + reserved] = 1;
        let error = decode_borrowed(&bad).unwrap_err();
        assert!(matches!(error, Error::Malformed(_)));
        assert!(error.to_string().contains("reserved bytes are nonzero"));
        let error = Index::decode(map, bad[at..].to_vec()).err().unwrap();
        assert!(matches!(error, Error::Malformed(_)));
        assert!(error.to_string().contains("reserved bytes are nonzero"));
    }
    for end in 0..bytes.len()
    {
        assert!(decode_borrowed(&bytes[..end]).is_err(), "accepted truncated native fork {end}");
    }
    let empty = encode_native(&[]).unwrap();
    assert_eq!(encoding(&empty, empty.len() as u64).unwrap(), Encoding::Native);
    assert!(decode(&empty).unwrap().is_empty());
}
