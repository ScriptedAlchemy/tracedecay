/* tracedecay dashboard placeholder shell.
 * The dashboard UI is being rebuilt from scratch. This minimal shell exists
 * only so the Rust build (build.rs / src/dashboard/assets.rs) and the daemon's
 * asset-serving routes stay green while the new frontend is built. */
(function () {
  var root = document.getElementById("root");
  if (!root) return;
  root.innerHTML =
    '<main style="font-family:system-ui,-apple-system,sans-serif;max-width:40rem;margin:4rem auto;padding:0 1.5rem;line-height:1.5">' +
    '<h1 style="font-size:1.5rem;margin:0 0 .5rem">tracedecay dashboard</h1>' +
    '<p style="opacity:.7;margin:0">The dashboard UI is being rebuilt. Check back soon.</p>' +
    "</main>";
})();
