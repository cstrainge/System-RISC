# External format references and constants

The Arsenic randomization constants in src/decode/arsenic_table.rs and
method-13 Huffman codebooks in src/decode/lz_huffman_tables.rs are from
sit-algos 0.3.0, part of Cyco's sit-rs project, licensed under Apache-2.0:
https://codeberg.org/cyco/sit-rs (https://crates.io/crates/sit-algos/0.3.0).
A copy of that license is included as LICENSE-APACHE-2.0. The resumable decoder
and parser state machines are implemented here; sit-algos is not a runtime
or build dependency. Its synchronous std::io reader is not used in the guest.

Layout and algorithm cross-checks also use MacPaw XADMaster's original parser
format notes and primary implementations. No XADMaster code is copied into this
crate. Its source remains at https://github.com/MacPaw/XADMaster.
