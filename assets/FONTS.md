# UI typeface

The Options menu sets its text in **DejaVu Sans** (regular), rasterized
at 19 px through the `fontdue` crate into a coverage-alpha atlas
(`src/fontvec.rs`).

- File: `assets/DejaVuSans.ttf` (759 676 bytes, DejaVu 2.37 minimal).
- Provenance: copied from the Nix store
  (`dejavu-fonts-minimal-2.37/share/fonts/truetype/DejaVuSans.ttf`),
  so the build stays hermetic — no system font lookup at runtime.
- Copyright: © Bitstream, Inc., © Tavmjong Bah and contributors.
  Licence: DejaVu Fonts License (permissive, BSD-style — allows
  embedding and redistribution provided the copyright and licence
  notices travel with the font).
  Full text: https://dejavu-fonts.github.io/ (upstream repository:
  https://github.com/dejavu-fonts/dejavu-fonts).
