// Early boot: starts the first API calls while the app bundle downloads,
// and applies the font stylesheet the head preloads. Plain JavaScript,
// served as a classic script from /early.js (not bundled): a classic
// script runs during parsing, before the deferred app bundle, so the API
// calls start in parallel with the bundle download. It must stay a plain
// external file, never inline: the app shell ships a strict CSP with no
// 'unsafe-inline' anywhere.
// The font swap runs here because the preload link (see partials/head.html)
// never applies itself; without an inline onload handler (forbidden by
// the CSP) this turns it into a stylesheet. A link that already finished
// loading applies from cache.
(function () {
  var h = location.hash;
  if (h && h !== '#/' && h !== '#') return;
  var get = function (u) {
    return fetch(u, { credentials: 'same-origin', headers: { 'content-type': 'application/json' } });
  };
  try {
    window.__early = {
      '/api/v1/session': get('/api/v1/session'),
      '/api/v1/mailboxes': get('/api/v1/mailboxes'),
    };
  } catch (e) { /* fetch unavailable; the app fetches normally */ }

  var links = document.querySelectorAll('link[rel="preload"][as="style"]');
  for (var i = 0; i < links.length; i++) links[i].rel = 'stylesheet';
})();
