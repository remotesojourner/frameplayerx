// The relay FramePlayerX sends a crash report or diagnostics to when a user
// chooses to. It files each as an issue in a private GitHub repository, so
// the token that can do that stays here instead of in the app, and answers
// with the issue's number. Set up as docs/DEVELOPMENT.md (Crash reports)
// describes; the secret GITHUB_TOKEN may only write issues of REPO.
//
// It also keeps the release list update checks read (/updates/releases).

const MAX_BODY = 96 * 1024;
// GitHub takes issue bodies up to 65536 characters.
const MAX_TEXT = 60000;
// A report too long keeps its start, where the versions and what went
// wrong are, and its end, where the latest of the log is.
const KEEP_START = 8000;
const KINDS = ["crash", "diagnostics"];

export default {
  async fetch(request, env, ctx) {
    const url = new URL(request.url);
    if (request.method === "GET" && url.pathname === "/updates/releases") {
      return releases(request, env, ctx);
    }
    if (request.method !== "POST" || url.pathname !== "/report") {
      return new Response("Not found", { status: 404 });
    }
    // The address only counts reports here; it is not kept.
    if (env.LIMITER) {
      const key = request.headers.get("cf-connecting-ip") ?? "unknown";
      const { success } = await env.LIMITER.limit({ key });
      if (!success) {
        return new Response("Too many reports", { status: 429 });
      }
    }
    const raw = await request.text();
    if (raw.length > MAX_BODY) {
      return new Response("Too large", { status: 413 });
    }
    let report;
    try {
      report = JSON.parse(raw);
    } catch {
      return new Response("Not JSON", { status: 400 });
    }
    // Copies released before diagnostics send no kind.
    const { app, kind = "crash", version, summary, text } = report ?? {};
    if (
      app !== "frameplayerx" ||
      !KINDS.includes(kind) ||
      ![version, summary, text].every((v) => typeof v === "string")
    ) {
      return new Response("Not a FramePlayerX report", { status: 400 });
    }
    const kept =
      text.length > MAX_TEXT
        ? text.slice(0, KEEP_START) + "\n…\n" + text.slice(-(MAX_TEXT - KEEP_START))
        : text;
    // A fence longer than any run of backticks in the report.
    const longest = Math.max(0, ...(kept.match(/`+/g) ?? []).map((run) => run.length));
    const fence = "`".repeat(Math.max(3, longest + 1));
    const diagnostics = kind === "diagnostics";
    const issue = {
      title: `${version.slice(0, 40)}${diagnostics ? " diagnostics" : ""}: ${summary.slice(0, 200)}`,
      body: `${diagnostics ? "Diagnostics sent from FramePlayerX." : "Sent from FramePlayerX after a crash."}\n\n${fence}text\n${kept}\n${fence}\n`,
    };
    const reply = await fetch(`https://api.github.com/repos/${env.REPO}/issues`, {
      method: "POST",
      headers: {
        Accept: "application/vnd.github+json",
        Authorization: `Bearer ${env.GITHUB_TOKEN}`,
        "User-Agent": "frameplayerx-crash-relay",
        "X-GitHub-Api-Version": "2022-11-28",
      },
      body: JSON.stringify(issue),
    });
    if (!reply.ok) {
      console.error(`GitHub answered ${reply.status}: ${await reply.text()}`);
      return new Response("Could not file the report", { status: 502 });
    }
    const { number } = await reply.json();
    return Response.json({ number }, { status: 201 });
  },
};

// How long a copy of the release list is kept: each of Cloudflare's data
// centres asks GitHub for it at most this often, however many players
// check.
const RELEASES_TTL = 300;

// GitHub's list of RELEASES_REPO's recent releases, cut to what the update
// check reads (fp-updater's RELEASES_URL), so that players don't spend
// their network's 60 requests an hour to GitHub. Its answers are trusted
// no further than GitHub's: each release's manifest is signature-checked.
async function releases(request, env, ctx) {
  const key = new Request(new URL("/updates/releases", request.url));
  const kept = await caches.default.match(key);
  if (kept) {
    return kept;
  }
  const headers = {
    Accept: "application/vnd.github+json",
    "User-Agent": "frameplayerx-website",
    "X-GitHub-Api-Version": "2022-11-28",
  };
  // Cloudflare's addresses are shared, so without a token GitHub's limit
  // there would soon be spent too.
  if (env.RELEASES_TOKEN) {
    headers.Authorization = `Bearer ${env.RELEASES_TOKEN}`;
  }
  const reply = await fetch(
    `https://api.github.com/repos/${env.RELEASES_REPO}/releases?per_page=10`,
    { headers },
  );
  if (!reply.ok) {
    return new Response(`GitHub answered ${reply.status}`, { status: 502 });
  }
  const list = (await reply.json()).map((r) => ({
    tag_name: r.tag_name,
    draft: r.draft,
    prerelease: r.prerelease,
    assets: (r.assets ?? []).map((a) => ({
      name: a.name,
      browser_download_url: a.browser_download_url,
    })),
  }));
  const response = Response.json(list, {
    headers: { "Cache-Control": `public, max-age=${RELEASES_TTL}` },
  });
  ctx.waitUntil(caches.default.put(key, response.clone()));
  return response;
}
