# Fonts installed by default

Every `.ttf` file in this folder is copied into `System Folder/Fonts` when a
System RISC volume is built (`install_fonts` in `tools/xtask/src/disk.rs`).
They are ordinary installed fonts: a user can remove or replace one.

## Twemoji.ttf

The default colour emoji font, intended as the fallback for fonts that have no
emoji of their own.

| | |
|---|---|
| Family name in the font | Twemoji Mozilla |
| Font version | 0.7.0 |
| Artwork | Twemoji 15.0.3 (Unicode 15 era) |
| Format | TrueType outlines with `COLR` version 0 and `CPAL` colour layers |
| Size | 1,448,348 bytes |
| Glyphs | 13,878, of which 3,720 are colour base glyphs |
| Palette | 1,099 colours |
| Emoji sequences | `GSUB` ligature lookups only |

### Provenance

1. Source: the npm package `twemoji-colr-font` version 15.0.3
   (https://github.com/mrdrogdrog/twemoji-color-font), which packages the font
   built by Mozilla's `twemoji-colr` (https://github.com/mozilla/twemoji-colr).
   - `twemoji-colr-font-15.0.3.tgz` SHA-256
     `10cb935907d7dc3d46e254d6ff54b3e042652eb08f6973c81cd2c4225e6ebac5`
   - `package/twemoji.woff2` SHA-256
     `598b6867d47954acab46edd6a9773d734b844d221d3a95834da7442d5df64f98`
2. The package ships the font only as WOFF2. It was unpacked to a plain
   TrueType file with fontTools 4.67.0, changing the container and nothing else:

   ```python
   from fontTools.ttLib import TTFont
   font = TTFont("twemoji.woff2")
   font.flavor = None
   font.save("Twemoji.ttf")
   ```
3. Result: `Twemoji.ttf` SHA-256
   `cb6afc9972e3a24286e338a2c323654f4d05dff15d2d1a262a470f99d4d63dba`

### Licences

- The font file is under the SIL Open Font License 1.1.
- The emoji artwork is from Twemoji, by Twitter, Inc and other contributors,
  under Creative Commons Attribution 4.0 (CC BY 4.0). That licence requires
  attribution wherever the artwork is distributed, so a System RISC release
  that includes this font must credit Twemoji somewhere a user can find it.

Both texts are in `Twemoji-LICENSE.md`, as shipped in the source package.

### Status

- **Not drawn yet.** The Font Manager refuses any font carrying colour tables,
  so this file is logged and skipped at startup. It is installed now so the
  file, its provenance and its licence are settled before the OpenType
  extension that will draw it exists.
- **Two Unicode versions behind.** Upstream Twemoji has moved on from 15.0.3.
  The plan is to rebuild this font from current artwork before a release.
