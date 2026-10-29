// as4tainr browser 5250 terminal.
// The server keeps the 5250 data stream; this page draws the screen, edits
// input fields like a display station, and sends AID keys with the changed
// fields. See core/src/web/mod.rs for the message format.
"use strict";

(() => {
  const $ = (id) => document.getElementById(id);
  const screenEl = $("screen");
  const kbd = $("kbd");
  // A touch screen: tapping a key of the bar should not open the phone's
  // keyboard unless it was already open.
  const coarse = !!(window.matchMedia && window.matchMedia("(pointer: coarse)").matches);
  const keyboardEl = $("row-base").closest(".keyboard");

  // ---- State ----------------------------------------------------------
  let ws = null;
  let rows = 24, cols = 80;
  let chars = [];        // one character per position
  let cellAttr = [];     // display attribute in effect per position
  let attrPos = new Set();
  let fields = [];       // {p, l, ffw:[b1,b2]|null, a, mdt}
  let cursor = 0;
  let locked = true;
  let insert = false;
  let opError = "";
  let lockText = "";
  let hotspots = [];     // {from, to, aid}
  let fkeyLabels = {};   // "F3" -> "Exit"

  // ---- Function key bar ----------------------------------------------
  // Like a 122-key 5250 keyboard: F13-F24 above F1-F12, in groups of four.
  const fkeyButtons = {};
  function buildRow(el, first) {
    for (let g = 0; g < 3; g++) {
      if (g > 0) {
        const gap = document.createElement("span");
        gap.className = "gap";
        el.appendChild(gap);
      }
      for (let k = 0; k < 4; k++) {
        const n = first + g * 4 + k;
        const b = document.createElement("button");
        b.type = "button";
        b.className = "key fkey";
        b.dataset.aid = "F" + n;
        b.innerHTML = '<span class="cap">F' + n + '</span><span class="lbl"></span>';
        el.appendChild(b);
        fkeyButtons["F" + n] = b;
      }
    }
  }
  buildRow($("row-shift"), 13);
  buildRow($("row-base"), 1);

  document.querySelectorAll(".keyboard .key").forEach((b) => {
    // Keep keyboard focus on the input field when a key is clicked.
    let hadKeys = false;
    b.addEventListener("pointerdown", () => { hadKeys = document.activeElement === kbd; });
    b.addEventListener("mousedown", (e) => e.preventDefault());
    b.addEventListener("click", () => {
      if (b.dataset.aid) sendAid(b.dataset.aid);
      else if (b.dataset.local === "reset") reset();
      else if (b.dataset.local === "fieldexit") fieldExit("");
      else if (b.dataset.local === "tab") tab(1);
      else if (b.dataset.local === "backtab") tab(-1);
      if (!coarse || hadKeys) focusKeys();
    });
  });

  function flashKey(aid) {
    const b = document.querySelector('.keyboard .key[data-aid="' + aid + '"]');
    if (!b) return;
    b.classList.add("pressed");
    setTimeout(() => b.classList.remove("pressed"), 120);
  }

  // ---- Attributes -----------------------------------------------------
  // Screen attribute byte 0x20-0x3F on a color display.
  function attrClasses(a) {
    if ((a & 0x07) === 0x07) return null; // nondisplay
    const group = (a >> 3) & 0x03;
    const alt = (a & 0x02) !== 0;
    const color = [alt ? "white" : "green", "red", alt ? "yellow" : "turquoise", alt ? "blue" : "pink"][group];
    let cls = "a-" + color;
    if (a & 0x01) cls += " a-rev";
    if (a & 0x04) cls += " a-ul";
    return cls;
  }

  // ---- Fields ---------------------------------------------------------
  const isInput = (f) => f.ffw && (f.ffw[0] & 0x20) === 0;
  const inputFields = () => fields.filter(isInput);
  function fieldAt(p) {
    for (const f of fields) if (p >= f.p && p < f.p + f.l) return f;
    return null;
  }
  const shift = (f) => f.ffw[0] & 0x07;
  const monocase = (f) => (f.ffw[1] & 0x20) !== 0;
  const autoEnter = (f) => (f.ffw[1] & 0x80) !== 0;
  const isNumeric = (f) => [3, 5, 7].includes(shift(f));

  function allowed(f, ch) {
    switch (shift(f)) {
      case 3: return /[0-9+\-., ]/.test(ch);      // numeric only
      case 5: return /[0-9]/.test(ch);            // digits only
      case 7: return /[0-9+\-., ]/.test(ch);      // signed numeric
      default: return ch >= " ";
    }
  }
  function fieldText(f) { return chars.slice(f.p, f.p + f.l).join(""); }
  function setFieldText(f, s) {
    for (let i = 0; i < f.l; i++) chars[f.p + i] = i < s.length ? s[i] : " ";
    f.mdt = true;
  }

  function nextField(from, dir) {
    const list = inputFields();
    if (!list.length) return null;
    if (dir > 0) return list.find((f) => f.p > from) || list[0];
    const cur = fieldAt(from);
    if (cur && isInput(cur) && from > cur.p) return cur;
    const before = list.filter((f) => f.p < (cur ? cur.p : from));
    return before.length ? before[before.length - 1] : list[list.length - 1];
  }

  // ---- Drawing --------------------------------------------------------
  function findHotspots() {
    hotspots = [];
    fkeyLabels = {};
    for (let r = 0; r < rows; r++) {
      const line = chars.slice(r * cols, (r + 1) * cols).join("");
      const re = /\b(F(\d{1,2}))=(\S+(?: \S+)*?)(?=\s{2,}|\s+F\d{1,2}=|\s*$)/g;
      let m;
      while ((m = re.exec(line))) {
        const n = Number(m[2]);
        if (n < 1 || n > 24) continue;
        const from = r * cols + m.index;
        // Only text that is displayed and outside input fields.
        const f = fieldAt(from);
        if (f && isInput(f)) continue;
        if (attrClasses(cellAttr[from] ?? 0x20) === null) continue;
        hotspots.push({ from, to: from + m[0].length, aid: m[1] });
        if (!fkeyLabels[m[1]]) fkeyLabels[m[1]] = m[3];
      }
    }
    // "More..." at the bottom of a list: Page Down shows the next page.
    const more = chars.join("").includes("More...");
    document.querySelector('.keyboard .key[data-aid="ROLLUP"]').classList.toggle("active", more);
    for (const [k, b] of Object.entries(fkeyButtons)) {
      const lbl = fkeyLabels[k] || "";
      b.querySelector(".lbl").textContent = lbl;
      b.classList.toggle("active", !!lbl);
      b.title = lbl ? k + "=" + lbl : k;
    }
  }

  const esc = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

  function render() {
    const inField = new Uint8Array(rows * cols);
    for (const f of fields) if (isInput(f)) for (let i = 0; i < f.l; i++) inField[f.p + i] = 1;
    const hot = new Int16Array(rows * cols).fill(-1);
    hotspots.forEach((h, i) => { for (let p = h.from; p < h.to; p++) hot[p] = i; });

    let html = "";
    for (let r = 0; r < rows; r++) {
      html += '<div class="row">';
      let run = "", runCls = null, runHot = -1;
      const flush = () => {
        if (!run) return;
        const attrs = (runCls ? ' class="' + runCls + '"' : "") + (runHot >= 0 ? ' data-hot="' + runHot + '"' : "");
        html += "<span" + attrs + ">" + esc(run) + "</span>";
        run = "";
      };
      for (let c = 0; c < cols; c++) {
        const p = r * cols + c;
        const a = cellAttr[p];
        const base = attrClasses(a);
        let ch = chars[p] || " ";
        let cls;
        if (attrPos.has(p)) { ch = " "; cls = ""; }
        else if (base === null) { ch = " "; cls = ""; }
        else cls = base;
        if (inField[p]) cls += " a-field";
        if (hot[p] >= 0) cls += " hot";
        if (p === cursor) cls += " cur";
        cls = cls.trim();
        if (cls !== runCls || hot[p] !== runHot || p === cursor || p === cursor + 1) {
          flush();
          runCls = cls; runHot = hot[p];
        }
        run += ch;
      }
      flush();
      html += "</div>";
    }
    screenEl.innerHTML = html;
    screenEl.classList.toggle("locked", locked);
    placeKbd();
    updateOia();
  }

  // ---- Keyboard input field --------------------------------------------
  // Typed characters arrive in two ways. A desktop keyboard (and iOS) sends
  // keydown with the character in e.key: the keydown handler below types it
  // and cancels the event, so nothing reaches the field. Android keyboards
  // and input methods send keydown "Unidentified" (keyCode 229) and put the
  // text into the field, often as a composition they keep changing until
  // the word is done. The field holds PAD (zero-width spaces, so Backspace
  // has something to delete) and what was typed after it; each change is
  // compared with the last one and replayed on the screen as Backspaces
  // and typed characters, then the field is set back to PAD once no
  // composition is open.
  const PAD = "\u200b".repeat(16);
  let kbdValue = PAD;
  let composing = false;
  let skipInput = false; // the keydown handler already did this key

  function focusKeys() {
    if (document.activeElement !== kbd) kbd.focus({ preventScroll: true });
  }
  function resetKbd() {
    kbd.value = PAD;
    kbd.setSelectionRange(PAD.length, PAD.length);
    kbdValue = PAD;
  }
  function placeKbd() {
    const cur = screenEl.querySelector(".cur");
    if (!cur) return;
    const wrap = screenEl.parentElement;
    const r = cur.getBoundingClientRect(), w = wrap.getBoundingClientRect();
    kbd.style.left = Math.round(r.left - w.left + wrap.scrollLeft) + "px";
    kbd.style.top = Math.round(r.top - w.top + wrap.scrollTop) + "px";
  }
  function takeInput() {
    const v = kbd.value;
    if (v === kbdValue) return;
    let i = 0;
    while (i < v.length && i < kbdValue.length && v[i] === kbdValue[i]) i++;
    const removed = kbdValue.length - i;
    const added = v.slice(i);
    kbdValue = v;
    for (let k = 0; k < removed; k++) if (!locked) backspace();
    for (const ch of added) {
      if (ch === "\u200b") continue;
      if (ch === "\n" || ch === "\r") { sendAid("ENTER"); break; }
      if (ch === "\t") { tab(1); continue; }
      typeChar(ch);
    }
  }
  kbd.addEventListener("compositionstart", () => { composing = true; });
  kbd.addEventListener("compositionend", () => {
    composing = false;
    takeInput();
    // Some browsers send the composition's last input event after this one.
    setTimeout(() => { if (!composing) { takeInput(); resetKbd(); } }, 0);
  });
  kbd.addEventListener("input", () => {
    if (skipInput) { resetKbd(); return; }
    takeInput();
    if (!composing) resetKbd();
  });
  kbd.addEventListener("focus", () => { if (!composing) resetKbd(); });
  // Tabbing to the screen gives the focus to the input field.
  screenEl.addEventListener("focus", focusKeys);

  function updateOia() {
    $("oia-lock").textContent = locked ? (lockText || "X SYSTEM") : "";
    $("oia-msg").textContent = opError;
    $("oia-ins").textContent = insert ? "Insert" : "";
    const r = Math.floor(cursor / cols) + 1, c = (cursor % cols) + 1;
    $("oia-pos").textContent = String(r).padStart(2, "0") + "/" + String(c).padStart(3, "0");
  }

  // Fit 80 columns into the window.
  function fit() {
    const wrap = screenEl.parentElement;
    const probe = document.createElement("span");
    probe.style.cssText = "position:absolute;visibility:hidden;font-family:var(--mono);font-size:100px;white-space:pre";
    probe.textContent = "0".repeat(10);
    document.body.appendChild(probe);
    const chPer100 = probe.getBoundingClientRect().width / 10;
    probe.remove();
    const w = chPer100 / 100; // character width per pixel of font size
    // Width: 80 columns plus 0.8 cells of padding on each side and the border.
    const byWidth = (wrap.clientWidth - 2) / (cols * w + 1.6);
    // Height: 24 rows of 1.32 lines plus padding, leaving room for the
    // status line below; never shrink below a readable size for height alone.
    const top = screenEl.getBoundingClientRect().top + window.scrollY;
    const byHeight = (window.innerHeight - top - 64) / (rows * 1.32 + 1.2);
    const size = Math.min(byWidth, Math.max(byHeight, 12));
    const fitted = Math.max(7, Math.min(22, Math.floor(size * 10) / 10));
    document.documentElement.style.setProperty("--cell", fitted + "px");
  }
  window.addEventListener("resize", fit);

  // ---- Server messages -----------------------------------------------
  function onScreen(m) {
    rows = m.rows; cols = m.cols;
    chars = [];
    for (const line of m.lines) for (const ch of line) chars.push(ch);
    attrPos = new Set(m.attrs.map((x) => x[0]));
    cellAttr = new Array(rows * cols);
    let cur = 0x20, k = 0;
    const sorted = m.attrs.slice().sort((a, b) => a[0] - b[0]);
    for (let p = 0; p < rows * cols; p++) {
      while (k < sorted.length && sorted[k][0] === p) { cur = sorted[k][1]; k++; }
      cellAttr[p] = cur;
    }
    fields = m.fields.map((f) => ({ p: f.p, l: f.l, ffw: f.f, a: f.a, mdt: false }));
    cursor = m.cursor;
    locked = m.locked;
    lockText = "";
    opError = m.error || "";
    findHotspots();
    render();
  }

  function setConn(state, text) {
    const el = $("conn");
    el.dataset.state = state;
    el.textContent = text;
  }

  function connect() {
    $("overlay").hidden = true;
    setConn("connecting", "Connecting");
    const proto = location.protocol === "https:" ? "wss:" : "ws:";
    ws = new WebSocket(proto + "//" + location.host + "/ws");
    ws.onopen = () => setConn("open", "Connected");
    ws.onmessage = (ev) => {
      let m;
      try { m = JSON.parse(ev.data); } catch { return; }
      if (m.t === "screen") onScreen(m);
      else if (m.t === "closed") ended(m.reason);
    };
    ws.onclose = () => ended("");
  }

  function ended(reason) {
    if (ws) { ws.onclose = null; try { ws.close(); } catch {} }
    ws = null;
    locked = true;
    lockText = "";
    setConn("closed", "Disconnected");
    $("overlay-text").textContent = reason || "The connection to the server was lost.";
    $("overlay").hidden = false;
    updateOia();
  }
  $("reconnect").addEventListener("click", connect);

  // ---- Keys -----------------------------------------------------------
  function sendAid(aid) {
    flashKey(aid);
    if (!ws || ws.readyState !== WebSocket.OPEN) return;
    if (locked) { lockText = "X SYSTEM"; updateOia(); return; }
    const changed = fields.filter((f) => isInput(f) && f.mdt)
      .map((f) => ({ p: f.p, v: fieldText(f).replace(/\s+$/, "") }));
    ws.send(JSON.stringify({ t: "aid", aid, cursor, fields: changed }));
    locked = true;
    lockText = "X SYSTEM";
    opError = "";
    render();
  }

  function reset() {
    opError = "";
    if (!locked) lockText = "";
    updateOia();
  }

  function operatorError(msg) {
    opError = msg;
    updateOia();
  }

  function moveTo(p) {
    cursor = (p + rows * cols) % (rows * cols);
    render();
  }

  function typeChar(ch) {
    if (locked) { lockText = "X SYSTEM"; updateOia(); return; }
    const f = fieldAt(cursor);
    if (!f || !isInput(f)) return operatorError("Cursor in protected area of display.");
    if (monocase(f)) ch = ch.toUpperCase();
    if (!allowed(f, ch)) return operatorError(isNumeric(f) ? "Field requires numeric characters." : "Character not allowed here.");
    const end = f.p + f.l - 1;
    if (insert) {
      if (chars[end] !== " " && chars[end] !== "\u0000") return operatorError("No room to insert data.");
      for (let p = end; p > cursor; p--) chars[p] = chars[p - 1];
    }
    chars[cursor] = ch;
    f.mdt = true;
    opError = "";
    if (cursor < end) cursor++;
    else if (autoEnter(f)) { render(); return sendAid("ENTER"); }
    else { const n = nextField(cursor, 1); if (n) cursor = n.p; }
    render();
  }

  function deleteChar(at) {
    const f = fieldAt(at);
    if (!f || !isInput(f)) return operatorError("Cursor in protected area of display.");
    const end = f.p + f.l - 1;
    for (let p = at; p < end; p++) chars[p] = chars[p + 1];
    chars[end] = " ";
    f.mdt = true;
    render();
  }

  function backspace() {
    const f = fieldAt(cursor);
    if (!f || !isInput(f)) return moveTo(cursor - 1);
    if (cursor > f.p) { cursor--; deleteChar(cursor); }
    else { const p = nextField(cursor, -1); if (p && p !== f) moveTo(p.p + p.l - 1); }
  }

  // Field Exit: clear the rest of the field, right-adjust a numeric field,
  // go to the next field. Field- also makes a signed numeric value negative.
  function fieldExit(sign) {
    if (locked) return;
    const f = fieldAt(cursor);
    if (!f || !isInput(f)) return operatorError("Cursor in protected area of display.");
    let text = fieldText(f).slice(0, cursor - f.p);
    if (isNumeric(f)) {
      text = text.trim();
      if (sign === "-") {
        if (shift(f) !== 7 && shift(f) !== 3) return operatorError("Field minus is only for numeric fields.");
        text = text.replace(/-$/, "") + "-";
      }
      text = text.padStart(f.l, " ").slice(-f.l);
    }
    setFieldText(f, text);
    if (autoEnter(f)) { render(); return sendAid("ENTER"); }
    const n = nextField(f.p, 1);
    cursor = n ? n.p : cursor;
    render();
  }

  function tab(dir) {
    const n = nextField(cursor, dir);
    if (n) moveTo(n.p);
  }

  function endOfField() {
    const f = fieldAt(cursor);
    if (!f || !isInput(f)) return;
    const t = fieldText(f).replace(/\s+$/, "");
    moveTo(f.p + Math.min(t.length, f.l - 1));
  }

  const keyAids = { PageUp: "ROLLDOWN", PageDown: "ROLLUP" };

  document.addEventListener("keydown", (e) => {
    // An input method's key: its text comes as input (takeInput).
    if (e.isComposing || e.keyCode === 229) return;
    // Let the page's own controls (Connect again) work.
    if (e.target !== kbd && e.target !== screenEl && e.target !== document.body) return;
    if (e.target !== kbd) focusKeys();
    const fk = /^F(\d{1,2})$/.exec(e.key);
    if (fk) {
      const n = Number(fk[1]);
      if (n >= 1 && n <= 12) {
        e.preventDefault();
        sendAid("F" + (e.shiftKey ? n + 12 : n));
        return;
      }
      if (n >= 13 && n <= 24) { e.preventDefault(); sendAid("F" + n); return; }
    }
    if (e.ctrlKey || e.metaKey || e.altKey) return; // leave browser shortcuts alone
    const f = fieldAt(cursor);
    let handled = true;
    switch (e.key) {
      case "Enter":
        if (e.code === "NumpadEnter") fieldExit("");
        else sendAid("ENTER");
        break;
      case "Tab": tab(e.shiftKey ? -1 : 1); break;
      case "PageUp": case "PageDown": sendAid(keyAids[e.key]); break;
      case "ArrowLeft": moveTo(cursor - 1); break;
      case "ArrowRight": moveTo(cursor + 1); break;
      case "ArrowUp": moveTo(cursor - cols); break;
      case "ArrowDown": moveTo(cursor + cols); break;
      case "Home": { const n = inputFields()[0]; moveTo(n ? n.p : 0); break; }
      case "End": endOfField(); break;
      case "Backspace": if (!locked) backspace(); break;
      case "Delete": if (!locked) deleteChar(cursor); break;
      case "Insert": insert = !insert; updateOia(); break;
      case "Escape": reset(); break;
      default:
        if (e.code === "NumpadAdd" && f && isInput(f) && isNumeric(f)) fieldExit("+");
        else if (e.code === "NumpadSubtract" && f && isInput(f) && isNumeric(f)) fieldExit("-");
        else if (e.key.length === 1) typeChar(e.key);
        else handled = false;
    }
    if (handled) {
      e.preventDefault();
      // If a browser puts the key into the field anyway, it is not typed twice.
      skipInput = true;
      setTimeout(() => { skipInput = false; }, 0);
    }
  });

  function paste(e) {
    e.preventDefault();
    const text = (e.clipboardData || window.clipboardData).getData("text");
    for (const ch of text.replace(/\r?\n.*/s, "")) {
      if (locked) break;
      typeChar(ch);
    }
  }
  screenEl.addEventListener("paste", paste);
  kbd.addEventListener("paste", paste);

  document.addEventListener("keydown", (e) => { if (e.key === "Shift") keyboardEl.classList.add("shift"); });
  document.addEventListener("keyup", (e) => { if (e.key === "Shift") keyboardEl.classList.remove("shift"); });
  window.addEventListener("blur", () => keyboardEl.classList.remove("shift"));

  // A tap on the screen must not take the focus from the input field (the
  // phone would hide its keyboard and show it again).
  screenEl.addEventListener("mousedown", (e) => { if (document.activeElement === kbd) e.preventDefault(); });

  // Click moves the cursor; a key hint like "F3=Exit" presses that key.
  // Clicking the screen also gives the input field the focus: on a phone
  // that opens the keyboard (it must happen in the tap's own event).
  screenEl.addEventListener("click", (e) => {
    const span = e.target.closest("span");
    if (span && span.dataset.hot !== undefined) {
      const h = hotspots[Number(span.dataset.hot)];
      if (h) { if (!coarse) focusKeys(); sendAid(h.aid); return; }
    }
    if (composing) { composing = false; resetKbd(); }
    focusKeys();
    const rowEl = e.target.closest(".row");
    if (!rowEl) return;
    const r = Array.prototype.indexOf.call(screenEl.children, rowEl);
    const rect = rowEl.getBoundingClientRect();
    const cw = rect.width / cols; // rows are exactly 80ch wide
    const c = Math.max(0, Math.min(cols - 1, Math.floor((e.clientX - rect.left) / cw)));
    moveTo(r * cols + c);
  });

  // Blank screen until the server draws the sign-on display.
  chars = new Array(rows * cols).fill(" ");
  cellAttr = new Array(rows * cols).fill(0x20);
  fit();
  render();
  connect();
  focusKeys();
})();
