import DOMPurify from "dompurify";
import { marked } from "marked";

marked.setOptions({ gfm: true, breaks: false });

/** Markdown -> sanitized HTML. Images are stripped so model output can never
 *  make the app fetch remote URLs. */
export function renderMarkdown(src: string): string {
  const html = marked.parse(src, { async: false }) as string;
  return DOMPurify.sanitize(html, {
    FORBID_TAGS: ["img", "style", "iframe", "form", "input", "video", "audio", "source", "object", "embed"],
    FORBID_ATTR: ["style", "srcset"],
  });
}

export function timeAgo(ms: number | null | undefined): string {
  if (!ms) return "never";
  const s = Math.max(0, (Date.now() - ms) / 1000);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)}m ago`;
  if (s < 86400) return `${Math.floor(s / 3600)}h ago`;
  return `${Math.floor(s / 86400)}d ago`;
}

export function releasedLabel(secs: number | null): string {
  if (!secs) return "";
  const d = Math.floor((Date.now() / 1000 - secs) / 86400);
  if (d < 1) return "today";
  if (d < 60) return `${d}d old`;
  if (d < 730) return `${Math.round(d / 30)}mo old`;
  return `${(d / 365).toFixed(1)}y old`;
}

export function ctxLabel(n: number | null): string {
  if (!n) return "—";
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(n % 1_000_000 ? 1 : 0)}M`;
  return `${Math.round(n / 1000)}K`;
}

export function pct(n: number | null | undefined): string {
  return n == null ? "—" : `${Math.round(n * 100)}%`;
}

/** "this Mac" / "this PC" / "this computer" for UI copy. */
export const DEVICE: string = (() => {
  const ua = typeof navigator === "undefined" ? "" : navigator.userAgent;
  if (/Windows/i.test(ua)) return "this PC";
  if (/Macintosh|Mac OS X/i.test(ua)) return "this Mac";
  return "this computer";
})();
