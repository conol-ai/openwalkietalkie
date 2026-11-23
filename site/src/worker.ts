import { MESSAGES, type Lang, type Message } from "./messages.ts";

/** Hardware waitlist. Static pages stay on Workers Assets; only /api/*
 * invokes this handler. Signups also work as ordinary HTML form posts. */
const POLICY_VERSION = "2026-10-16";
const MAX_BODY = 4096;
const EMAIL = /^[a-z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-z0-9](?:[a-z0-9-]*[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]*[a-z0-9])?)+$/i;

/** The page's language, from the form's hidden `lang` field; English otherwise. */
function language(value: unknown): Lang {
  return typeof value === "string" && Object.hasOwn(MESSAGES, value) ? (value as Lang) : "en";
}

function response(status: number, body: string, type: string, extra: Record<string, string> = {}): Response {
  return new Response(body, {
    status,
    headers: {
      "Content-Type": type,
      "Cache-Control": "no-store",
      "X-Content-Type-Options": "nosniff",
      "X-Frame-Options": "DENY",
      "Referrer-Policy": "strict-origin-when-cross-origin",
      "Permissions-Policy": "camera=(), microphone=(), geolocation=()",
      "Content-Security-Policy": "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; font-src 'self'; base-uri 'none'; form-action 'self'; frame-ancestors 'none'; object-src 'none'",
      ...extra,
    },
  });
}

function reply(json: boolean, status: number, code: Message, extra: Record<string, string> = {}, lang: Lang = "en"): Response {
  const ok = status === 200;
  const t = MESSAGES[lang];
  const message = t[code];
  if (json) {
    return response(status, JSON.stringify(ok ? { ok, message } : { ok, error: code, message }), "application/json; charset=utf-8", extra);
  }
  if (ok) {
    return response(303, "", "text/plain; charset=utf-8", { Location: `${t.prefix}/waitlist/thanks/` });
  }
  // All copy comes from fixed strings below, never from submitted fields.
  return response(status, `<!doctype html><html lang="${lang}"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="robots" content="noindex"><title>${t.page_title}</title>
<link rel="stylesheet" href="/css/style.css"><link rel="stylesheet" href="/css/blog.css">
<script src="/js/theme-init.js"></script></head><body>
<main class="blog-main"><header class="post-head"><p class="section-kicker">OPENWALKIETALKIE</p>
<h1>${t.page_heading}</h1></header><div class="post-body"><p>${message}</p>
<p><a href="${t.prefix}/#waitlist">${t.page_return}</a></p>
<p><a href="${t.prefix}/privacy/">${t.privacy_policy}</a> · <a href="${t.prefix}/cookies/">${t.cookie_information}</a></p>
</div></main></body></html>`, "text/html; charset=utf-8", extra);
}

class BodyTooLarge extends Error {}

async function readBody(request: Request): Promise<string> {
  if (Number(request.headers.get("content-length")) > MAX_BODY) throw new BodyTooLarge();
  if (!request.body) return "";
  const reader = request.body.getReader();
  const decoder = new TextDecoder();
  let size = 0;
  let text = "";
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      size += value.byteLength;
      if (size > MAX_BODY) {
        await reader.cancel();
        throw new BodyTooLarge();
      }
      text += decoder.decode(value, { stream: true });
    }
    return text + decoder.decode();
  } finally {
    reader.releaseLock();
  }
}

async function signup(request: Request, env: Env): Promise<Response> {
  const contentType = (request.headers.get("content-type") || "").split(";")[0].trim().toLowerCase();
  const json = contentType !== "application/x-www-form-urlencoded";
  if (request.method !== "POST") {
    return reply(true, 405, "method_not_allowed", { Allow: "POST" });
  }
  const origin = request.headers.get("origin");
  if ((origin !== null && origin !== new URL(request.url).origin) || request.headers.get("sec-fetch-site") === "cross-site") {
    return reply(json, 403, "cross_origin");
  }
  if (contentType !== "application/json" && contentType !== "application/x-www-form-urlencoded") {
    return reply(true, 415, "content_type");
  }

  let fields: Record<string, unknown>;
  try {
    const body = await readBody(request);
    const parsed: unknown = json ? JSON.parse(body) : Object.fromEntries(new URLSearchParams(body));
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) throw new Error();
    fields = parsed as Record<string, unknown>;
  } catch (error) {
    return error instanceof BodyTooLarge
      ? reply(json, 413, "too_large")
      : reply(json, 400, "invalid_body");
  }
  const lang = language(fields.lang);
  // An off-screen honeypot catches simple form-filling bots without a tracker.
  if (typeof fields.company === "string" && fields.company.trim()) return reply(json, 200, "success", {}, lang);
  const email = typeof fields.email === "string" ? fields.email.trim().toLowerCase() : "";
  if (email.length > 254 || email.split("@")[0].length > 64 || !EMAIL.test(email) || email.startsWith(".") || email.includes("..") || email.includes(".@")) {
    return reply(json, 400, "invalid_email", {}, lang);
  }
  if (fields.consent !== true && fields.consent !== "yes") {
    return reply(json, 400, "consent_required", {}, lang);
  }

  try {
    // Cloudflare supplies this header. The address is used only for a short
    // rate-limit window and is never written into the waitlist or app logs.
    const key = `owt-waitlist:${request.headers.get("cf-connecting-ip") || "local"}`;
    const { success } = await env.WAITLIST_LIMITER.limit({ key });
    if (!success) {
      return reply(json, 429, "rate_limited", { "Retry-After": "60" }, lang);
    }
    await env.DB.prepare(
      "INSERT INTO waitlist (email, created_at, consent_version) VALUES (?, ?, ?) ON CONFLICT(email) DO NOTHING",
    ).bind(email, new Date().toISOString(), POLICY_VERSION).run();
  } catch {
    // No email, IP, submitted body, or database error text in logs.
    console.error(JSON.stringify({ event: "waitlist.unavailable" }));
    return reply(json, 503, "unavailable", { "Retry-After": "60" }, lang);
  }
  // New and existing addresses get exactly the same response.
  return reply(json, 200, "success", {}, lang);
}

export default {
  async fetch(request: Request, env: Env): Promise<Response> {
    const path = new URL(request.url).pathname;
    if (path === "/api/waitlist") return signup(request, env);
    if (path.startsWith("/api/")) return reply(true, 404, "not_found");
    return env.ASSETS.fetch(request);
  },
  async scheduled(_controller: ScheduledController, env: Env): Promise<void> {
    const cutoff = new Date();
    cutoff.setUTCFullYear(cutoff.getUTCFullYear() - 2);
    await env.DB.prepare("DELETE FROM waitlist WHERE created_at < ?").bind(cutoff.toISOString()).run();
  },
} satisfies ExportedHandler<Env>;
