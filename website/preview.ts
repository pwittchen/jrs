// Serves dist/ as a static site would be served, to check a production build.
const root = new URL("./dist/", import.meta.url).pathname;

const server = Bun.serve({
  port: Number(process.env.PORT ?? 4173),
  async fetch(req) {
    let path = decodeURIComponent(new URL(req.url).pathname);
    if (path.endsWith("/")) path += "index.html";
    const file = Bun.file(root + path.replace(/^\/+/, ""));
    return (await file.exists()) ? new Response(file) : new Response("Not found", { status: 404 });
  },
});

console.log(`Serving dist/ at ${server.url}`);
