# Terminal colour scheme provenance

Traditional is Terminal's grey-on-black default. The other schemes are terminal
content palettes; they do not select or inspect the operating system theme.

Each directory retains the upstream licence, source data, Git blob identifiers,
source URLs and the exact numeric palette used to generate `src/schemes.rs`.
Witch Hazel uses its explicitly sRGB iTerm palette. Nord, Dracula and Catppuccin
use upstream Alacritty hexadecimal RGB palettes. Tomorrow Night uses its
upstream Xdefaults palette. Catppuccin retains the explicit index 16/17 overrides.

Monokai uses Microsoft's MIT-licensed VS Code port. The original palette is
attributed to Wimer Hazenberg; the original upstream palette licence has not
been established. This uncertainty is retained in `monokai/scheme.json`.
No Monokai Pro assets are included.

A scheme reset restores the selected scheme's values, including indexed
overrides. Runtime OSC colour changes do not rewrite the bundled source data.
