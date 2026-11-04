# Vendored code

| What | Upstream | Version | Licence |
|---|---|---|---|
| ARA Audio Random Access API, translated to Rust | https://github.com/Celemony/ARA_API (part of https://github.com/Celemony/ARA_SDK) | 2.3.0 (tag `releases/2.3.0`, commit 65ec5c4) | Apache-2.0 (`LICENSE-ARA_API.txt`) |

No header is copied. The declarations in `src/ffi.rs` are a hand-made
translation of `ARAInterface.h` (Copyright (c) 2012-2025, Celemony Software
GmbH): the types, constants, the model graph's property structs, the content
events, the host and plug-in interfaces, the factory and the plug-in
extension, with the header's struct packing (1 byte on x86, natural on ARM).
The companion-API pieces come from the same release:

- `ARAVST3.h` — the interface IDs and vtables of `ARA::IPlugInEntryPoint`
  and `ARA::IPlugInEntryPoint2`, and the "ARA Main Factory Class" category
  (`crates/faderframe-plugin-vst3/src/ara.rs`).
- `ARACLAP.h` — `clap_ara_factory_t`, `clap_ara_plugin_extension_t`, their
  identifiers and the `ara:supported`/`ara:required` features
  (`crates/faderframe-plugin-clap/src/ara.rs`).

Left out: the ARA Library (C++ helpers), the examples, the audio file
chunks (`ARAAudioFileChunks.h`), Audio Unit integration (`ARAAudioUnit.h`),
the deprecated ARA 1 archiving calls (declared only for the layout).

The ARA API README notes that implementing the API in a host or plug-in is
normal use and does not make that software a derived work; the translation
is still kept under the API's licence terms (this file and the licence
text ship with the source).

## Local changes

None beyond the translation (names without the `ARA` prefix, `Option` for
plug-in function pointers that may be missing or null).
