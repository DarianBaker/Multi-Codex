#!/usr/bin/env node
/**
 * MC-1 throwaway proxy: logs every Codex request and forwards it upstream.
 *
 * Usage:
 *   node scripts/local-proxy.mjs [PORT]
 *   MC2_AUTH_FILE=/path/to/second/auth.json node scripts/local-proxy.mjs [PORT]
 *
 * Then add ONE line to ~/.codex/config.toml:
 *   openai_base_url = "http://localhost:PORT/backend-api/codex"
 *
 * Works with ChatGPT-account auth (cookies/tokens forwarded verbatim).
 */

import http from "node:http";
import https from "node:https";
import fs from "node:fs";

const PORT = parseInt(process.argv[2] ?? "47839", 10);
const UPSTREAM_HOST = "chatgpt.com";
const UPSTREAM_BASE = "/backend-api";
const authFile = process.env.MC2_AUTH_FILE;

let replacementHeaders;
if (authFile) {
  const auth = JSON.parse(fs.readFileSync(authFile, "utf8"));
  const accessToken = auth?.tokens?.access_token;
  const accountId = auth?.tokens?.account_id;
  if (typeof accessToken !== "string" || typeof accountId !== "string") {
    throw new Error("MC2_AUTH_FILE must contain an access token and account ID");
  }
  replacementHeaders = {
    authorization: `Bearer ${accessToken}`,
    "chatgpt-account-id": accountId,
  };
}

function redactHeaders(headers) {
  const sensitive = new Set(["authorization", "cookie", "set-cookie"]);
  return Object.fromEntries(
    Object.entries(headers).map(([k, v]) => [
      k,
      k.toLowerCase() === "authorization" && v?.startsWith("Bearer ")
        ? "Bearer [redacted]"
        : sensitive.has(k.toLowerCase())
          ? "[redacted]"
          : v,
    ])
  );
}

const server = http.createServer((req, res) => {
  const upstreamPath = req.url; // already includes /backend-api/...

  console.log(`\n→ ${req.method} ${req.url}`);
  console.log("  headers:", JSON.stringify(redactHeaders(req.headers), null, 2));

  req.on("error", (err) => console.error(`  req error: ${err.message}`));
  res.on("error", (err) => console.error(`  res error: ${err.message}`));

  const chunks = [];
  req.on("data", (chunk) => chunks.push(chunk));
  req.on("end", () => {
    const body = Buffer.concat(chunks);
    if (body.length > 0) {
      const preview = body.slice(0, 512).toString("utf8");
      console.log(`  body (${body.length}B): ${preview}${body.length > 512 ? "…" : ""}`);
    }

    const upstreamHeaders = {
      ...req.headers,
      ...replacementHeaders,
      host: UPSTREAM_HOST,
    };
    if (replacementHeaders) {
      console.log("  replaced authorization and chatgpt-account-id before forwarding");
    }
    // Strip hop-by-hop headers that must not be forwarded
    delete upstreamHeaders["transfer-encoding"];
    delete upstreamHeaders["connection"];
    delete upstreamHeaders["keep-alive"];
    delete upstreamHeaders["proxy-connection"];
    delete upstreamHeaders["upgrade"];
    delete upstreamHeaders["te"];
    delete upstreamHeaders["trailers"];

    const options = {
      hostname: UPSTREAM_HOST,
      port: 443,
      path: upstreamPath,
      method: req.method,
      headers: upstreamHeaders,
    };

    const upstreamReq = https.request(options, (upstreamRes) => {
      console.log(`← ${upstreamRes.statusCode} (${req.method} ${req.url})`);

      // Forward status and headers back to Codex
      const responseHeaders = { ...upstreamRes.headers };
      delete responseHeaders["transfer-encoding"];
      res.writeHead(upstreamRes.statusCode, responseHeaders);

      // Pipe response (handles SSE/chunked streaming transparently).
      // Destroy upstream if client disconnects to avoid hanging connections.
      upstreamRes.pipe(res, { end: true });
      res.on("close", () => upstreamRes.destroy());
    });

    upstreamReq.on("error", (err) => {
      console.error(`  upstream error: ${err.message}`);
      if (!res.headersSent) {
        res.writeHead(502);
      }
      res.end(`Proxy upstream error: ${err.message}`);
    });

    if (body.length > 0) {
      upstreamReq.write(body);
    }
    upstreamReq.end();
  });
});

server.on("error", (err) => {
  console.error(`Failed to start proxy: ${err.message}`);
  if (err.code === "EACCES" || err.code === "EADDRINUSE") {
    console.error(`Port ${PORT} unavailable. Choose another high port outside the dynamic range.`);
  }
  process.exit(1);
});

server.listen(PORT, "127.0.0.1", () => {
  console.log(`Proxy listening on http://127.0.0.1:${PORT}`);
  console.log(`Forwarding → https://${UPSTREAM_HOST}${UPSTREAM_BASE}/...`);
  console.log();
  console.log("Add this ONE line to ~/.codex/config.toml:");
  console.log(`  openai_base_url = "http://localhost:${PORT}/backend-api/codex"`);
  console.log();
  console.log("Then start a normal Codex session. Ctrl-C to stop.");
});
