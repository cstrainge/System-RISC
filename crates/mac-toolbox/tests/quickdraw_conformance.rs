//! Aggregate userspace compatibility tests. Expected bytes come from the original
//! System7.6.1 probe; every operation goes through public SDK and resident gate.
#![cfg(not(target_arch = "riscv64"))]
mod support;
mod conformance;
use mac_toolbox::quickdraw::Rect;
fn unhex(text: &str) -> Vec<u8>
{
    assert!(text.len().is_multiple_of(2));
    text.as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| u8::from_str_radix(core::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}
#[test]
fn aggregate_acceptance_run_all_uses_public_sdks()
{
    support::with_system(|system| conformance::run_all(system).unwrap());
}

#[test]
fn all_267_original_oval_regions_match_through_public_sdk()
{
    let fixtures: serde_json::Value =
        serde_json::from_str(include_str!("data/original-quickdraw-regions.json")).unwrap();
    let corpus = fixtures["oval_regions"].as_array().unwrap();
    assert_eq!(corpus.len(), 267);
    support::with_system(|system| {
        let mut output = [0u8; 8192];
        for item in corpus
        {
            let name = item["name"].as_str().unwrap();
            let (width, height) = name.strip_prefix("oval_w").unwrap().split_once("_h").unwrap();
            let expected = unhex(item["bytes_hex"].as_str().unwrap());
            conformance::original_oval(system,
                                       width.parse().unwrap(),
                                       height.parse().unwrap(),
                                       &expected,
                                       &mut output).unwrap_or_else(|error| {
                                                       panic!("{name}: {error:?}")
                                                   });
        }
    });
}
#[test]
fn original_standard_region_fixtures_are_independent_and_retained_verbatim()
{
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("data/original-quickdraw-regions.json")).unwrap();
    let originals = [("rect", conformance::RECT_A),
                     ("rect_b", conformance::RECT_B),
                     ("intersection", conformance::INTERSECTION),
                     ("union", conformance::UNION),
                     ("difference", conformance::DIFFERENCE),
                     ("xor", conformance::XOR),
                     ("oval60x80", conformance::OVAL),
                     ("inset_oval", conformance::INSET_OVAL)];
    for (name, bytes) in originals
    {
        let value =
            fixture["regions"].as_array().unwrap().iter().find(|r| r["name"] == name).unwrap();
        assert_eq!(unhex(value["bytes_hex"].as_str().unwrap()), bytes);
    }
    assert_eq!(fixture["provenance"]["runtime_report_sha256"],
               "33687cc39e8d607dc7efa3fb50dfeb298dca08b862d5eb69b32065f1371865d9");
    // The facade's ordinary value types remain public without resident imports.
    assert_eq!(Rect::new(1, 2, 3, 4).to_be_bytes(), [0, 2, 0, 1, 0, 4, 0, 3]);
}
#[test]
fn host_sessions_are_scoped_and_reset_after_unwind()
{
    let failure =
        std::panic::catch_unwind(|| support::with_system(|_| panic!("intentional fixture unwind")));
    assert!(failure.is_err());
    support::with_system(|system| conformance::geometry_and_regions(system).unwrap());
}

#[test]
fn original_memory_new_handle_clear_allocation_observations()
{
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("data/original-memory-cases.json")).unwrap();
    let clear: Vec<_> =
        fixture.as_array().unwrap().iter().filter(|r| r["operation"] == "NewHandleClear").collect();
    assert_eq!(clear.len(), 3);
    for (record, size) in clear.into_iter().zip([0usize, 1, 16])
    {
        assert_eq!(record["size"], size);
        assert_eq!(record["state"], 0);
        assert_eq!(record["mem_error"], 0);
        assert_eq!(record["known_payload"], "00".repeat(size));
    }
    support::with_system(|system| assert_eq!(conformance::memory_new_clear(system).unwrap(), 3));
}

#[test]
fn compact_original_oval_corpus_also_runs_through_the_aggregate_sdk()
{
    support::with_system(|system| {
        assert_eq!(conformance::original_oval_corpus(system).unwrap(), 267)
    });
}
#[test]
fn all_48_original_handle_observations_match_through_the_aggregate_sdk()
{
    support::with_system(|system| assert_eq!(conformance::memory_handles(system).unwrap(), 48));
}
#[test]
fn compiled_memory_oracle_matches_original_json_and_marks_pointer_cases_deferred()
{
    let records: serde_json::Value =
        serde_json::from_str(include_str!("data/original-memory-cases.json")).unwrap();
    let records = records.as_array().unwrap();
    assert_eq!(records.len(), 51);
    assert_eq!(records.iter().filter(|r| r["operation"] == "NewPtrClear").count(), 3);
    assert_eq!(conformance::memory::OBSERVATIONS.len(), 48);
    for oracle in conformance::memory::OBSERVATIONS
    {
        let row = &records[oracle.sequence as usize];
        assert_eq!(row["case"], oracle.case);
        assert_eq!(row["operation"], oracle.operation);
        assert_eq!(row["mem_error"], oracle.error);
        assert_eq!(row["size_error"], oracle.size_error);
        assert_eq!(row["state_error"], oracle.state_error);
        assert_eq!(row["size"].as_u64().map(|n| n as usize), oracle.size);
        assert_eq!(row["state"].as_u64().map(|n| n as u8), oracle.state);
        assert_eq!(unhex(row["known_payload"].as_str().unwrap()), oracle.payload);
    }
}
