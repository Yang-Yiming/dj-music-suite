import {
  startMetadataScan,
  startMetadataWrite,
  type MetadataPatch,
  type TrackMetadata,
} from "./api";
import { watchJob } from "./job.svelte";

export interface MetadataDraft {
  title: string;
  artist: string;
  album: string;
  album_artist: string;
  year: string;
  genre: string;
  track_number: string;
  disc_number: string;
}

const EMPTY_DRAFT: MetadataDraft = {
  title: "",
  artist: "",
  album: "",
  album_artist: "",
  year: "",
  genre: "",
  track_number: "",
  disc_number: "",
};

export const metadata = $state<{
  /** empty = configured library root */
  input: string;
  search: string;
  tracks: TrackMetadata[];
  selectedPath: string | null;
  original: TrackMetadata | null;
  draft: MetadataDraft;
  error: string;
  writeError: string;
  scanning: boolean;
  saving: boolean;
  savedPath: string | null;
}>({
  input: "",
  search: "",
  tracks: [],
  selectedPath: null,
  original: null,
  draft: { ...EMPTY_DRAFT },
  error: "",
  writeError: "",
  scanning: false,
  saving: false,
  savedPath: null,
});

function draftFrom(track: TrackMetadata): MetadataDraft {
  return {
    title: track.title ?? "",
    artist: track.artist ?? "",
    album: track.album ?? "",
    album_artist: track.album_artist ?? "",
    year: track.year ?? "",
    genre: track.genre ?? "",
    track_number: track.track_number ?? "",
    disc_number: track.disc_number ?? "",
  };
}

export function metadataDirty(): boolean {
  if (!metadata.original) return false;
  return Object.keys(EMPTY_DRAFT).some((key) => {
    const field = key as keyof MetadataDraft;
    return metadata.draft[field] !== (metadata.original?.[field] ?? "");
  });
}

function replaceTrack(track: TrackMetadata) {
  const index = metadata.tracks.findIndex((item) => item.path === track.path);
  if (index >= 0) metadata.tracks[index] = track;
}

export function selectMetadataTrack(path: string) {
  if (path === metadata.selectedPath) return;
  if (metadataDirty() && !window.confirm("当前歌曲有未保存的修改，确定切换吗？")) return;
  const track = metadata.tracks.find((item) => item.path === path);
  if (!track) return;
  metadata.selectedPath = path;
  metadata.original = track;
  metadata.draft = draftFrom(track);
  metadata.writeError = "";
  metadata.savedPath = null;
}

export function resetMetadataDraft() {
  if (!metadata.original) return;
  metadata.draft = draftFrom(metadata.original);
  metadata.writeError = "";
  metadata.savedPath = null;
}

export async function doMetadataScan(libraryRoot: string | null) {
  if (metadataDirty() && !window.confirm("当前歌曲有未保存的修改，确定重新扫描吗？")) return;
  metadata.error = "";
  metadata.writeError = "";
  metadata.savedPath = null;
  metadata.scanning = true;
  metadata.tracks = [];
  metadata.selectedPath = null;
  metadata.original = null;
  metadata.draft = { ...EMPTY_DRAFT };
  try {
    const input = metadata.input.trim() || libraryRoot || "";
    await startMetadataScan(input);
    const end = await watchJob("metadata-scan");
    if (end.type === "done") {
      metadata.tracks = end.result as TrackMetadata[];
      if (metadata.tracks.length) selectMetadataTrack(metadata.tracks[0].path);
    } else {
      metadata.error = end.type === "error" ? end.message : "没有收到歌曲扫描结果";
    }
  } catch (e) {
    metadata.error = e instanceof Error ? e.message : String(e);
  } finally {
    metadata.scanning = false;
  }
}

function buildPatch(): MetadataPatch {
  const original = metadata.original;
  if (!original) return {};
  const patch: MetadataPatch = {};
  for (const key of Object.keys(EMPTY_DRAFT) as (keyof MetadataDraft)[]) {
    const value = metadata.draft[key];
    if (value !== (original[key] ?? "")) patch[key] = value;
  }
  return patch;
}

export async function saveMetadata() {
  const original = metadata.original;
  if (!original) return;
  const patch = buildPatch();
  if (Object.keys(patch).length === 0) return;

  metadata.saving = true;
  metadata.writeError = "";
  metadata.savedPath = null;
  try {
    await startMetadataWrite({
      path: original.path,
      expected_size: original.size,
      expected_mtime_nanos: original.mtime_nanos,
      patch,
    });
    const end = await watchJob("metadata-write");
    if (end.type === "done") {
      const saved = end.result as TrackMetadata;
      replaceTrack(saved);
      metadata.selectedPath = saved.path;
      metadata.original = saved;
      metadata.draft = draftFrom(saved);
      metadata.savedPath = saved.path;
    } else if (end.type === "error") {
      metadata.writeError = end.message;
    } else {
      metadata.writeError = "没有收到元信息写入结果";
    }
  } catch (e) {
    metadata.writeError = e instanceof Error ? e.message : String(e);
  } finally {
    metadata.saving = false;
  }
}

/// Apply a completed metadata job after a page reload or background resume.
export function applyMetadataFinish(
  kind: "metadata-scan" | "metadata-write",
  end: { type: string; result?: unknown; message?: string },
) {
  if (end.type === "error") {
    if (kind === "metadata-scan") metadata.error = end.message ?? "任务失败";
    else metadata.writeError = end.message ?? "任务失败";
    return;
  }
  if (end.type !== "done") return;
  if (kind === "metadata-scan") {
    metadata.tracks = end.result as TrackMetadata[];
    metadata.selectedPath = null;
    metadata.original = null;
    metadata.draft = { ...EMPTY_DRAFT };
    if (metadata.tracks.length) selectMetadataTrack(metadata.tracks[0].path);
  } else {
    const saved = end.result as TrackMetadata;
    replaceTrack(saved);
    if (!metadata.tracks.some((track) => track.path === saved.path)) {
      metadata.tracks = [saved, ...metadata.tracks];
    }
    metadata.selectedPath = saved.path;
    metadata.original = saved;
    metadata.draft = draftFrom(saved);
    metadata.savedPath = saved.path;
  }
}
