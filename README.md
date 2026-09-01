# dj-music-suite

Small tools for DJ music libraries: convert NetEase `.ncm` files to mp3/flac
with full tags, import new music into a library folder with duplicate
detection, and reorganize a library folder on disk.

## Usage

```bash
cargo build --release
./target/release/dj-music-suite convert --input <DIR> --output <DIR>
./target/release/dj-music-suite import --input <DIR> --root <MUSIC_DIR>
./target/release/dj-music-suite reorg --root <MUSIC_DIR>
```

The CLI is self-explanatory: running it with no arguments (or `help`) prints
the command overview, `help convert` (or `convert --help`) prints all options,
and missing required arguments print the usage along with the error. Omitting
the subcommand (`./dj-music-suite --input <DIR> ...`) still runs `convert`.

## convert

Batch converts `.ncm` files and embeds tags: title, artist and album come from
the `.ncm` metadata; the cover from `<meta-dir>/track-<musicId>.jpg`, then the
image embedded in the `.ncm` file, then the albumPic URL (unless
`--no-download`; downloads are cached into `<meta-dir>`); a same-named `.lrc`
file is embedded as unsynced lyrics (USLT).

| Option | Description |
| --- | --- |
| `--input <DIR>` | folder containing `.ncm` files (required) |
| `--output <DIR>` | folder to write converted audio to (required) |
| `--threads <N>` | parallel conversions (default: 8) |
| `--meta-dir <DIR>` | cover folder (default: `<input>/meta`) |
| `--no-download` | never fetch missing covers from NetEase |

Downloaded covers are cached into `--meta-dir` as `track-<musicId>.<ext>`, so
later runs (including offline or with `--no-download`) pick them up locally.

## import

Copies (or moves) new audio files into the music library folder — the gate
before files enter the library. Detection against the existing library: same
artist + title with a similar duration (±3 s) is skipped as a duplicate; a
different duration is treated as an alternate version (another mix) and
imported; a target that already exists is never overwritten unless
`--overwrite` is given.

```bash
./target/release/dj-music-suite import --input <DIR> --root <MUSIC_DIR>            # report only
./target/release/dj-music-suite import --input <DIR> --root <MUSIC_DIR> --execute  # place the files
```

| Option | Description |
| --- | --- |
| `--input <DIR>` | folder with new audio files (scanned recursively, required) |
| `--root <DIR>` | music library root to import into (must exist, required) |
| `--template <T>` | destination layout relative to root (default `{artist}/{filename}.{ext}`); placeholders: `{artist}` `{title}` `{album}` `{filename}` `{ext}` |
| `--mode <copy\|move>` | copy (default) or move the files into the library |
| `--overwrite` | on duplicate/conflict, replace the existing library file with the incoming one (atomic tmp+rename; duplicates are replaced in place, keeping the library path) |
| `--execute` | actually place the files (default: report only) |

Typical flow: `convert` into a staging folder, review the import report, then
`import --execute` and let rekordbox import/refresh the library root — new
tracks come in with tags and artwork already embedded.

## reorg

Moves the files of a music library folder into a tag-based layout. It only
touches the filesystem — analyze first, review the plan, then execute and let
rekordbox relink the moved files via *File → Display All Missing Files →
Relocate* (point it at the library root; it matches by filename).

```bash
./target/release/dj-music-suite reorg --root <MUSIC_DIR>                # analyze, writes reorg-plan.json
./target/release/dj-music-suite reorg --root <MUSIC_DIR> --execute      # apply the plan
./target/release/dj-music-suite reorg --execute --from reorg-plan.json  # apply a (hand-edited) plan
```

| Option | Description |
| --- | --- |
| `--root <DIR>` | music library folder to reorganize |
| `--template <T>` | destination layout relative to root (default `{artist}/{filename}.{ext}`); placeholders: `{artist}` `{title}` `{album}` `{filename}` `{ext}` |
| `--execute` | actually move files (default: analyze only) |
| `--from <FILE>` | execute a previously generated plan json instead of re-analyzing |
| `--allow-rename` | also apply renames — rekordbox relocates by filename, so renamed files must be relinked manually one by one |
| `--plan <FILE>` | plan json path (default `reorg-plan.json`) |

The analysis classifies every file: `move` / `rename` / `move+rename`,
`in place` (already organized), `conflict` (target exists), `untagged`
(missing a tag the template needs) and possible duplicates (same normalized
artist + title). Only `move` entries execute by default; renames need
`--allow-rename`; conflicts and untagged files are never touched.

## Acknowledgements

- [magic-akari/ncmc](https://github.com/magic-akari/ncmc) for `ncm-core`.
