#!/usr/bin/env python3
"""Run tools/e2e_5250.py (or gate3_5250.py) against as4tainr's browser 5250
terminal instead of the tn5250 curses client (not buildable here). The
browser terminal is a 5250 display station of its own (core/src/web), so
screens read the same rows and columns. Each session owns a thread with
its own Playwright (the sync API is bound to one thread; gate3 drives its
clerks from threads). Usage:
  WEB=http://127.0.0.1:5250 python3 tools/e2e_5250_web.py tools/e2e_5250.py 127.0.0.1:2323
"""
import os, sys, time, types, runpy, threading, queue
from playwright.sync_api import sync_playwright

WEB = os.environ.get("WEB", "http://127.0.0.1:5250").rstrip("/")


class _Page:
    """The page's operations, run on the session's own thread."""

    def __init__(self):
        self.pw = sync_playwright().start()
        self.browser = self.pw.chromium.launch()
        self.page = self.browser.new_page(viewport={"width": 1400, "height": 1000})
        self.page.goto(WEB + "/")

    def close(self):
        self.browser.close()
        self.pw.stop()

    def rows(self):
        return self.page.evaluate(
            "() => [...document.querySelectorAll('#screen .row')].map(r => r.textContent.replace(/\\s+$/, ''))")

    def ready(self):
        self.page.wait_for_function(
            "() => { const o = document.getElementById('oia-lock'); return !o || o.textContent === ''; }",
            timeout=90_000)

    def press(self, k):
        self.ready()
        self.page.keyboard.press(k)

    def typ(self, s):
        self.ready()
        self.page.keyboard.type(s)


class Tn5250:
    def __init__(self, host, cols=80, rows=26, binary=None):
        self.q = queue.Queue()

    def _loop(self, started):
        p = _Page()
        started.set()
        while True:
            fn, args, out = self.q.get()
            if fn is None:
                p.close()
                out.put((None, None))
                return
            try:
                out.put((getattr(p, fn)(*args), None))
            except Exception as e:  # noqa: BLE001
                out.put((None, e))

    def _call(self, fn, *args):
        out = queue.Queue()
        self.q.put((fn, args, out))
        r, e = out.get()
        if e:
            raise e
        return r

    def __enter__(self):
        started = threading.Event()
        self.th = threading.Thread(target=self._loop, args=(started,), daemon=True)
        self.th.start()
        started.wait()
        return self

    def __exit__(self, *exc):
        out = queue.Queue()
        self.q.put((None, None, out))
        out.get()

    def pump(self, timeout=0.2):
        time.sleep(timeout)

    def text(self):
        return "\n".join(self._call("rows"))

    def line(self, n):
        r = self._call("rows")
        return r[n - 1] if n <= len(r) else ""

    def wait_for(self, needle, timeout=10.0):
        end = time.time() + timeout
        while time.time() < end:
            t = self.text()
            if needle in t:
                return t
            time.sleep(0.1)
        raise AssertionError(f"timed out waiting for {needle!r}; screen was:\n{self.text()}")

    def send(self, s):
        if s == "\x0b":  # tn5250's Field Exit (Ctrl-K): the keypad Enter
            self._call("press", "NumpadEnter")
        else:
            self._call("typ", s)
        time.sleep(0.15)

    def type(self, s):
        self._call("typ", s)

    def tab(self):
        self._call("press", "Tab")

    def enter(self):
        self._call("press", "Enter")
        time.sleep(0.4)

    def fkey(self, n):
        self._call("press", f"F{n}" if n <= 12 else f"Shift+F{n - 12}")
        time.sleep(0.4)

    def page_down(self):
        self._call("press", "PageDown")
        time.sleep(0.4)

    def page_up(self):
        self._call("press", "PageUp")
        time.sleep(0.4)


mod = types.ModuleType("tn5250_driver")
mod.Tn5250 = Tn5250
sys.modules["tn5250_driver"] = mod
script = sys.argv[1]
sys.argv = sys.argv[1:]
runpy.run_path(script, run_name="__main__")
