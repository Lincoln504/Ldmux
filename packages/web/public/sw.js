/*
 * ldmux service worker: cache the app shell so the client opens instantly and
 * survives a flaky mobile network. Terminal traffic is never cached — /ws is
 * excluded explicitly (WebSockets bypass fetch anyway, but be explicit).
 */
"use strict";

const VERSION = "ldmux-v2";
const SHELL = ["/", "/index.html", "/app.css", "/core.js", "/app.js", "/manifest.webmanifest", "/icon.svg", "/vendor/xterm.css", "/vendor/xterm.js", "/vendor/addon-fit.js"];

self.addEventListener("install", (event) => {
  event.waitUntil(caches.open(VERSION).then((cache) => cache.addAll(SHELL)).then(() => self.skipWaiting()));
});

self.addEventListener("activate", (event) => {
  event.waitUntil(
    caches
      .keys()
      .then((keys) => Promise.all(keys.filter((k) => k !== VERSION).map((k) => caches.delete(k))))
      .then(() => self.clients.claim()),
  );
});

self.addEventListener("fetch", (event) => {
  const url = new URL(event.request.url);
  if (event.request.method !== "GET" || url.origin !== self.location.origin || url.pathname.startsWith("/ws")) return;

  // Navigation: network-first so a redeploy is picked up, cache as fallback.
  if (event.request.mode === "navigate") {
    event.respondWith(
      fetch(event.request)
        .then((res) => {
          const copy = res.clone();
          caches.open(VERSION).then((c) => c.put("/index.html", copy));
          return res;
        })
        .catch(() => caches.match("/index.html")),
    );
    return;
  }

  // Assets: cache-first.
  event.respondWith(
    caches.match(event.request).then((hit) => hit || fetch(event.request).then((res) => {
      if (res.ok) {
        const copy = res.clone();
        caches.open(VERSION).then((c) => c.put(event.request, copy));
      }
      return res;
    })),
  );
});