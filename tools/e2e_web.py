#!/usr/bin/env python3
"""End-to-end test of the browser 5250 terminal (`as4tainr tn5250 --web-listen`)
in a real browser (Chromium, through Playwright).

Expects the sample objects in QGPL (scripts/samples.sh). QSECOFR's password
is either Green5creen! (what tools/e2e_5250.py leaves) or the initial,
expired QSECOFR, which this test changes to Green5creen!.

    pip install playwright && python3 -m playwright install chromium
    python3 tools/e2e_web.py http://127.0.0.1:5250
"""
import json
import socket
import sys
import time
import urllib.request

from playwright.sync_api import TimeoutError as PlaywrightTimeout, sync_playwright

URL = (sys.argv[1] if len(sys.argv) > 1 else "http://127.0.0.1:5250").rstrip("/")
PW = "Green5creen!"
LIB = f"WEB{int(time.time()) % 100000:05d}"  # new each run
passed = 0


class Term:
    def __init__(self, page):
        self.page = page

    def text(self):
        return self.page.evaluate(
            "() => [...document.querySelectorAll('#screen .row')].map(r => r.textContent.replace(/\\s+$/, '')).join('\\n')"
        )

    def line(self, n):
        lines = self.text().split("\n")
        return lines[n - 1] if n <= len(lines) else ""

    def wait_for(self, s, timeout=20):
        try:
            self.page.wait_for_function(
                "s => [...document.querySelectorAll('#screen .row')].some(r => r.textContent.includes(s))",
                arg=s,
                timeout=timeout * 1000,
            )
        except PlaywrightTimeout:
            check(False, f"waiting for {s!r}", self)

    def ready(self):
        """Wait until the host has unlocked the keyboard."""
        self.page.wait_for_function("() => document.getElementById('oia-lock').textContent === ''", timeout=90_000)

    def type(self, s):
        self.ready()
        self.page.keyboard.type(s)

    def key(self, k):
        self.ready()
        self.page.keyboard.press(k)

    def click_key(self, label):
        """Click a key in the function key bar by its aid (F3, ENTER, ROLLUP...)."""
        self.ready()
        self.page.click(f'.keyboard .key[data-aid="{label}"]')

    def command(self, cmd, expect, timeout=20):
        self.type(cmd)
        self.key("Enter")
        self.wait_for(expect, timeout)


def check(cond, what, t=None):
    global passed
    if not cond:
        print(f"FAIL: {what}")
        if t:
            print(f"--- screen ---\n{t.text()}\n--------------")
        sys.exit(1)
    passed += 1
    print(f"ok   {what}")


def sign_on(t):
    t.wait_for("Sign On")
    t.type("qsecofr"); t.key("Tab"); t.type(PW); t.key("Enter")
    t.ready()  # the screen after this sign-on, not the one before it
    if "Main Menu" not in t.text():
        # Fresh database: the initial password, which has expired.
        t.type("qsecofr"); t.key("Tab"); t.type("QSECOFR"); t.key("Enter")
        t.wait_for("Change Password")
        t.type("QSECOFR"); t.key("Tab"); t.type(PW); t.key("Tab"); t.type(PW); t.key("Enter")
        t.wait_for("Password changed")
        t.type("qsecofr"); t.key("Tab"); t.type(PW); t.key("Enter")
    t.wait_for("Main Menu")  # the initial menu
    t.command("call qcmd", "Command Entry")


def fkey_label(page, aid):
    return page.evaluate(
        "a => { const b = document.querySelector(`.keyboard .key[data-aid=\"${a}\"]`);"
        " return {label: b.querySelector('.lbl').textContent, active: b.classList.contains('active')}; }",
        aid,
    )


# --- HTTP and WebSocket checks without a browser -------------------------
with urllib.request.urlopen(URL + "/") as r:
    csp = r.headers.get("Content-Security-Policy", "")
    check(r.status == 200 and "default-src 'self'" in csp and "frame-ancestors 'none'" in csp,
          "the page is served with a content security policy")


def ws_upgrade(origin):
    host = URL.split("://", 1)[1]
    h, p = host.rsplit(":", 1)
    s = socket.create_connection((h, int(p)), timeout=5)
    req = (f"GET /ws HTTP/1.1\r\nHost: {host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n"
           f"Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nOrigin: {origin}\r\n\r\n")
    s.sendall(req.encode())
    status = s.recv(4096).split(b"\r\n", 1)[0].decode()
    s.close()
    return status


check(" 403 " in ws_upgrade("https://evil.example"), "a WebSocket from another site is refused")
status = ws_upgrade(URL)
check(" 101 " in status, "a WebSocket from the page's own origin is accepted")

# --- The terminal in a real browser ---------------------------------------
with sync_playwright() as pw:
    browser = pw.chromium.launch()
    page = browser.new_page(viewport={"width": 1280, "height": 900})
    page.goto(URL + "/")
    t = Term(page)

    rows = page.evaluate(
        "() => ['row-shift', 'row-base'].map(id => [...document.querySelectorAll('#' + id + ' .key')].map(b => b.dataset.aid))"
    )
    check(rows == [[f"F{n}" for n in range(13, 25)], [f"F{n}" for n in range(1, 13)]],
          "the key bar has F13-F24 above F1-F12, like a 122-key 5250 keyboard")

    t.wait_for("Sign On")
    check("AS4TAINR" in t.line(2) and "QPADEV" in t.line(4), "the browser shows the sign-on screen", t)
    check(page.locator("#conn").get_attribute("data-state") == "open", "the connection indicator says connected")

    t.type("qsecofr"); t.key("Tab"); t.type("not-it"); t.key("Enter")
    t.wait_for("CPF1107")
    check("not-it" not in t.text(), "the password field is not displayed (nondisplay attribute)", t)

    sign_on(t)
    # Command Entry shows the request level, not the user (IBM i 7.5, PUB400).
    check("Request level:" in t.line(2), "signed on from the browser: command entry is shown", t)
    labels = {k: fkey_label(page, k) for k in ("F3", "F4", "F9", "F12", "F5")}
    check(labels["F3"] == {"label": "Exit", "active": True} and labels["F4"]["label"] == "Prompt"
          and labels["F9"]["label"] == "Retrieve" and labels["F12"]["label"] == "Cancel",
          "keys named on the screen show their function on the key bar", t)
    check(labels["F5"] == {"label": "", "active": False}, "keys the screen does not name are dimmed", t)

    t.command(f"crtlib {LIB.lower()} text('Made in a browser')", f"Library {LIB} was created.")
    check(f"Library {LIB} was created." in t.text() and t.line(24).strip() == "",
          "CRTLIB from the browser: its message in the Command Entry log", t)

    t.command(f"dsplib {LIB}", "Display Library")
    check(LIB in t.line(3) and "Made in a browser" in t.text(), "DSPLIB shows the new library and its text", t)
    t.click_key("F3")
    t.wait_for("Command Entry")
    check(True, "clicking F3 on the key bar leaves DSPLIB", t)

    t.type("crtlib")
    t.key("F4")
    t.wait_for("Create Library (CRTLIB)")
    check(True, "the keyboard's F4 prompts the command (the browser does not take it)", t)
    t.key("F12")
    t.wait_for("Command Entry")
    check(True, "the keyboard's F12 cancels the prompt (not the browser's developer tools)", t)

    t.command("dsplibl", "Display Library List")
    t.ready()
    page.click("#screen .hot >> text=F3=Exit")
    t.wait_for("Command Entry")
    check(True, "clicking F3=Exit on the screen presses F3", t)

    # Interactive RPG with Field Exit from the key bar.
    t.command("call custinq", "Customer Inquiry", timeout=90)
    t.type("5")
    t.ready()
    page.click('.keyboard .key[data-local="fieldexit"]')
    t.click_key("ENTER")
    t.wait_for("Lake, Pine & Co", timeout=30)
    check("Duluth" in t.line(7) and "75.25-" in t.line(8), "Field Exit on the key bar, then Enter: CHAIN found customer 5", t)
    t.click_key("F12")
    t.wait_for("Type a customer number")
    t.click_key("F3")
    t.wait_for("Command Entry")
    check("ended abnormally" not in t.text(), "F3 ends CUSTINQ normally", t)

    # Subfile paging with the PgDn / PgUp keys of the bar.
    t.command("call wrkcust", "Work with Customers", timeout=90)
    check("More..." in t.line(11), "WRKCUST shows the first page", t)
    t.click_key("ROLLUP")
    t.wait_for("Lake, Pine")
    check("Bottom" in t.line(11), "PgDn on the key bar shows the next page", t)
    t.key("PageUp")
    t.wait_for("Acme Tooling")
    check("Prairie Freight" in t.line(10), "the keyboard's Page Up returns to the first page", t)
    t.key("Tab"); t.key("Tab"); t.type("5"); t.key("Enter")
    t.wait_for("Display Customer")
    check("Bluebird Supply" in t.line(6), "option 5 typed on the third subfile line selects it (READC)", t)
    t.key("F12")
    t.wait_for("Work with Customers")
    t.key("F3")
    t.wait_for("Command Entry")

    # Order entry: numeric input fields with the keypad's Enter (Field Exit),
    # which right-adjusts the number, as on a 5250 keyboard.
    t.command("call ordentc", "Order Entry", timeout=90)
    t.type("2"); t.key("NumpadEnter"); t.key("Enter")
    t.wait_for("Midwest Fasteners", timeout=30)
    check("Chicago" in t.line(4), "keypad Enter is Field Exit: the customer number is accepted", t)
    t.type("bolt-m8"); t.key("Tab"); t.type("40"); t.key("NumpadEnter"); t.key("Enter")
    t.wait_for("Hex bolt M8 x 30", timeout=30)
    check("6.00" in t.line(9) and "6.00" in t.line(21), "a typed order line is priced from ITEMMAST", t)
    t.key("F3")
    t.wait_for("next order number", timeout=30)
    check(True, "F3 leaves order entry without creating the order", t)

    # A second tab is a second display device and job.
    page2 = browser.new_page()
    page2.goto(URL + "/")
    t2 = Term(page2)
    t2.wait_for("Sign On")
    check(t2.line(4).split()[-1].startswith("QPADEV"), "a second browser tab gets its own display device", t2)
    sign_on(t2)
    t.command("wrkactjob", "Work with Active Jobs")
    jobs = [l for l in t.text().split("\n") if "QPADEV" in l and "QSECOFR" in l and " INT " in l]
    check(len(jobs) == 2, "WRKACTJOB shows both browser sessions as interactive jobs", t)
    t.key("F3")
    t.wait_for("Command Entry")

    # Signing off ends the session; the page offers to connect again.
    t2.command("signoff", "Sign On")
    check(True, "SIGNOFF returns the second tab to the sign-on screen", t2)
    page2.close()

    page.set_viewport_size({"width": 390, "height": 844})
    page.wait_for_timeout(200)
    overflow = page.evaluate("() => document.documentElement.scrollWidth - document.documentElement.clientWidth")
    check(overflow <= 0, "at phone width the page does not scroll sideways", t)
    browser.close()

print(f"\nweb terminal end to end: {passed} checks passed")
