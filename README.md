# jmap

JSON based format for storing UObject reflection data, vtable layouts, version information, among other things extracted from compiled Unreal Engine binaries.

## [jmap_dumper](jmap_dumper)

Dump from running process:
```console
cargo run --release -- --pid 12345 output.jmap
```

Dump from existing full-memory minidump:
```console
cargo run --release -- --minidump FSD-Win64-Shipping.DMP output.jmap
```

Or output to .usmap:
```console
cargo run --release -- --minidump FSD-Win64-Shipping.DMP output.usmap
```

In case of message "Error: Resolution: EngineVersion: expected at least one value", set an environment variable for patternsleuth library:

`$env:PATTERNSLEUTH_RES_EngineVersion="ver"` e.g. `$env:PATTERNSLEUTH_RES_EngineVersion="5.6"`

In case of message like "Error: Resolution: FNamePool: found 2 unique values [7FF6DB379E00, 7FFDB90C6140]", set an environment variable for patternsleuth library:

`$env:PATTERNSLEUTH_RES_FNamePool=0x<one of the found FNamePool values>` e.g. `$env:PATTERNSLEUTH_RES_FNamePool=0x7FF6DB379E00`

## output
The output JSON is a superset of .usmap and contains enough information to fully reconstruct a matching project in the Unreal Engine editor.

It contains:
- Reflection data for: Classes, Functions, Structs, Enums, etc.
- Class Default Objects (CDOs) and property values

It also does light VTables analysis and dumps approximate VTables for all UObjects found.

## [jmap](jmap)
JMAP dumps preserve available reflection metadata in object/property `metadata`
maps and enum `entry_metadata` maps, including `Bitmask`, `BitmaskEnum`, `Bitflags`,
`UseEnumValuesAsMaskValuesInEditor`, display names and hidden entries. Capture
counts/layout availability are recorded in `metadata.reflection_metadata`.
Shipping builds may strip this data. Optional `--infer-bitmasks` adds heuristic
annotations for Int32 properties containing a flag word, using enum-name matches,
owning package/type context, and enum values. Ambiguous matches/modes are skipped.
Original flags, numbers, offsets and defaults are preserved. Applied/skipped
decisions are recorded separately in `bitmask_inference`; they are not counted
as captured reflection metadata. It also works offline with `--jmap old.jmap
--infer-bitmasks new.jmap`. Explicit metadata takes precedence.
Use `--editor` only when dumping an actual editor build. Package metadata capture
currently supports UE 4.20–5.5; newer layouts still support available FField maps.

Crate for reading/writing .jmap files.

## [usmap](usmap)
Crate for reading/writing .usmap files (legacy binary format created by https://github.com/TheNaeem/UnrealMappingsDumper still used by many tools today).

## [ue_binja](ue_binja)
Binary Ninja plugin to reconstruct classes and structs from reflection data.

![Binary Ninja preview](media/uebinja.png)

## creating a full game dump

For development and debugging purposes, it is handy to make a full memory dump of the game. Windows makes this really easy via task manager:

![create minidump via task manager](media/create_dump1.png)


After a moment or two the dump should complete:
![minidump complete](media/create_dump2.png)


