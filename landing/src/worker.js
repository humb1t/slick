// slick landing: static page from ./public, plus the feature-request API.
//
//   POST /api/feature-request        { text, email, token, lang } → checks Turnstile, stores a pending
//                                     proposal in D1 and emails a confirmation link → { id }
//   GET  /api/confirm?t=…            confirms it, then redirects back to the page's #feature-requests DM
//   GET  /api/feature-request/:id    → { status } so the page can tell when a proposal was confirmed
//
// Bindings: DB (D1), EMAIL (send_email), ASSETS. Secret: TURNSTILE_SECRET. Var: MAIL_FROM.

const MAX_TEXT = 4000;
const CONFIRM_DAYS = 7;
const EMAIL_RE = /^[^\s@]+@[^\s@]+\.[^\s@]+$/;

export default {
  async fetch(request, env) {
    const url = new URL(request.url);
    const { pathname } = url;
    if (pathname === "/api/feature-request" && request.method === "POST") return create(request, env, url);
    if (pathname === "/api/confirm" && request.method === "GET") return confirm(url, env);
    const m = pathname.match(/^\/api\/feature-request\/([0-9a-f-]{36})$/);
    if (m && request.method === "GET") return status(m[1], env);
    if (pathname.startsWith("/api/")) return json({ error: "not_found" }, 404);
    return env.ASSETS.fetch(request);
  },
};

async function create(request, env, url) {
  let data;
  try { data = await request.json(); } catch { return json({ error: "bad_json" }, 400); }

  const text = String(data.text || "").trim();
  const email = String(data.email || "").trim().toLowerCase();
  const token = String(data.token || "");
  const lang = data.lang === "pt" ? "pt" : "en";
  if (!text || text.length > MAX_TEXT) return json({ error: "bad_text" }, 400);
  if (email.length > 254 || !EMAIL_RE.test(email)) return json({ error: "bad_email" }, 400);
  if (!token) return json({ error: "no_token" }, 400);

  const form = new FormData();
  form.append("secret", env.TURNSTILE_SECRET);
  form.append("response", token);
  const ip = request.headers.get("CF-Connecting-IP");
  if (ip) form.append("remoteip", ip);
  const check = await fetch("https://challenges.cloudflare.com/turnstile/v0/siteverify", { method: "POST", body: form })
    .then(r => r.json())
    .catch(() => null);
  if (!check || !check.success) return json({ error: "captcha" }, 403);

  const id = crypto.randomUUID();
  const confirmToken = hex(crypto.getRandomValues(new Uint8Array(32)));
  await env.DB.prepare("INSERT INTO feature_requests (id, text, email, lang, confirm_token) VALUES (?, ?, ?, ?, ?)")
    .bind(id, text, email, lang, confirmToken)
    .run();

  const link = `${url.origin}/api/confirm?t=${confirmToken}`;
  try {
    await env.EMAIL.send({ to: email, from: { email: env.MAIL_FROM, name: "slick" }, ...mail(lang, text, link) });
  } catch (e) {
    await env.DB.prepare("DELETE FROM feature_requests WHERE id = ?").bind(id).run();
    console.error("send failed", e && e.code, e && e.message);
    return json({ error: "email", code: e && e.code }, 502);
  }
  return json({ id });
}

async function confirm(url, env) {
  const token = url.searchParams.get("t") || "";
  const back = q => Response.redirect(`${url.origin}/?${q}#requests`, 303);
  if (!/^[0-9a-f]{64}$/.test(token)) return back("confirm=invalid");

  const row = await env.DB.prepare(
    `UPDATE feature_requests SET status = 'confirmed', confirmed_at = datetime('now')
     WHERE confirm_token = ? AND status = 'pending' AND created_at > datetime('now', '-${CONFIRM_DAYS} days')
     RETURNING id`,
  ).bind(token).first();
  if (row) return back(`confirmed=${row.id}`);

  // Clicked twice: still a success.
  const done = await env.DB.prepare("SELECT id FROM feature_requests WHERE confirm_token = ? AND status = 'confirmed'")
    .bind(token)
    .first();
  return back(done ? `confirmed=${done.id}` : "confirm=invalid");
}

async function status(id, env) {
  const row = await env.DB.prepare("SELECT status FROM feature_requests WHERE id = ?").bind(id).first();
  return row ? json({ status: row.status }) : json({ error: "not_found" }, 404);
}

function mail(lang, text, link) {
  const quote = escapeHtml(text).replace(/\n/g, "<br>");
  if (lang === "pt") {
    return {
      subject: "Confirme sua proposta para o slick",
      text: `Você mandou esta proposta para o slick:\n\n${text}\n\nPara enviar, confirme abrindo este link:\n${link}\n\nSe não foi você, ignore este e-mail.`,
      html: page("Confirme sua proposta", "Você mandou esta proposta para o slick:", quote, link, "Confirmar e enviar", "Se não foi você, ignore este e-mail."),
    };
  }
  return {
    subject: "Confirm your proposal for slick",
    text: `You sent this proposal to slick:\n\n${text}\n\nTo send it, confirm by opening this link:\n${link}\n\nIf this wasn't you, ignore this email.`,
    html: page("Confirm your proposal", "You sent this proposal to slick:", quote, link, "Confirm and send", "If this wasn't you, ignore this email."),
  };
}

function page(title, intro, quote, link, cta, footer) {
  return `<!doctype html><html><body style="margin:0;padding:24px;background:#282828;color:#ebdbb2;font:15px/1.5 ui-monospace,Menlo,Consolas,monospace">
<p style="margin:0 0 16px;font-weight:700;color:#fbf1c7">${title}</p>
<p style="margin:0 0 8px">${intro}</p>
<p style="margin:0 0 20px;padding-left:12px;border-left:2px solid #665c54">${quote}</p>
<p style="margin:0 0 20px"><a href="${link}" style="display:inline-block;padding:8px 16px;background:#ecb22e;color:#282828;font-weight:700;text-decoration:none">${cta}</a></p>
<p style="margin:0;color:#a89984">${footer}</p>
</body></html>`;
}

const json = (body, status = 200) =>
  new Response(JSON.stringify(body), { status, headers: { "content-type": "application/json" } });
const hex = bytes => [...bytes].map(b => b.toString(16).padStart(2, "0")).join("");
const escapeHtml = s => s.replace(/[&<>"']/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);
