import { invoke as tauriInvoke } from "@tauri-apps/api/core";

export type LogLevel = "error" | "warn" | "info" | "debug" | "trace";
export type LogFields = Record<string, unknown>;

const SENSITIVE = [
  "path",
  "filename",
  "search",
  "caption",
  "keyword",
  "person",
  "hash",
  "url",
  "name",
];

function redact(value: unknown): unknown {
  if (value == null) return value;
  if (typeof value === "string") return value.length > 200 ? `${value.slice(0, 200)}…` : value;
  if (typeof value !== "object") return value;
  if (Array.isArray(value)) return value.slice(0, 20).map(redact);
  const out: Record<string, unknown> = {};
  for (const [key, item] of Object.entries(value as Record<string, unknown>)) {
    const k = key.toLowerCase();
    out[key] = SENSITIVE.some((s) => k.includes(s)) ? "[redacted]" : redact(item);
  }
  return out;
}

function errorSummary(err: unknown): LogFields {
  if (err instanceof Error) return { errorType: err.name };
  return { errorType: typeof err };
}

async function send(level: LogLevel, target: string, message: string, fields?: LogFields) {
  const safeFields = redact(fields ?? {}) as LogFields;
  if (import.meta.env.DEV && !("__TAURI_INTERNALS__" in window)) {
    const fn = level === "error" ? console.error : level === "warn" ? console.warn : console.info;
    fn(`[${target}] ${message}`, safeFields);
    return;
  }
  try {
    await tauriInvoke("frontend_log", { level, target, message, fields: safeFields });
  } catch {
    // Logging must never affect app behavior.
  }
}

export const log = {
  error: (target: string, message: string, fields?: LogFields) => void send("error", target, message, fields),
  warn: (target: string, message: string, fields?: LogFields) => void send("warn", target, message, fields),
  info: (target: string, message: string, fields?: LogFields) => void send("info", target, message, fields),
  debug: (target: string, message: string, fields?: LogFields) => void send("debug", target, message, fields),
  trace: (target: string, message: string, fields?: LogFields) => void send("trace", target, message, fields),
  errorSummary,
};

export async function measure<T>(target: string, op: string, fn: () => Promise<T>, fields?: LogFields): Promise<T> {
  const start = performance.now();
  log.debug(target, "start", { op, ...fields });
  try {
    const result = await fn();
    log.debug(target, "success", { op, durationMs: Math.round(performance.now() - start), ...fields });
    return result;
  } catch (err) {
    log.warn(target, "failed", {
      op,
      durationMs: Math.round(performance.now() - start),
      ...errorSummary(err),
      ...fields,
    });
    throw err;
  }
}
