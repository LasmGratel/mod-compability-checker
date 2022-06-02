# mod-compatibility-checker

Simple Rust program that checks mod compatibility. Only supports 1.12.2 by now.

## Usage

Execute `mod-compatibility-checker <your mods directory>` or just place it in your mods directory and execute it.

Help message:
```
USAGE:
    mod-compatibility-checker [OPTIONS] [PATH]

ARGS:
    <PATH>

OPTIONS:
    -d, --dirty      Write a .sha or .strict-sha file in target directory
    -h, --help       Print help information
        --strict     Hash JAR file instead of modid:version lines
    -v, --verbose    List all the client-side mods or mods that accept all remote versions while
                     parsing
    -V, --version    Print version information
```

If two or more mods directory output the same checksum, they are compatible and free to join each other.

## Principle

1.12.2: Reads `META-INF/fml_cache_annotation.json` and get the annotation value, marks all client-only and `acceptableRemoteVersions = "*"` mods.

1.13+ Forge: Reads `META-INF/mods.toml` and filter mods with `[[dependencies.modid]] side="CLIENT"`

Fabric: Reads `fabric.mod.json` and filter mods with `environment: client`

Hash `modid:version` line or JAR file if strict mode is on.

## TODO

- [ ] Optimize performance further
- [ ] Support 1.7.10
- [ ] Support 1.13+
- [ ] Support Fabric

## Benchmark

| Modpack | Avg time | Avg time (Strict) |
| -- |  -- | -- |
| Nomifactory | 223.6 ms | 313.0 ms |
| Enigmatica 2 Expert Skyblock | 270.7 ms | 402.4 ms |
| FTB Revelation | 304.5 ms | 493.5 ms |
| FTB University 1.12 | 338.5 ms | 478.0 ms |

## Credits

[BLAKE3](https://github.com/BLAKE3-team/BLAKE3) for rapid fast hashing.

[hyperfine](https://github.com/sharkdp/hyperfine) for benchmarking.
