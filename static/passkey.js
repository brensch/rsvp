// Passkey ceremonies for Wedding HQ. No dependencies.
(() => {
  "use strict";

  const b64u = {
    toBuf(s) {
      const pad = "=".repeat((4 - (s.length % 4)) % 4);
      const bin = atob((s + pad).replace(/-/g, "+").replace(/_/g, "/"));
      return Uint8Array.from(bin, (c) => c.charCodeAt(0)).buffer;
    },
    fromBuf(buf) {
      let bin = "";
      for (const b of new Uint8Array(buf)) bin += String.fromCharCode(b);
      return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
    },
  };

  const errEl = () => document.getElementById("auth-error");
  function showError(msg) {
    const el = errEl();
    if (el) { el.textContent = msg; el.hidden = false; } else { alert(msg); }
  }
  function clearError() { const el = errEl(); if (el) el.hidden = true; }

  async function post(url, body, extraHeaders = {}) {
    const res = await fetch(url, {
      method: "POST",
      credentials: "same-origin",
      headers: { "content-type": "application/json", ...extraHeaders },
      body: JSON.stringify(body ?? {}),
    });
    let data = {};
    try { data = await res.json(); } catch (_) { /* non-JSON error */ }
    if (!res.ok) throw new Error(data.error || `Request failed (${res.status})`);
    return data;
  }

  function creationOptions(opts) {
    const pk = opts.publicKey;
    pk.challenge = b64u.toBuf(pk.challenge);
    pk.user.id = b64u.toBuf(pk.user.id);
    (pk.excludeCredentials || []).forEach((c) => (c.id = b64u.toBuf(c.id)));
    // Discoverable credential so sign-in is a single tap, no username.
    pk.authenticatorSelection = { ...(pk.authenticatorSelection || {}), residentKey: "required", requireResidentKey: true, userVerification: "required" };
    return { publicKey: pk };
  }

  function requestOptions(opts) {
    const pk = opts.publicKey;
    pk.challenge = b64u.toBuf(pk.challenge);
    (pk.allowCredentials || []).forEach((c) => (c.id = b64u.toBuf(c.id)));
    return { publicKey: pk };
  }

  function encodeRegistration(cred) {
    const r = cred.response;
    return {
      id: cred.id,
      rawId: b64u.fromBuf(cred.rawId),
      type: cred.type,
      response: {
        attestationObject: b64u.fromBuf(r.attestationObject),
        clientDataJSON: b64u.fromBuf(r.clientDataJSON),
        transports: typeof r.getTransports === "function" ? r.getTransports() : undefined,
      },
      extensions: cred.getClientExtensionResults ? cred.getClientExtensionResults() : {},
    };
  }

  function encodeAssertion(cred) {
    const r = cred.response;
    return {
      id: cred.id,
      rawId: b64u.fromBuf(cred.rawId),
      type: cred.type,
      response: {
        authenticatorData: b64u.fromBuf(r.authenticatorData),
        clientDataJSON: b64u.fromBuf(r.clientDataJSON),
        signature: b64u.fromBuf(r.signature),
        userHandle: r.userHandle ? b64u.fromBuf(r.userHandle) : null,
      },
      extensions: cred.getClientExtensionResults ? cred.getClientExtensionResults() : {},
    };
  }

  function friendly(e) {
    if (e && e.name === "NotAllowedError") return "Cancelled, or no passkey for this site on this device.";
    if (e && e.name === "InvalidStateError") return "This device already has a passkey for your account.";
    return (e && e.message) || "Something went wrong.";
  }

  function busy(btn, on) {
    if (!btn) return;
    btn.disabled = on;
    btn.dataset.label = btn.dataset.label || btn.textContent;
    btn.textContent = on ? "Waiting for your device…" : btn.dataset.label;
  }

  if (!window.PublicKeyCredential) {
    document.addEventListener("DOMContentLoaded", () => showError("This browser doesn't support passkeys. Try an up-to-date Safari, Chrome, Edge or Firefox."));
  }

  // Sign in
  const loginBtn = document.getElementById("passkey-login");
  if (loginBtn) {
    loginBtn.addEventListener("click", async () => {
      clearError(); busy(loginBtn, true);
      try {
        const opts = await post("/admin/api/login/start");
        const cred = await navigator.credentials.get(requestOptions(opts));
        const done = await post("/admin/api/login/finish", encodeAssertion(cred));
        location.assign(done.redirect || "/admin");
      } catch (e) { showError(friendly(e)); busy(loginBtn, false); }
    });
  }

  // Register
  const regForm = document.getElementById("register-form");
  if (regForm) {
    regForm.addEventListener("submit", async (ev) => {
      ev.preventDefault(); clearError();
      const btn = regForm.querySelector("button[type=submit]");
      busy(btn, true);
      try {
        const name = document.getElementById("reg-name").value.trim();
        const inviteEl = document.getElementById("reg-invite");
        const opts = await post("/admin/api/register/start", { name, invite: inviteEl ? inviteEl.value : null });
        const cred = await navigator.credentials.create(creationOptions(opts));
        const done = await post("/admin/api/register/finish", encodeRegistration(cred));
        location.assign(done.redirect || "/admin");
      } catch (e) { showError(friendly(e)); busy(btn, false); }
    });
  }

  // Add another passkey (account page)
  const addBtn = document.getElementById("passkey-add");
  if (addBtn) {
    const csrf = document.querySelector('meta[name="csrf"]')?.content || "";
    addBtn.addEventListener("click", async () => {
      clearError(); busy(addBtn, true);
      try {
        const opts = await post("/admin/api/passkey/start", {}, { "x-csrf": csrf });
        const cred = await navigator.credentials.create(creationOptions(opts));
        const done = await post("/admin/api/passkey/finish", encodeRegistration(cred), { "x-csrf": csrf });
        location.assign(done.redirect || "/admin/account");
      } catch (e) { showError(friendly(e)); busy(addBtn, false); }
    });
  }
})();
