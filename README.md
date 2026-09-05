# dj-music-suite

Small tools for DJ music libraries: convert NetEase `.ncm` files to mp3/flac
with full tags, import new music into a library folder with duplicate
detection, and reorganize a library folder on disk. Comes as a CLI and a
local web UI.

## Usage

```bash
cargo build --release
./target/release/dj-music-suite convert --input <DIR> --output <DIR>
./target/release/dj-music-suite import --input <DIR> --root <MUSIC_DIR>
./target/release/dj-music-suite reorg --root <MUSIC_DIR>
./target/release/dj-music-suite-web   # web UI as a separate binary, opens the browser
```

The CLI is self-explanatory: running it with no arguments (or `help`) prints
the command overview, `help convert` (or `convert --help`) prints all options,
and missing required arguments print the usage along with the error. Omitting
the subcommand (`./dj-music-suite --input <DIR> ...`) still runs `convert`.

`import`, `reorg` and `dedup` operate on the music library root: pass
`--root` explicitly, or omit it to use the library root configured in the
web UI (stored in the app config dir).

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
| `--template <T>` | destination layout relative to root (default `{artist}/{filename}.{ext}`); placeholders: `{artist}` `{artists}` `{title}` `{album}` `{filename}` `{ext}` |
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
| `--template <T>` | destination layout relative to root (default `{artist}/{filename}.{ext}`); placeholders: `{artist}` `{artists}` `{title}` `{album}` `{filename}` `{ext}` |
| `--execute` | actually move files (default: analyze only) |
| `--from <FILE>` | execute a previously generated plan json instead of re-analyzing |
| `--allow-rename` | also apply renames — rekordbox relocates by filename, so renamed files must be relinked manually one by one |
| `--plan <FILE>` | plan json path (default `reorg-plan.json`) |

The analysis classifies every file: `move` / `rename` / `move+rename`,
`in place` (already organized), `conflict` (target exists), `untagged`
(missing a tag the template needs) and possible duplicates (same normalized
artist + title). Only `move` entries execute by default; renames need
`--allow-rename`; conflicts and untagged files are never touched.

## serve

A local web UI covering the two flows non-CLI users need: converting `.ncm`
files and importing music into the library. Everything stays on the machine —
the server listens on `127.0.0.1` only, and files are uploaded into a local
staging folder (never sent anywhere).

```bash
./target/release/dj-music-suite-web [--port 8765] [--no-open]
```

The page walks through the steps: set the library root once (stored in the
app config dir, e.g. `~/Library/Application Support/dj-music-suite/config.toml`
on macOS — the CLI reads the same value as its `--root` default), drag & drop
files, convert with tags/covers/lyrics, then run the
import analysis. The analysis shows a preview table — new / alt-version /
duplicate / conflict / untagged — and writes to the library only after an
explicit confirmation (the same analyze → execute two-phase design as the
CLI). Progress streams live into the page. `--no-open` skips launching the
browser automatically.

Workspace layout: `crates/core` (UI-agnostic logic, event sink + structured
results), `crates/cli` (argument parsing + terminal rendering),
`crates/server` (axum web UI calling into core).

## Acknowledgements

- [magic-akari/ncmc](https://github.com/magic-akari/ncmc) for `ncm-core`.
