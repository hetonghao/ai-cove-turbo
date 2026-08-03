import { createServer } from "node:http";
import { promises as fs } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = path.resolve(fileURLToPath(new URL(".", import.meta.url)));
const rawPort = process.env.PORT ?? process.argv[2] ?? "4173";
const port = Number(rawPort);

if (!Number.isInteger(port) || port < 0 || port > 65535) {
  console.error(`Invalid port: ${rawPort}`);
  process.exit(1);
}

const contentTypes = {
  ".css": "text/css; charset=utf-8",
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".png": "image/png",
  ".svg": "image/svg+xml",
  ".txt": "text/plain; charset=utf-8",
};

function responseText(response, statusCode, message) {
  response.writeHead(statusCode, {
    "Cache-Control": "no-store",
    "Content-Type": "text/plain; charset=utf-8",
  });
  response.end(message);
}

function resolveFile(requestUrl) {
  let pathname;
  try {
    pathname = decodeURIComponent(new URL(requestUrl, "http://127.0.0.1").pathname);
  } catch {
    return null;
  }

  const relativePath = pathname === "/" ? "index.html" : pathname.replace(/^\/+/, "");
  const filePath = path.resolve(root, relativePath);
  const insideRoot = filePath === root || filePath.startsWith(`${root}${path.sep}`);
  return insideRoot ? filePath : null;
}

const server = createServer(async (request, response) => {
  if (!request.url) {
    responseText(response, 400, "Bad request\n");
    return;
  }
  if (request.method !== "GET" && request.method !== "HEAD") {
    responseText(response, 405, "Method not allowed\n");
    return;
  }

  let filePath = resolveFile(request.url);
  if (!filePath) {
    responseText(response, 400, "Bad request\n");
    return;
  }

  try {
    const initialStats = await fs.stat(filePath);
    if (initialStats.isDirectory()) filePath = path.join(filePath, "index.html");
    const body = await fs.readFile(filePath);
    const extension = path.extname(filePath).toLowerCase();
    response.writeHead(200, {
      "Cache-Control": "no-store",
      "Content-Length": body.byteLength,
      "Content-Type": contentTypes[extension] ?? "application/octet-stream",
    });
    if (request.method === "HEAD") response.end();
    else response.end(body);
  } catch (error) {
    if (error?.code === "ENOENT" || error?.code === "ENOTDIR") {
      responseText(response, 404, "Not found\n");
      return;
    }
    console.error(error);
    responseText(response, 500, "Internal server error\n");
  }
});

server.on("error", (error) => {
  console.error(`[turbo] ${error.message}`);
  process.exitCode = 1;
});

function shutdown() {
  server.close(() => process.exit(0));
}

process.once("SIGINT", shutdown);
process.once("SIGTERM", shutdown);

server.listen({ host: "127.0.0.1", port }, () => {
  const address = server.address();
  const activePort = typeof address === "object" && address ? address.port : port;
  console.log(`[turbo] prototype server: http://127.0.0.1:${activePort}`);
});
