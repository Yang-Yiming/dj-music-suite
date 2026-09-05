// Central job progress state, fed by the /api/events SSE stream.

import type { EndPayload, JobEvent, JobSnapshot } from "./api";

export interface LogLine {
  text: string;
  warn: boolean;
}

export const job = $state<{
  kind: string | null;
  running: boolean;
  current: number;
  total: number;
  log: LogLine[];
}>({
  kind: null,
  running: false,
  current: 0,
  total: 0,
  log: [],
});

let source: EventSource | null = null;

function applyEvent(ev: JobEvent) {
  switch (ev.type) {
    case "start":
      job.total = ev.total;
      job.current = 0;
      break;
    case "step":
      job.current += 1;
      break;
    case "line":
      job.log.push({ text: ev.text, warn: false });
      break;
    case "warn":
      job.log.push({ text: ev.text, warn: true });
      break;
  }
  if (job.log.length > 300) job.log.splice(0, job.log.length - 300);
}

/// Start watching a job that has just been created server-side. The stream
/// replays everything that already happened, so calling this right after the
/// POST that started the job never misses events. Resolves with the terminal
/// payload (done/error).
export function watchJob(kind: string): Promise<EndPayload> {
  stopWatching();
  job.kind = kind;
  job.running = true;
  job.current = 0;
  job.total = 0;
  job.log = [];

  return new Promise((resolve) => {
    const es = new EventSource("/api/events");
    source = es;
    es.addEventListener("job", (e) => {
      applyEvent(JSON.parse((e as MessageEvent).data));
    });
    es.addEventListener("end", (e) => {
      es.close();
      source = null;
      job.running = false;
      resolve(JSON.parse((e as MessageEvent).data) as EndPayload);
    });
    es.onerror = () => {
      // connection lost: the server is gone; surface as an error once
      if (job.running) {
        es.close();
        source = null;
        job.running = false;
        resolve({ type: "error", message: "与服务器的连接中断" });
      }
    };
  });
}

/// Restore log/progress visuals from a /api/job snapshot (page reload).
export function replaySnapshot(kind: string, events: JobEvent[]) {
  stopWatching();
  job.kind = kind;
  job.running = true;
  job.current = 0;
  job.total = 0;
  job.log = [];
  for (const ev of events) applyEvent(ev);
}

export function stopWatching() {
  source?.close();
  source = null;
  job.running = false;
}

/// Resume a running job after a page reload: replays its events live.
export function resumeRunningJob(snapshot: JobSnapshot): Promise<EndPayload> {
  replaySnapshot(snapshot.kind, snapshot.events);
  return new Promise((resolve) => {
    const es = new EventSource("/api/events");
    source = es;
    let sent = snapshot.events.length;
    es.addEventListener("job", (e) => {
      // the stream replays from zero; skip what we already have
      if (sent > 0) {
        sent -= 1;
        return;
      }
      applyEvent(JSON.parse((e as MessageEvent).data));
    });
    es.addEventListener("end", (e) => {
      es.close();
      source = null;
      job.running = false;
      resolve(JSON.parse((e as MessageEvent).data) as EndPayload);
    });
    es.onerror = () => {
      es.close();
      source = null;
      job.running = false;
      resolve({ type: "error", message: "与服务器的连接中断" });
    };
  });
}
