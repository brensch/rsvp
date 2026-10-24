// Small progressive enhancements for the admin UI.
(() => {
  "use strict";

  // Confirm destructive actions: <button data-confirm="Are you sure?">
  document.addEventListener("click", (ev) => {
    const el = ev.target.closest("[data-confirm]");
    if (el && !confirm(el.dataset.confirm)) {
      ev.preventDefault();
      ev.stopPropagation();
    }
  }, true);

  // Copy buttons: <button data-copy="#input-id">
  document.addEventListener("click", async (ev) => {
    const btn = ev.target.closest("[data-copy]");
    if (!btn) return;
    const input = document.querySelector(btn.dataset.copy);
    if (!input) return;
    try {
      await navigator.clipboard.writeText(input.value);
    } catch (_) {
      input.select();
      document.execCommand("copy");
    }
    const label = btn.textContent;
    btn.textContent = "Copied";
    setTimeout(() => (btn.textContent = label), 1500);
  });

  // Inline-save forms (tier switch, event chips): POST in the background, update the buttons.
  document.addEventListener("submit", async (ev) => {
    const form = ev.target.closest("form[data-inline]");
    if (!form || !ev.submitter) return;
    ev.preventDefault();
    const body = new URLSearchParams(new FormData(form));
    body.set(ev.submitter.name, ev.submitter.value);
    form.classList.add("saving");
    try {
      const res = await fetch(form.action, { method: "POST", body, headers: { "x-inline": "1" }, credentials: "same-origin" });
      const data = await res.json();
      if (!res.ok) throw new Error(data.error || "save failed");
      const row = form.closest("tr") || document;
      row.querySelectorAll('button[name="tier"]').forEach((b) => b.setAttribute("aria-pressed", String(Number(b.value) === data.tier)));
      row.querySelectorAll('button[name="toggle_event"]').forEach((b) => b.setAttribute("aria-pressed", String(data.events.includes(b.value))));
      row.querySelectorAll('button[name="toggle_australia"]').forEach((b) => b.setAttribute("aria-pressed", String(!!data.australia)));
      row.querySelectorAll('button[name="toggle_checked"]').forEach((b) => b.setAttribute("aria-pressed", String(!!data.checked)));
      if ("checked_by" in data) row.querySelectorAll(".checked-by").forEach((el) => { el.textContent = data.checked_by; });
      form.classList.add("saved");
      setTimeout(() => form.classList.remove("saved"), 700);
    } catch (e) {
      toast("Couldn't save. Reload and try again.", true);
    } finally {
      form.classList.remove("saving");
    }
  });

  function toast(msg, bad) {
    const t = document.createElement("div");
    t.className = "toast" + (bad ? " bad" : "");
    t.textContent = msg;
    document.body.appendChild(t);
    setTimeout(() => t.remove(), 2600);
  }

  // Filter bars apply as soon as a select changes (search applies on Enter).
  document.querySelectorAll("form.autosubmit select").forEach((sel) =>
    sel.addEventListener("change", () => sel.form.requestSubmit()),
  );

  // Close the mobile menu after navigating.
  document.querySelectorAll(".sidebar nav a").forEach((a) =>
    a.addEventListener("click", () => {
      const t = document.getElementById("nav-toggle");
      if (t) t.checked = false;
    }),
  );

  // Strip one-shot query params (flash messages, invite codes) from the address bar.
  const url = new URL(location.href);
  let changed = false;
  for (const k of ["m", "code", "hours"]) {
    if (url.searchParams.has(k)) { url.searchParams.delete(k); changed = true; }
  }
  if (changed) history.replaceState(null, "", url.pathname + (url.search || "") + url.hash);
})();
