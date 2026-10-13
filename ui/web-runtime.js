(function () {
  if (window.__TAURI__) return;

  window.__JR_RECEIVER_WEB__ = true;
  window.__TAURI__ = {
    core: {
      async invoke(command, args = {}) {
        const response = await fetch(`/api/invoke/${encodeURIComponent(command)}`, {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify(args),
        });
        const body = await response.json().catch(() => ({}));
        if (!response.ok) throw new Error(body.error || `HTTP ${response.status}`);
        return body;
      },
    },
  };
})();
