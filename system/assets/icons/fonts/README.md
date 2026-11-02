# Standard font file icons

These are the existing original Mac OS 7.6.1 Finder families, moved from
Finder's resource fork into System without changing their IDs, attributes or
payload bytes:

- **14500**: standalone TrueType (`sfnt`).
- **15500**: font suitcase (`FFIL`).

Both retain `ICN#`, `icl4`, `icl8`, `ics#`, `ics4` and `ics8`. No artwork is
redrawn, recolored or resampled. The original source Finder snapshot SHA-256 is
`647b19dba89dc0167ae45dda20fd02bd8e44c91252e7513c9c584f1588eb4d3f`.

The new System MACS `BNDL`/`FREF` metadata registers these as generic file-type
defaults through existing Icon Manager lookup, including files with other
creators. File-local or creator-specific icon handling keeps its existing
precedence. Finder no longer keeps duplicate copies of these families.

The shared file-type registry supplies a case-insensitive `.ttf` fallback only
for untyped files (zero type or `BINA`) without a matching preference entry.
Explicit types and user associations win; stored metadata is not rewritten.
