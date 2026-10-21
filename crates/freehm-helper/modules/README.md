# PawnIO modules (third-party, LGPL-2.1)

These are the unmodified, signed module binaries from
[namazso/PawnIO.Modules](https://github.com/namazso/PawnIO.Modules) release **0.2.11**
(`release_0_2_11.zip`). They are executed by the PawnIO driver's sandboxed VM, not by
FreeHM. Source code is available at the repository above; the license text is in
`COPYING.LGPL-2.1`.

| File | SHA-256 | Used for |
|---|---|---|
| IntelMSR.bin | d6ed85d65ab17a22f813ef98207d6d537155ee2ded5976a21cb48413c9b92e5f | Intel package temperature (IA32_PACKAGE_THERM_STATUS, MSR_TEMPERATURE_TARGET) and RAPL package energy |
| AMDFamily17.bin | dae74615761b78bdf064dfb3e136252ddcc6fc727d88f14738d0e5800d427a91 | AMD family 17h–1Ah Tctl (SMN 0x59800) and RAPL package energy |

The helper embeds these files; a unit test checks their SHA-256 against this table.
