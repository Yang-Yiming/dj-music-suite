# dj-music-suite

A suite of small command-line tools for working with music files. Currently ships one command: batch convert NetEase Cloud Music `.ncm` files to plain audio (mp3/flac), automatically embedding album art and lyrics into the ID3 tags.

Single self-contained Rust binary — decryption is built in via [ncm_core](https://github.com/magic-akari/ncmc) and tagging uses the `id3` crate. No ffmpeg, no external tools.

## Usage

```bash
cargo build --release
./target/release/dj-music-suite convert --input <DIR> --output <DIR>
```

The CLI is self-explanatory: running it with no arguments (or `help`) prints the command overview, `help convert` (or `convert --help`) prints all options, and missing required arguments print the usage along with the error. Omitting the subcommand (`./dj-music-suite --input <DIR> ...`) still runs `convert`.

## Commands

| Command | Description |
| --- | --- |
| `convert` | batch convert `.ncm` files to mp3/flac and embed cover art + lyrics |

### convert options

| Option | Description |
| --- | --- |
| `--input <DIR>` | folder containing `.ncm` files (required) |
| `--output <DIR>` | folder to write converted audio to (required) |
| `--threads <N>` | parallel conversions (default: 8) |
| `--meta-dir <DIR>` | cover folder (default: `<input>/meta`) |
| `--no-download` | never fetch missing covers from NetEase |

Expected input layout (extra files are optional):

```
music/
├── ARTMS - BURN.ncm
├── ARTMS - BURN.lrc          # optional: embedded as synced lyrics (USLT)
└── meta/
    └── track-<musicId>.jpg   # optional: embedded as cover art (APIC)
```

`<musicId>` is the NetEase track id, read from the `.ncm` metadata automatically.

Cover priority: `<input>/meta/track-<musicId>.jpg` → image embedded in the `.ncm` (if any) → download from the album art URL stored inside the `.ncm` file.

## Notes

- Audio streams are decoded bit-for-bit; only ID3 tags are added.
- NetEase's JSON-flavored LRC lines (`{"t":..,"c":[..]}`) are normalized to standard `[mm:ss.xxx]` LRC before embedding.
- Apple Music/iTunes does not display embedded lyrics; Poweramp, Musicolet, foobar2000 etc. do.

## Acknowledgements

- [magic-akari/ncmc](https://github.com/magic-akari/ncmc) (`ncm_core`, MIT) — NCM decryption, and [anonymous5l/ncmdump](https://github.com/anonymous5l/ncmdump) before it
