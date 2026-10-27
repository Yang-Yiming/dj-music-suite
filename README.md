# dj-music-suite

Small tools for DJ music libraries: convert NetEase `.ncm` files to mp3/flac
with full tags, transcode existing audio between formats, import new music
into a library folder with duplicate detection, and reorganize a library
folder on disk. Comes as a CLI and a local web UI.

## Usage

```bash
cargo build --release
./target/release/dj-music-suite convert --input <DIR> --output <DIR>
./target/release/dj-music-suite cover --input <FILE_OR_DIR>
./target/release/dj-music-suite transcode --to mp3 --input <DIR>
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

The Web convert tab has the same two output destinations as the CLI: keep the
result in the app staging area for the import review, or choose
`直接保存到指定目录` and write to an absolute output path. Direct output
does not require a configured library root, refuses to overwrite an existing
target file, and removes that batch's staging directory after a fully
successful conversion. Failed conversions keep their staging batch for retry.

## cover

Finds missing album artwork from metadata and embeds it in the audio file. The
search uses MusicBrainz/Cover Art Archive first and falls back to iTunes when
`--provider auto` cannot obtain a usable image. Only artist/title/album text is
sent to those services; audio files are never uploaded. When tags are missing,
an `Artist - Title` filename is used as a conservative fallback.

```bash
./target/release/dj-music-suite cover --input track.flac                 # analyze, write cover-plan.json
./target/release/dj-music-suite cover --input track.flac --execute       # analyze, then embed high-confidence matches
./target/release/dj-music-suite cover --input library/                  # whole library
./target/release/dj-music-suite cover --execute --from cover-plan.json  # apply a reviewed plan
```

| Option | Description |
| --- | --- |
| `--input <FILE_OR_DIR>` | audio file or folder to scan (default: configured library root) |
| `--provider <auto\|musicbrainz\|itunes>` | search source (default: `auto`) |
| `--min-confidence <0.0..1.0>` | minimum score for automatic embedding (default: `0.88`) |
| `--country <CC>` | iTunes storefront, falling back to US when empty (default: `US`) |
| `--overwrite` | replace an existing cover (default: files with a cover are skipped) |
| `--execute` | actually write the selected covers (default: analyze only) |
| `--from <FILE>` | execute a reviewed plan instead of searching again |
| `--plan <FILE>` | plan path (default: `cover-plan.json`) |

Only high-confidence matches are marked ready; ambiguous results remain in
`review` and are never written automatically. Existing labels such as
`<BURN> - Single` returned by iTunes are normalized when scoring. The first
version writes MP3, FLAC and M4A; AAC, WAV and AIFF are reported as
unsupported instead of risking a broken tag. Search results are cached for
seven days and downloaded artwork for longer, so repeated album runs do not
hit the services again. Writes happen through a same-directory temporary copy;
the original path is replaced atomically only after the new tag has been read
back and verified.

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

## transcode

Re-encodes existing audio files into another format. Unlike every other
command this one rewrites the audio instead of moving it, so it is its own
command with its own analyze → plan → execute pair.

**ffmpeg is required** (the one external dependency in the suite): it is
looked up on `$PATH`, or passed explicitly with `--ffmpeg`. The target's
encoder is checked *before* the analysis runs — a build without `libmp3lame`
refuses `--to mp3` instead of failing halfway or silently degrading. Targets:
`mp3` (`v0`…`v9` or e.g. `320k`), `flac` (`0`…`12`), `m4a` (AAC, e.g. `256k`),
`alac`, `wav`.

```bash
./target/release/dj-music-suite transcode --to mp3 --input <DIR> --output <DIR>          # analyze, writes transcode-plan.json
./target/release/dj-music-suite transcode --to mp3 --input <DIR> --output <DIR> --execute
./target/release/dj-music-suite transcode --to flac --input <DIR> --replace --execute    # convert in place, sources to the trash
./target/release/dj-music-suite transcode --to mp3 --execute --from transcode-plan.json  # apply a plan
```

| Option | Description |
| --- | --- |
| `--to <FMT>` | target format: `mp3` `flac` `m4a` `alac` `wav` (required) |
| `--input <DIR>` | folder to scan recursively (default: the configured library root) |
| `--output <DIR>` | folder to write into (default: next to each source). The tree below `--input` is preserved, so same-named files in different folders never collide |
| `--quality <Q>` | encoder setting: mp3 `v0`..`v9` or `320k` (default `v0`), m4a bitrate (default `256k`), flac `0`..`12` (default `8`); alac and wav take none, and passing one is an error rather than being ignored |
| `--replace` | move each source into the trash folder after it converted (default `<input>/.dedup-trash`). Only valid without `--output` |
| `--threads <N>` | parallel conversions (default: 8) |
| `--ffmpeg <FILE>` | ffmpeg binary to use (default: `ffmpeg` from `$PATH`) |
| `--trash <DIR>` | trash folder for `--replace` (default: `<input>/.dedup-trash`) |
| `--execute` | actually convert (default: analyze only and write the plan) |
| `--from <FILE>` | execute a previously generated plan json instead of re-analyzing |
| `--plan <FILE>` | plan json path (default `transcode-plan.json`) |

The analysis classifies every file: `ready`, `same-codec` (already the target
codec, per the source's *actual* container — an ALAC `.m4a` counts as a real
conversion to AAC `.m4a` even though the extension matches), `conflict`
(target exists) and `needs-replace` (the output would overwrite its own
source, e.g. `alac→aac` in place without `--replace`). Only `ready` entries
convert, and `same-codec` files are skipped on purpose: re-encoding a file
that already is the target only loses quality.

What carries over is ffmpeg's metadata mapping (`-map_metadata 0`) plus the
attached cover (`-map 0:v?`, valid in mp3/flac/m4a, dropped for `wav` — which
the report says out loud). Lyrics are re-written afterwards in the key each
container defines (ID3v2 `USLT`, Vorbis `LYRICS`, MP4 `©lyr`), because
ffmpeg's own mapping puts them where DJ software does not look.

Two things worth knowing before a run:

- lossy → lossless (`mp3` → `flac`) is flagged as *no quality gain*, not
  blocked: the report says the file only gets bigger.
- a file the analysis could not probe (ape, wma, dsd, …) is still converted;
  the note says its tags could not be read, so the output may be untagged.

Converted files have to be re-read by rekordbox: refresh the library root.
Replacing a source in place keeps its path, so no relink is needed there.

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

Every file dedup moves out — and every source `transcode --replace` retires —
gets a record of where it came from, appended to
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
make build   # bun build of the web bundle, then cargo build --release
# same thing with just: just build
```

`make` is enough on a stock macOS/Linux setup; `just` is optional. Both run
the frontend bundle first because the web binary embeds `frontend/dist` at
compile time. Building with plain `cargo build --release` still works, but the
build script warns when the bundle is older than the frontend sources instead
of silently shipping the previous UI.

Outputs two binaries in `target/release/`:

- `dj-music-suite` — the CLI (no web stack; builds without bun entirely)
- `dj-music-suite-web` — the web UI; frontend sources live in
  `crates/server/frontend` (bun + Vite + Svelte + Tailwind), the built bundle
  is embedded into the binary, so the web app still ships as one file

`just dev-web` (or `make dev-web`) runs the frontend dev server with a proxy
to a locally running `dj-music-suite-web` for UI iteration.

## web ui

A local web UI covering the flows non-CLI users need: converting `.ncm`
files, importing music into the library, reorganizing it, removing
duplicates, transcoding formats and filling in missing cover art. The server
listens on `127.0.0.1` only, and files are uploaded into a local staging folder
(never sent anywhere). Cover lookup is the only feature that makes outbound
requests, and it sends metadata text only — never audio.

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

The 整理 (reorg), 去重 (dedup), 转码 (transcode) and 补封面 (cover) tabs run
the CLI's other commands on the configured library root, with the same
analyze → review → execute split:

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
- **转码** drives the same ffmpeg-backed conversion as the CLI: pick a target
  (mp3 / flac / m4a-AAC / m4a-ALAC / wav) and quality, and choose between
  converting in place and writing into a subfolder of the library root (the
  web UI only ever writes inside the library it manages, so a path outside it
  is rejected). The tab probes ffmpeg once on load and greys out any target
  the local build cannot encode, with the install hint shown when ffmpeg is
  missing entirely. The preview table shows each file's source codec, status
  and notes (lossy → lossless, dropped wav cover, unreadable tags), with only
  `ready` rows ticked; the per-file encoder position streams into the progress
  bar while a conversion runs. Executing with replace on leaves each source in
  `<root>/.dedup-trash/transcoded/`, so it appears in the 去重 tab's trash
  list and can be restored like any dedup loser.
- **补封面** scans the library (or a single file/subfolder) for missing
  artwork, searches MusicBrainz/Cover Art Archive and iTunes, and shows the
  selected candidate with a local thumbnail. `ready` rows are ticked by
  default; `review` rows stay unticked until the user chooses a candidate and
  opts in. The configured confidence threshold controls only automatic
  matches. Writing supports mp3/flac/m4a and uses the same atomic,
  verify-before-replace path as the CLI.

The library-wide tabs keep their analyzed plans in server memory (the
hash/score cache is reused when re-scanning), so a page reload restores the
plan and any running job's progress, while restarting the server means
analyzing again.

### Web-local data

The settings dialog shows the disk usage of everything the web UI created:
staging batches, MusicBrainz/iTunes cover caches, web-managed `meta` cover
directories, and the config file. Its cleanup action removes all staging and
cover-cache data, plus every `meta` directory the web UI created or can still
discover through a surviving `input-folder` batch pointer. Legacy `meta`
directories without an ownership marker are shown separately before deletion.
The config file and library-root setting can optionally be cleared too.

Cleanup never removes library audio, source folders, or files in a
user-selected direct-output directory. Newly created `meta` directories carry
a `.dj-music-suite-web-owned` marker and are also recorded in
`web-meta-dirs.json`, so they can be found after their staging batch is gone.
The cleanup job is blocked while another job is running.

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

Dependencies: everything is Rust, except `transcode`, which shells out to
**ffmpeg** (found on `$PATH`, or given explicitly). No other command needs an
external binary — `.ncm` decryption is built in via `ncm-core` — and nothing
is downloaded at build time.

## Acknowledgements

- [magic-akari/ncmc](https://github.com/magic-akari/ncmc) for `ncm-core`.
