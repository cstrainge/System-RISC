//! Seekable decoding must preserve resource semantics without reading padding
//! or allocating an unselected executable/font payload.
use mac_formats::{Error,
                  resource::{self, Resource,
                             stream::{Budget, read_selected}}};
fn sample() -> Vec<u8>
{
    let mut bytes = resource::encode(&[Resource { kind:       *b"CODE",
                                                  id:         0,
                                                  name:       Some(b"unselected".to_vec()),
                                                  attributes: 0x60,
                                                  data:       vec![0x55; 5 * 1024 * 1024] },
                                       Resource { kind:       *b"STR ",
                                                  id:         -7,
                                                  name:       Some(Vec::new()),
                                                  attributes: 0x28,
                                                  data:       b"\x05Hello".to_vec() }]).unwrap();
    // Legal trailing fork bytes are not payload and must never be fetched.
    bytes.resize(bytes.len() + 1024 * 1024, 0x77);
    bytes
}
#[test]
fn selected_resource_reads_only_map_and_payload_and_preserves_empty_name()
{
    let bytes = sample();
    let map = u32::from_be_bytes(bytes[4..8].try_into().unwrap()) as u64;
    let mut fetched = 0;
    let actual = read_selected(bytes.len() as u64,
                               Budget { payload_bytes: 6,
                                        ..Default::default() },
                               |kind, id| kind == *b"STR " && id == -7,
                               |at, out| {
                                   assert!(at == 0 || at >= map || at >= 5 * 1024 * 1024,
                                           "unselected CODE payload was read");
                                   fetched += out.len();
                                   out.copy_from_slice(&bytes
                                                           [at as usize..at as usize + out.len()]);
                                   Ok(())
                               }).unwrap();
    assert!(fetched < 1024);
    assert_eq!(actual.len(), 1);
    assert_eq!(actual[0],
               Resource { kind:       *b"STR ",
                          id:         -7,
                          name:       Some(Vec::new()),
                          attributes: 0x28,
                          data:       b"\x05Hello".to_vec() });
    let decoded = resource::decode(&bytes).unwrap();
    assert_eq!(actual[0], decoded[1]);
}
#[test]
fn oversized_selection_fails_before_payload_read_and_storage_errors_propagate()
{
    let bytes = sample();
    let error = read_selected(bytes.len() as u64,
                              Budget { payload_bytes: 32,
                                       ..Default::default() },
                              |kind, _| kind == *b"CODE",
                              |at, out| {
                                  assert!(out.len() < 1024,
                                          "large payload was read before budget validation");
                                  out.copy_from_slice(&bytes[at as usize..at as usize + out.len()]);
                                  Ok(())
                              }).unwrap_err();
    assert_eq!(error, Error("resource payload exceeds budget"));
    assert_eq!(read_selected(bytes.len() as u64,
                             Default::default(),
                             |_, _| true,
                             |_, _| Err(Error("injected read"))).unwrap_err(),
               Error("injected read"));
}
#[test]
fn duplicate_unselected_type_is_rejected_by_both_decoders()
{
    let mut bytes = sample();
    let map = u32::from_be_bytes(bytes[4..8].try_into().unwrap()) as usize;
    let types = map + u16::from_be_bytes(bytes[map + 24..map + 26].try_into().unwrap()) as usize;
    let kind: [u8; 4] = bytes[types + 2..types + 6].try_into().unwrap();
    bytes[types + 10..types + 14].copy_from_slice(&kind);
    assert!(resource::decode_borrowed(&bytes).is_err());
    assert!(read_selected(bytes.len() as u64,
                          Default::default(),
                          |_, _| false,
                          |at, out| {
                              out.copy_from_slice(&bytes[at as usize..at as usize + out.len()]);
                              Ok(())
                          }).is_err());
}
