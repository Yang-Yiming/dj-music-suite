//! Guard against shipping a binary whose web UI is older than its sources.
//!
//! `Assets` embeds `frontend/dist` at compile time. Running `cargo build
//! --release` without `bun run build` (or `just web` / `just build`) happily
//! embeds the previous bundle, so the binary serves a UI that no longer
//! matches the code. Cargo cannot know that by itself, so this build script
//! compares timestamps and warns instead of failing the build (building the
//! server alone must still work for people without bun).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let frontend = root.join("frontend");

    for path in [
        "frontend/index.html",
        "frontend/package.json",
        "frontend/vite.config.ts",
        "frontend/svelte.config.js",
        "frontend/src",
        "frontend/dist/index.html",
    ] {
        println!("cargo:rerun-if-changed={path}");
    }

    let dist = frontend.join("dist").join("index.html");
    let Some(dist_time) = modified(&dist) else {
        println!(
            "cargo:warning=frontend/dist is missing: the binary will not serve a web UI. Run `just web` (bun run build) before building."
        );
        return;
    };

    let mut newest: Option<(SystemTime, PathBuf)> = None;
    for needle in [frontend.join("src"), frontend.join("index.html")] {
        newest_of(&needle, &mut newest);
    }
    let Some((source_time, source_path)) = newest else {
        return;
    };

    if source_time > dist_time {
        let name = source_path
            .strip_prefix(&root)
            .unwrap_or(&source_path)
            .display();
        println!(
            "cargo:warning=frontend/dist is older than {name}: the binary would embed the previous web UI. Run `just web` (or `bun run build` in crates/server/frontend) before `cargo build --release`."
        );
    }
}

fn modified(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|meta| meta.modified()).ok()
}

fn newest_of(path: &Path, newest: &mut Option<(SystemTime, PathBuf)>) {
    let Ok(meta) = std::fs::metadata(path) else {
        return;
    };
    if meta.is_dir() {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            newest_of(&entry.path(), newest);
        }
        return;
    }
    let Ok(time) = meta.modified() else {
        return;
    };
    if newest.as_ref().is_none_or(|(newest, _)| time > *newest) {
        *newest = Some((time, path.to_path_buf()));
    }
}
