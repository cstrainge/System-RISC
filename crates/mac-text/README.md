# System 7 text conversion

`mac-text` is a safe, allocation-free `no_std` codec. It contains the single
System 7 MacRoman mapping, including byte DB as currency sign (not euro).

`characters` and `decode_into` decode every byte. `MacRomanText` validates an
immutable UTF-8 borrow and exposes its encoded length before a caller allocates.
`byte` is the exact scalar inverse. Unsupported scalars produce typed errors
with the character and zero-based scalar/UTF-8 byte positions; no normalization
or replacement character is applied.

`encode_pascal` and `MacRomanText::write_pascal` take a payload capacity (`u8`):
255, 63, 31, 27, or any other representable capacity. Length prefixes count
MacRoman bytes, never UTF-8 bytes. Builders refuse overflow and preserve output
on ordinary refusal. `PascalStr::from_exact` rejects trailing bytes;
`from_buffer` explicitly accepts unused storage after one checked string.

`project_filename_31` is the only prefix projection. It validates the entire
name (including a suffix beyond character 31) and the 255-character native name
limit before writing the Str31 view. Input/storage is not mutated. This does
not change filesystem name validation or normalization.

All buffer writers touch only the returned prefix on success. Insufficient
storage and invalid input leave caller output untouched. The crate neither
allocates nor calls OS services. `mac-formats::text` owns optional allocating
format values around this codec; it contains no separate conversion algorithm.
Native service requests are UTF-8; this crate does not introduce legacy native
trap variants or a classic dispatcher.
