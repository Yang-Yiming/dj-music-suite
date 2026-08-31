# dj-music-suite

Save mp3 and then automatically embed album art and lyrics into the ID3 tags.

## Usage

```bash
cargo build --release
./target/release/dj-music-suite convert --input <DIR> --output <DIR>
```

The CLI is self-explanatory: running it with no arguments (or `help`) prints the command overview, `help convert` (or `convert --help`) prints all options, and missing required arguments print the usage along with the error. Omitting the subcommand (`./dj-music-suite --input <DIR> ...`) still runs `convert`.

## Options

| Option | Description |
| --- | --- |
| `--input <DIR>` | folder containing `.ncm` files (required) |
| `--output <DIR>` | folder to write converted audio to (required) |
| `--threads <N>` | parallel conversions (default: 8) |
| `--meta-dir <DIR>` | cover folder (default: `<input>/meta`) |
| `--no-download` | never fetch missing covers from NetEase |

## Acknowledgements

- [magic-akari/ncmc](https://github.com/magic-akari/ncmc) for `ncm-core`.
