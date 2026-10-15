#!/usr/bin/env python3
"""End-to-end test of the ibmish 5250 server with the real tn5250 client.

Expects a fresh database (QSECOFR with its initial, expired password), the
sample file CUSTMAST and the display files CUSTINQD and CUSTWRKD in QGPL.
Usage:

    TN5250=/path/to/tn5250 python3 tools/e2e_5250.py 127.0.0.1:2323
"""
import os
import sys

sys.path.insert(0, os.path.dirname(__file__))
from tn5250_driver import Tn5250  # noqa: E402

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:2323"
NEWPW = "Green5creen!"
passed = 0


def check(cond, what, t):
    global passed
    if not cond:
        print(f"FAIL: {what}\n--- screen ---\n{t.text()}\n--------------")
        sys.exit(1)
    passed += 1
    print(f"ok   {what}")


def command(t, cmd, expect, timeout=20):
    t.type(cmd)
    t.enter()
    return t.wait_for(expect, timeout)


with Tn5250(HOST) as t:
    t.wait_for("Sign On")
    check("IBMISH" in t.line(2) and "QPADEV" in t.line(4), "sign-on screen shows system and display names", t)

    t.type("qsecofr"); t.tab(); t.type("wrong"); t.enter()
    t.wait_for("CPF1107")
    check("CPF1107" in t.line(24), "a wrong password is rejected on the message line", t)

    t.type("qsecofr"); t.tab(); t.type("QSECOFR"); t.enter()
    t.wait_for("Change Password")
    check("expired" in t.line(24), "the initial QSECOFR password must be changed", t)

    t.type("QSECOFR"); t.tab(); t.type(NEWPW); t.tab(); t.type("mismatch"); t.enter()
    t.wait_for("do not match")
    check(True, "new and verify passwords must match", t)

    t.type("QSECOFR"); t.tab(); t.type(NEWPW); t.tab(); t.type(NEWPW); t.enter()
    t.wait_for("Password changed")
    check("Sign On" in t.text(), "after the change, back to sign-on", t)

    t.type("qsecofr"); t.tab(); t.type("QSECOFR"); t.enter()
    t.wait_for("CPF1107")
    check(True, "the old password no longer works", t)

    t.type("qsecofr"); t.tab(); t.type(NEWPW); t.enter()
    t.wait_for("Command Entry")
    check("QSECOFR" in t.line(2), "signed on: command entry shows the user", t)

    command(t, "crtlib mylib text('My first library')", "Library MYLIB created.")
    check("Library MYLIB created." in t.line(24), "CRTLIB creates a library", t)

    command(t, "CRTLIB LIB(MYLIB)", "CPF2111")
    check("CPF2111" in t.line(24), "CRTLIB of an existing library fails with CPF2111", t)
    t.fkey(12)  # clear the retained command

    command(t, "dsplib qsys", "Display Library")
    screen = t.text()
    check("MYLIB" in screen and "*LIB" in screen and "My first library" in screen,
          "DSPLIB QSYS lists the new library with its text", t)
    t.fkey(3)
    t.wait_for("Command Entry")

    command(t, "dsplib qgpl", "Display Library")
    screen = t.text()
    check("CUSTMAST" in screen and "*FILE" in screen and "PF" in screen, "DSPLIB QGPL lists the sample physical files", t)
    t.fkey(3)
    t.wait_for("Command Entry")

    command(t, "dsplib nosuch", "CPF2110")
    check("CPF2110" in t.line(24), "DSPLIB of a missing library fails with CPF2110", t)
    t.fkey(12)

    command(t, "foo bar", "CPD0030")
    check("CPD0030" in t.line(24), "an unknown command gives CPD0030", t)
    t.fkey(12)

    command(t, "call custlist parm('3')", "Found Bluebird Supply in Omaha", timeout=90)
    check("DSPLY" in t.text(), "CALL runs an RPG program on Jariko and shows its DSPLY output", t)

    t.fkey(9)
    t.pump(0.5)
    check("call custlist" in t.line(20), "F9 retrieves the previous command", t)
    t.fkey(12)

    # Interactive RPG: EXFMT with the CUSTINQD display file.
    FIELD_EXIT = "\x0b"  # tn5250: Ctrl-K
    command(t, "call custinq", "Customer Inquiry", timeout=90)
    check("Customer number" in t.line(5) and "F3=Exit" in t.line(23), "EXFMT shows the PROMPT record from DDS", t)
    t.type("42"); t.enter()
    t.wait_for("Customer not found.")
    check("Customer not found." in t.line(24), "ERRMSG on *IN30 shows for an unknown customer", t)
    t.type("5"); t.send(FIELD_EXIT); t.enter()
    t.wait_for("Lake, Pine & Co")
    check("Duluth" in t.line(7), "DETAIL shows the record read with CHAIN", t)
    check("75.25-" in t.line(8), "EDTCDE(J) edits a negative balance", t)
    t.fkey(5)
    t.wait_for("not valid")
    check("F5" in t.line(24), "an undefined function key is rejected on the display", t)
    t.fkey(12)
    t.wait_for("Type a customer number")
    check(True, "F12 (CA12) returns to PROMPT", t)
    t.type("1"); t.send(FIELD_EXIT); t.enter()
    t.wait_for("Acme Tooling")
    check("1,250.50" in t.line(8), "EDTCDE(J) inserts thousands separators", t)
    t.fkey(3)
    t.wait_for("Command Entry")
    check("ended abnormally" not in t.text(), "F3 (CA03) ends the program normally", t)

    # Subfile: WRKCUST lists the customers in key order, 4 to a page.
    command(t, "call wrkcust", "Work with Customers", timeout=90)
    check("Acme Tooling" in t.line(7) and "Prairie Freight" in t.line(10),
          "the subfile shows its first page (SFLPAG 4) in key order", t)
    check("More..." in t.line(11) and "F3=Exit" in t.line(23),
          "SFLEND(*MORE) says More...; the FOOTER record stays (OVERLAY)", t)
    t.page_down()
    t.wait_for("Lake, Pine")
    check("Lake, Pine & Co" in t.line(7) and "Bottom" in t.line(11),
          "Page Down shows the next page, the last one says Bottom", t)
    t.page_down()
    t.wait_for("bottom of the list")
    check(True, "Page Down on the last page is refused", t)
    t.page_up()
    t.wait_for("Acme Tooling")
    check("Prairie Freight" in t.line(10), "Page Up returns to the first page", t)
    t.tab(); t.tab(); t.type("5"); t.enter()  # option 5 on the third line
    t.wait_for("Display Customer")
    check("Bluebird Supply" in t.line(6) and "Omaha" in t.line(7),
          "READC returns the line with an option; option 5 shows that customer", t)
    t.fkey(12)
    t.wait_for("Work with Customers")
    check(t.line(9)[2:3].strip() == "", "UPDATE of the subfile record cleared the option", t)
    t.type("x"); t.enter()  # a wrong option on the first line
    t.wait_for("Type option 5")
    check("Type option 5 or leave the option blank." in t.line(24),
          "a wrong option shows SFLMSG", t)
    check("X" == t.line(7)[2:3], "the wrong option stays on the line (SFLNXTCHG)", t)
    t.fkey(3)
    t.wait_for("Command Entry")
    check("ended abnormally" not in t.text(), "F3 ends WRKCUST normally", t)

    command(t, "signoff", "Sign On")
    check("signed off" in t.line(24), "SIGNOFF returns to the sign-on screen", t)

print(f"\nAll {passed} checks passed.")
