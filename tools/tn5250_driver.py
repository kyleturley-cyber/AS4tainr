#!/usr/bin/env python3
"""Drive the real tn5250 client from scripts, for end-to-end tests.

tn5250 runs in a pseudo-terminal; its curses output is rendered by `pyte`
(a VT100 emulator) so tests can read the 5250 screen as text.

    with Tn5250("127.0.0.1:2323") as t:
        t.wait_for("Sign On")
        t.type("QSECOFR"); t.tab(); t.type("secret"); t.enter()

Keys follow tn5250's curses defaults: Enter = CR, Tab = next field,
F1..F12 = ESC 1..9, 0, -, =.

Requires: tn5250 on PATH (or $TN5250), and `pip install pyte`.
"""
import os
import pty
import select
import signal
import time

import pyte

FKEYS = {1: "1", 2: "2", 3: "3", 4: "4", 5: "5", 6: "6", 7: "7", 8: "8", 9: "9", 10: "0", 11: "-", 12: "="}


class Tn5250:
    def __init__(self, host, cols=80, rows=26, binary=None):
        self.binary = binary or os.environ.get("TN5250", "tn5250")
        self.host = host
        self.screen = pyte.Screen(cols, rows)
        self.stream = pyte.ByteStream(self.screen)
        self.pid = None
        self.fd = None
        self.rows, self.cols = rows, cols

    def __enter__(self):
        pid, fd = pty.fork()
        if pid == 0:
            # xterm-r6: no REP (repeat char) capability, which pyte does not emulate.
            os.environ["TERM"] = "xterm-r6"
            os.environ["LINES"] = str(self.rows)
            os.environ["COLUMNS"] = str(self.cols)
            os.execvp(self.binary, [self.binary, self.host])
        self.pid, self.fd = pid, fd
        import fcntl, struct, termios
        fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", self.rows, self.cols, 0, 0))
        return self

    def __exit__(self, *exc):
        try:
            os.kill(self.pid, signal.SIGTERM)
            os.waitpid(self.pid, 0)
        except OSError:
            pass

    def pump(self, timeout=0.2):
        end = time.time() + timeout
        while True:
            left = end - time.time()
            if left <= 0:
                return
            r, _, _ = select.select([self.fd], [], [], left)
            if not r:
                return
            try:
                data = os.read(self.fd, 65536)
            except OSError:
                return
            if not data:
                return
            self.stream.feed(data)

    def text(self):
        self.pump(0.05)
        return "\n".join(line.rstrip() for line in self.screen.display)

    def line(self, n):
        """5250 row n (1-based)."""
        return self.screen.display[n - 1].rstrip()

    def wait_for(self, needle, timeout=10.0):
        end = time.time() + timeout
        while time.time() < end:
            self.pump(0.1)
            if needle in self.text():
                return self.text()
        raise AssertionError(f"timed out waiting for {needle!r}; screen was:\n{self.text()}")

    def send(self, s):
        os.write(self.fd, s.encode("latin-1") if isinstance(s, str) else s)
        self.pump(0.15)

    def type(self, s):
        for ch in s:
            self.send(ch)

    def tab(self):
        self.send("\t")

    def enter(self):
        self.send("\r")
        self.pump(0.4)

    def fkey(self, n):
        self.send("\x1b" + FKEYS[n])
        self.pump(0.4)


if __name__ == "__main__":
    import sys

    with Tn5250(sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:2323") as t:
        t.pump(3)
        print(t.text())
