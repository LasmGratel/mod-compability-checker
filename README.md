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

Checks `META-INF/fml_cache_annotation.json` and get the annotation value, marks all client-only and `acceptableRemoteVersions = "*"` mods.

Hash `modid:version` line or JAR file if strict mode is on.

## TODO

- [ ] Optimize performance further
- [ ] Support 1.7.10
- [ ] Support 1.13+
- [ ] Support Fabric

## Benchmark

| Modpack | Avg time | Avg time (Strict) |
| -- |  -- | -- |
| Nomifactory | 263.1 ms | 352.6 ms |
| Enigmatica 2 Expert Skyblock | 313.8 ms | 463.1 ms |
| FTB Revelation | 354.3 ms | 536.5 ms |
| FTB University 1.12 | 367.8 ms | 519.1 ms |

## Credits

[BLAKE3](https://github.com/BLAKE3-team/BLAKE3) for rapid fast hashing.

[hyperfine](https://github.com/sharkdp/hyperfine) for benchmarking.
