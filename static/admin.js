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
      row.querySelectorAll('button[name="toggle_unlikely"]').forEach((b) => {
        b.setAttribute("aria-pressed", String(!!data.unlikely));
        b.textContent = data.unlikely ? "Unlikely" : "Likely";
      });
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

  // Clickable table rows (e.g. the by-tier table on the overview).
  document.querySelectorAll("tr[data-href]").forEach((tr) =>
    tr.addEventListener("click", (e) => { if (!e.target.closest("a, button, input, select")) location.assign(tr.dataset.href); }),
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

  // "Add person" / "Add a plus one": clone a row from its <template>, numbered new0., new1., ...
  document.addEventListener("click", (ev) => {
    const add = ev.target.closest("[data-add]");
    if (add) {
      const tpl = document.getElementById(add.dataset.add);
      const box = add.closest("form").querySelector(".new-people");
      const i = Number(box.dataset.next || 0);
      box.dataset.next = i + 1;
      box.insertAdjacentHTML("beforeend", tpl.innerHTML.replaceAll("__I__", String(i)));
      box.lastElementChild.querySelector("input:not([type=hidden]):not([type=checkbox])")?.focus();
      return;
    }
    const rm = ev.target.closest("[data-remove-row]");
    if (rm) {
      rm.closest(".guest-row").remove();
    }
  });

  // Reorder people: drag the grip (mouse or touch), or focus it and use the arrow keys.
  // Hidden inputs travel with their row, so the form submits people in the new order.
  document.addEventListener("pointerdown", (ev) => {
    const handle = ev.target.closest("[data-drag]");
    if (!handle) return;
    const row = handle.closest(".guest-row");
    const list = row.parentElement;
    ev.preventDefault();
    handle.setPointerCapture(ev.pointerId);
    row.classList.add("dragging");
    const move = (e) => {
      const rows = [...list.children].filter((r) => r !== row && r.classList.contains("guest-row"));
      const before = rows.find((r) => { const b = r.getBoundingClientRect(); return e.clientY < b.top + b.height / 2; });
      if (before) { if (row.nextElementSibling !== before) list.insertBefore(row, before); }
      else if (list.lastElementChild !== row) list.appendChild(row);
    };
    const done = () => {
      row.classList.remove("dragging");
      handle.removeEventListener("pointermove", move);
      handle.removeEventListener("pointerup", done);
      handle.removeEventListener("pointercancel", done);
    };
    handle.addEventListener("pointermove", move);
    handle.addEventListener("pointerup", done);
    handle.addEventListener("pointercancel", done);
  });
  document.addEventListener("keydown", (ev) => {
    const handle = ev.target.closest && ev.target.closest("[data-drag]");
    if (!handle || (ev.key !== "ArrowUp" && ev.key !== "ArrowDown")) return;
    ev.preventDefault();
    const row = handle.closest(".guest-row");
    const sib = ev.key === "ArrowUp" ? row.previousElementSibling : row.nextElementSibling;
    if (!sib || !sib.classList.contains("guest-row")) return;
    if (ev.key === "ArrowUp") sib.before(row); else sib.after(row);
    handle.focus();
  });
})();
