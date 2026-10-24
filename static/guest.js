// Guest RSVP form: only show meal choices once someone accepts. Works without JS too.
(() => {
  "use strict";
  const blocks = document.querySelectorAll("[data-show-when]");
  function sync() {
    blocks.forEach((el) => {
      const name = el.dataset.showWhen;
      const yes = document.querySelector(`input[name="${CSS.escape(name)}"][value="yes"]`);
      el.hidden = !(yes && yes.checked);
    });
  }
  document.addEventListener("change", (ev) => {
    if (ev.target.matches("input[type=radio]")) sync();
  });
  sync();

  // Address search: type into one box, pick a suggestion. Suggestions come from our own
  // server (which asks Google), so no third-party scripts run on this page.
  document.querySelectorAll("form.address-form").forEach((form) => {
    const box = form.querySelector(".address-search");
    if (!box || !window.fetch) return;
    const base = form.getAttribute("action").split("#")[0];
    const input = box.querySelector(".addr-search");
    const list = box.querySelector(".addr-results");
    const status = box.querySelector(".addr-status");
    const picked = box.querySelector(".addr-picked");
    const pickedText = picked.querySelector("address");
    const manual = form.querySelector(".manual-fields");
    const unit = form.querySelector(".unit-field");
    const pickedFlag = form.querySelector('input[name="picked"]');
    const f = (n) => form.querySelector(`[name="${n}"]`);
    const newSession = () => Array.from(crypto.getRandomValues(new Uint8Array(16)), (b) => b.toString(16).padStart(2, "0")).join("");
    let session = newSession(), timer = null, items = [], active = -1, seq = 0, searchWorks = true;
    const backWrap = form.querySelector(".addr-back-wrap");

    function mode(m) {
      box.hidden = m === "manual";
      manual.hidden = m !== "manual";
      unit.hidden = m === "search";
      input.parentElement.hidden = m === "picked";
      picked.hidden = m !== "picked";
      box.querySelector(".addr-manual").parentElement.hidden = m === "picked";
      backWrap.hidden = !(m === "manual" && searchWorks);
      if (m !== "picked") pickedFlag.value = "";
      if (m === "search") { input.value = ""; input.focus(); }
    }
    function currentLines() {
      const loc = [f("city").value, f("region").value, f("postcode").value].map((v) => v.trim()).filter(Boolean).join(" ");
      const country = f("country").value === "other" ? f("country_other").value : f("country").value;
      return [f("addr1").value.trim(), loc, country].filter(Boolean);
    }
    function showPicked(lines) {
      pickedText.replaceChildren(...lines.flatMap((l, i) => (i ? [document.createElement("br"), l] : [l])));
      mode("picked");
    }
    function close() { list.hidden = true; list.replaceChildren(); input.setAttribute("aria-expanded", "false"); active = -1; }
    function render() {
      list.replaceChildren(...items.map((it, i) => {
        const li = document.createElement("li");
        li.setAttribute("role", "option");
        li.id = `addr-opt-${i}`;
        li.className = i === active ? "active" : "";
        const main = document.createElement("strong"); main.textContent = it.main;
        const sec = document.createElement("span"); sec.textContent = it.secondary;
        li.append(main, sec);
        li.addEventListener("mousedown", (e) => { e.preventDefault(); choose(i); });
        return li;
      }));
      list.hidden = items.length === 0;
      input.setAttribute("aria-expanded", String(items.length > 0));
      if (active >= 0) input.setAttribute("aria-activedescendant", `addr-opt-${active}`); else input.removeAttribute("aria-activedescendant");
    }
    // Rank Google's suggestions by how well they match what was typed: every typed word should
    // prefix-match a word in the suggestion, and a typed house number must appear exactly.
    function rank(q, list) {
      const words = (t) => t.toLowerCase().split(/[^a-z0-9]+/).filter(Boolean);
      const typed = words(q);
      const num = typed.find((w) => /^\d/.test(w));
      const scored = list.map((it, i) => {
        const have = words(`${it.main} ${it.secondary}`);
        const hits = typed.filter((t) => have.some((h) => h.startsWith(t))).length;
        const numOk = !num || have.includes(num);
        return { it, i, score: hits + (numOk ? 100 : 0) };
      });
      const best = Math.max(...scored.map((x) => x.score));
      return scored
        .filter((x) => x.score >= best - 1) // drop clearly worse matches
        .sort((a, b) => b.score - a.score || a.i - b.i)
        .map((x) => x.it);
    }
    async function search(q) {
      const my = ++seq;
      try {
        const r = await fetch(`${base}/suggest?q=${encodeURIComponent(q)}&s=${session}`, { credentials: "same-origin" });
        const d = await r.json();
        // Ignore anything that isn't for exactly what's in the box right now.
        if (my !== seq || q !== input.value.trim()) return;
        if (d.available === false) { searchWorks = false; mode("manual"); return; }
        items = rank(q, d.suggestions || []); active = items.length ? 0 : -1;
        status.textContent = items.length ? "" : "No matches yet. Keep typing, or enter it manually.";
        render();
      } catch (_) { if (my === seq) status.textContent = "Search isn't working. You can enter it manually."; }
    }
    async function choose(i) {
      const it = items[i]; if (!it) return;
      close(); status.textContent = "Looking that up…";
      try {
        const r = await fetch(`${base}/place?id=${encodeURIComponent(it.id)}&s=${session}`, { credentials: "same-origin" });
        const d = await r.json();
        if (!r.ok) throw new Error(d.error);
        const a = d.address;
        f("addr1").value = a.addr1; f("city").value = a.city; f("region").value = a.region; f("postcode").value = a.postcode;
        if (a.addr2) f("addr2").value = a.addr2;
        const sel = f("country");
        if ([...sel.options].some((o) => o.value === a.country)) { sel.value = a.country; } else { sel.value = "other"; f("country_other").value = a.country; }
        sel.dispatchEvent(new Event("change"));
        pickedFlag.value = "1";
        status.textContent = "";
        session = newSession(); // a session ends with the lookup
        showPicked(d.lines);
        f("addr2").focus();
      } catch (_) {
        status.textContent = "Couldn't look that one up. Please enter it manually.";
        mode("manual");
      }
    }
    input.addEventListener("input", () => {
      clearTimeout(timer);
      seq++; // invalidate any request still in flight for older text
      const q = input.value.trim();
      if (q.length < 3) { close(); status.textContent = ""; return; }
      timer = setTimeout(() => search(q), 250);
    });
    input.addEventListener("keydown", (e) => {
      if (e.key === "ArrowDown" && items.length) { active = (active + 1) % items.length; render(); e.preventDefault(); }
      else if (e.key === "ArrowUp" && items.length) { active = (active - 1 + items.length) % items.length; render(); e.preventDefault(); }
      else if (e.key === "Enter") { e.preventDefault(); if (active >= 0) choose(active); }
      else if (e.key === "Escape") close();
    });
    input.addEventListener("blur", () => setTimeout(close, 150));
    box.querySelector(".addr-change").addEventListener("click", () => mode("search"));
    box.querySelector(".addr-manual").addEventListener("click", () => { mode("manual"); f("addr1").focus(); });
    form.querySelector(".addr-back").addEventListener("click", () => { status.textContent = ""; mode("search"); });
    form.addEventListener("submit", (e) => {
      if (!manual.hidden) return;
      if (!f("addr1").value.trim()) { e.preventDefault(); status.textContent = "Pick your address from the list, or enter it manually."; input.focus(); }
    });

    const hadError = !!form.closest("section")?.querySelector(".notice.error");
    if (hadError) mode("manual");
    else if (f("addr1").value.trim()) showPicked(currentLines());
    else { mode("search"); input.blur(); }
  });

  // Address form: free-text country when "Somewhere else" is picked.
  document.querySelectorAll("form.address-form").forEach((f) => {
    const sel = f.querySelector('select[name="country"]');
    const other = f.querySelector(".country-other");
    if (!sel || !other) return;
    const syncCountry = () => { other.hidden = sel.value !== "other"; };
    sel.addEventListener("change", syncCountry);
    syncCountry();
  });

  // Focus the first error after a failed submit.
  const firstError = document.querySelector(".field-error");
  if (firstError) firstError.closest("fieldset, .event-answer")?.scrollIntoView({ block: "center" });
})();
