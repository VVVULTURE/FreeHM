# Third-party notices

FreeHM is MIT-licensed. It includes or links the following third-party components.

## Exo 2 font
Copyright 2013 The Exo 2 Project Authors (https://github.com/NDISCOVER/Exo-2.0).
Licensed under the SIL Open Font License, Version 1.1 (see "Exo 2 font" in the About page,
or `assets/fonts/OFL-Exo2.txt`). Bundled unmodified as the overlay's default font.

## Settings-window fonts (egui)
The settings window embeds egui's default fonts (crate `epaint_default_fonts` 0.37):
Ubuntu Light (Ubuntu Font Licence 1.0), Hack (MIT, with Bitstream Vera licence portions),
Noto Emoji (SIL OFL 1.1), emoji-icon-font (MIT) and egui-icons. Their licence texts are
included in `THIRD_PARTY_LICENSES.html` and in the crate's `fonts/` directory.

## PawnIO modules (Windows helper only)
`freehm-helper.exe` embeds two unmodified, signed PawnIO module binaries from the
PawnIO.Modules project (https://github.com/namazso/PawnIO.Modules), release 0.2.11:
`IntelMSR.bin` and `AMDFamily17.bin`. They are licensed under the GNU Lesser General Public
License, version 2.1 (`licenses/PawnIO-modules-LGPL-2.1.txt`; source code at the URL above).
SHA-256 hashes are listed in `crates/freehm-helper/modules/README.md` and checked by a unit
test. The modules run inside the PawnIO driver, which FreeHM does not ship.

## Rust crates
FreeHM is built from open-source Rust crates (MIT, Apache-2.0, BSD, Zlib, ISC, Unicode-3.0
and similar permissive licenses). The complete list with license texts is generated at
release time with `cargo about` and shipped as `THIRD_PARTY_LICENSES.html`.

## Not bundled
- NVIDIA Management Library (nvml.dll / libnvidia-ml.so) is part of the NVIDIA driver and is
  loaded from the driver installation at run time; it is not distributed with FreeHM.
- PawnIO (Windows, optional, for CPU temperature) is installed separately by the user from
  https://pawnio.eu; FreeHM loads `PawnIOLib.dll` from PawnIO's own installation folder.
  PawnIOLib is LGPL-2.1; the PawnIO driver is GPL-2.0 with an exception for programs that
  only communicate with it.
