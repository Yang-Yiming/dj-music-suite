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
./target/release/dj-music-suite dedup --root <MUSIC_DIR>
./target/release/dj-music-suite trash --root <MUSIC_DIR>
./target/release/dj-music-suite-web   # web UI as a separate binary, opens the browser
```

The CLI is self-explanatory: running it with no arguments (or `help`) prints
the command overview, `help convert` (or `convert --help`) prints all options,
and missing required arguments print the usage along with the error. Omitting
the subcommand (`./dj-music-suite --input <DIR> ...`) still runs `convert`.

`import`, `reorg`, `dedup` and `trash` operate on the music library root: pass
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

## dedup

Finds and removes duplicates. Files with identical content are grouped by
hash; the rest are grouped by normalized artist + title when their durations
are within 2 s (so different mixes stay separate). Each group keeps one file:
`--keep best` scores format (lossless first), bitrate, sample rate, bit depth,
cover art, lyrics and tag completeness; `--keep first` keeps the first in scan
order. Losers are moved into `<root>/.dedup-trash/<kind>/` unless `--delete`
is given.

```bash
./target/release/dj-music-suite dedup --root <MUSIC_DIR>              # analyze, writes dedup-plan.json
./target/release/dj-music-suite dedup --root <MUSIC_DIR> --execute    # move losers into the trash
./target/release/dj-music-suite dedup --root <MUSIC_DIR> --execute --delete  # remove them for good
```

| Option | Description |
| --- | --- |
| `--execute` | actually move/delete duplicates (default: analyze only) |
| `--from <FILE>` | execute a previously generated plan json instead of re-analyzing |
| `--keep <best\|first>` | which copy survives in each group (default `best`) |
| `--trash <DIR>` | folder to move duplicates into (default `<root>/.dedup-trash`) |
| `--delete` | delete duplicates outright instead of moving them to the trash (irreversible) |
| `--plan <FILE>` | plan json path (default `dedup-plan.json`); also seeds the hash cache of the next run |

The plan file doubles as a cache keyed by path + mtime + size, so re-running
against an unchanged library skips re-hashing. A group whose keeper is gone is
left untouched: it might be the only surviving copy of the song.

### trash

Every file dedup moves out gets a record of where it came from, appended to
`<root>/.dedup-trash/manifest.json` (by the CLI and the web UI alike). Without
it a trashed `track01.flac` is just a name — and its `track01-1.flac` sibling
is a guess — so restoring would be guesswork.

```bash
# list the trash
./target/release/dj-music-suite trash --root <MUSIC_DIR>
# put entries back where they came from (entry names as printed above)
./target/release/dj-music-suite trash --root <MUSIC_DIR> --restore 'identical/song.mp3'
# or delete what you no longer need
./target/release/dj-music-suite trash --root <MUSIC_DIR> --delete 'identical/song.mp3'
./target/release/dj-music-suite trash --root <MUSIC_DIR> --empty
```

Restore never overwrites: a target that is occupied again is skipped and
reported, because rekordbox relinks by path and silently choosing a different
name is worse than staying put. Files with no manifest record (dropped in by
hand, or trashed before the manifest existed) are listed as unknown origin:
they can be deleted, but not restored.

## Building

```bash
just build   # bun build of the web bundle, then cargo build --release
```

Outputs two binaries in `target/release/`:

- `dj-music-suite` — the CLI (no web stack; builds without bun entirely)
- `dj-music-suite-web` — the web UI; frontend sources live in
  `crates/server/frontend` (bun + Vite + Svelte + Tailwind), the built bundle
  is embedded into the binary, so the web app still ships as one file

`just dev-web` runs the frontend dev server with a proxy to a locally
running `dj-music-suite-web` for UI iteration.

## web ui

A local web UI covering the flows non-CLI users need: converting `.ncm`
files, importing music into the library, reorganizing it and removing
duplicates. Everything stays on the machine — the server listens on
`127.0.0.1` only, and files are uploaded into a local staging folder (never
sent anywhere).

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

The 整理 (reorg) and 去重 (dedup) tabs run the CLI's other two commands on the
configured library root, with the same analyze → review → execute split:

- **整理** analyzes the root against a directory template and lists every file
  as move / rename / move+rename / in place / conflict / untagged, plus the
  same suspicious-duplicate groups the CLI reports. Rows are ticked per file;
  only plain moves are ticked by default, because rekordbox relinks relocated
  files by file name — a rename needs `--allow-rename` in the CLI and a
  manual relink afterwards, so the web UI makes it an explicit per-row opt-in
  (with a warning next to it). Executing applies exactly the ticked rows.
- **去重** hashes the library and groups duplicates (identical content, or the
  same artist + title within the duration tolerance), keeping one file per
  group: the best-scoring one, or — per group — whichever the user picks.
  Removals only ever move losers into `<root>/.dedup-trash/<kind>/`; the web
  UI never deletes outright (`--delete` stays a CLI-only, irreversible
  option). Groups whose keeper is gone are left untouched, as in the CLI.

Both keep the analyzed plan in server memory (the hash/score cache is reused
when re-scanning), so a page reload restores the plan and any running job's
progress, while restarting the server means analyzing again.

### dedup trash

The 去重 tab lists the trash below the duplicate groups: original location,
group kind, size and age, with restore / delete per selection and
restore-all / empty for the whole folder. The manifest that makes restoring
possible is written by core, so files trashed by
`dj-music-suite dedup --execute` show up here with their origins intact (see
[trash](#trash) for the command line equivalent). Restore never overwrites: an
occupied target is skipped and reported. Files with no manifest record are
listed as unknown origin — deletable, not restorable.

Workspace layout: `crates/core` (UI-agnostic logic, event sink + structured
results), `crates/cli` (argument parsing + terminal rendering),
`crates/server` (axum web UI calling into core).

## Acknowledgements

- [magic-akari/ncmc](https://github.com/magic-akari/ncmc) for `ncm-core`.
