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

    # Blanks in front of the password (tn5250j sends them when the cursor
    # was put in the middle of the field) are not part of it. At QPWDLVL 0
    # (IBM's shipped value, ibmish's default) passwords are not case
    # sensitive.
    t.type("qsecofr"); t.tab(); t.type("   " + NEWPW.lower()); t.enter()
    t.wait_for("Command Entry")
    check("QSECOFR" in t.line(2), "signed on (blanks ignored, password not case sensitive): command entry shows the user", t)

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

    # DSPLIBL: system part, no current library (QSECOFR has *CRTDFT), user part.
    command(t, "dsplibl", "Display Library List")
    check(t.line(8).split()[:2] == ["QSYS", "SYS"] and t.line(11).split()[:2] == ["QUSRSYS", "SYS"],
          "DSPLIBL starts with the system part of the library list", t)
    check(t.line(12).split()[:2] == ["QGPL", "USR"] and t.line(13).split()[:2] == ["QTEMP", "USR"] and " CUR " not in t.text(),
          "the user part is QGPL and QTEMP, with no current library", t)
    t.tab(); t.tab(); t.tab(); t.tab(); t.type("5"); t.enter()
    t.wait_for("Display Library")
    check("CUSTMAST" in t.text() and "QGPL" in t.text(), "option 5 on QGPL shows its objects (DSPLIB)", t)
    t.fkey(12)
    t.wait_for("Display Library List")
    check(True, "F12 from DSPLIB returns to the library list", t)
    t.fkey(3)
    t.wait_for("Command Entry")
    command(t, "chgcurlib mylib", "Current library changed to MYLIB.")
    command(t, "dsplibl", "Display Library List")
    check(t.line(12).split()[:2] == ["MYLIB", "CUR"], "CHGCURLIB puts the current library between the system and user parts", t)
    t.fkey(3); t.wait_for("Command Entry")
    command(t, "rmvlible qtemp", "Library QTEMP removed from library list.")
    check(True, "RMVLIBLE removes a library from the user part", t)

    # A CL program that calls an RPG program.
    command(t, "call custrpt parm(L)", "Report done for QSECOFR", timeout=90)
    check("Steps 123 total 7.50" in t.text() and "DSPLY  Acme Tooling 1250.50" in t.text(),
          "CALL runs a CL program: its messages and the RPG program's DSPLY output are in the log", t)

    # SQL: STRSQL shows rows, RUNSQL changes them.
    command(t, "strsql", "Enter SQL Statements")
    t.type("select cusno, cusnam, cusbal from qgpl/custmast where cusbal > 0 order by cusbal desc"); t.enter()
    t.wait_for("Display Data")
    check(t.line(4).split() == ["CUSNO", "CUSNAM", "CUSBAL"] and "Prairie Freight" in t.line(5) and "9999999.99" in t.line(5),
          "STRSQL: SELECT shows the rows under their column names, without the record number", t)
    t.fkey(3)
    t.wait_for("3 rows selected.")
    check(True, "back on the SQL screen, the row count is shown", t)
    t.type("select * from nosuch"); t.enter()
    t.wait_for("SQL0204")
    check(True, "an unknown table gives SQL0204", t)
    t.fkey(3)
    t.wait_for("Command Entry")
    command(t, "runsql sql('update custmast set cusctY = ''Des Moines'' where cusno = 1')", "1 rows updated.")
    check(True, "RUNSQL runs an UPDATE and reports the rows changed", t)
    command(t, "runsql sql('select * from custmast')", "SQL0084")
    check(True, "RUNSQL does not run SELECT", t)
    t.fkey(12)

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

    # F4 prompting, from the command definitions.
    t.type("crtlib"); t.fkey(4)
    t.wait_for("Create Library (CRTLIB)")
    check("Library" in t.line(5) and "*PROD" in t.line(6), "F4 prompts CRTLIB with its parameters and defaults", t)
    t.type("prmlib"); t.tab(); t.tab(); t.type("Made with F4"); t.enter()
    t.wait_for("Library PRMLIB created.")
    check("CRTLIB LIB(PRMLIB) TEXT('Made with F4')" in t.text(),
          "Enter on the prompt runs the command it built", t)
    t.type("dspusrprf"); t.fkey(4)
    t.wait_for("Display User Profile (DSPUSRPRF)")
    t.enter()
    t.wait_for("required parameter USRPRF")
    check(True, "the prompt checks required parameters", t)
    t.fkey(12)
    t.wait_for("Command Entry")
    check("dspusrprf" in t.line(20), "F12 on the prompt returns with the command as typed", t)
    t.fkey(12)
    t.fkey(4)
    t.wait_for("Command Groups")
    check("4. User profile commands" in t.text() and "Selection or command" in t.text(),
          "F4 on an empty command line shows a numbered menu of command groups", t)
    t.type("9"); t.enter()
    t.wait_for("Selection 9 is not valid")
    check(True, "a number that is not on the menu is rejected", t)
    t.type("4"); t.enter()
    t.wait_for("User Profile Commands")
    check("2. Display User Profile" in t.text() and "DSPUSRPRF" in t.text(),
          "an option number opens the group's numbered command menu", t)
    t.type("2"); t.enter()
    t.wait_for("Display User Profile (DSPUSRPRF)")
    check(True, "an option number on the command menu prompts that command", t)
    t.type("qsecofr"); t.enter()
    t.wait_for("Special authority")
    check("*SECOFR" in t.text(), "Enter on the prompt runs the command chosen from the menu", t)
    t.fkey(3)
    t.wait_for("Command Entry")
    t.fkey(4)
    t.wait_for("Command Groups")
    t.type("1"); t.enter()
    t.wait_for("Library Commands")
    t.fkey(12)
    t.wait_for("Command Groups")
    check(True, "F12 goes back one menu", t)
    t.type("dsplibl"); t.enter()
    t.wait_for("Display Library List")
    check(True, "a command typed on the menu's selection line runs", t)
    t.fkey(3)
    t.wait_for("Command Entry")

    # User profiles, WRKOBJ, DSPPFM.
    command(t, "crtusrprf bob password(Secret42) usrcls(*pgmr) text('Bob the builder')", "User profile BOB created.")
    check(True, "CRTUSRPRF creates a profile", t)
    command(t, "dspusrprf bob", "Display User Profile")
    check("*PGMR" in t.text() and "Bob the builder" in t.text(), "DSPUSRPRF shows the class and text", t)
    t.fkey(3); t.wait_for("Command Entry")
    command(t, "wrkobj qgpl/cust*", "Work with Objects")
    screen = t.text()
    check("CUSTMAST" in screen and "CUSTWRKD" in screen and "ORDLIN" not in screen,
          "WRKOBJ with a generic name lists the matching objects", t)
    t.fkey(3); t.wait_for("Command Entry")
    command(t, "dsppfm custmast", "Display Physical File Member")
    check("Bluebird Supply" in t.line(8) and "CUSNAM" in t.line(7),
          "DSPPFM shows the records in arrival order under the field names", t)
    t.fkey(3); t.wait_for("Command Entry")

    command(t, "signoff", "Sign On")
    check("signed off" in t.line(24), "SIGNOFF returns to the sign-on screen", t)

    # The new user: no *SECADM, so no CRTUSRPRF; CHGPWD shows the screen.
    t.type("bob"); t.tab(); t.type("secret42"); t.enter()
    t.wait_for("Command Entry")
    check("BOB" in t.line(2), "the new profile signs on", t)
    command(t, "crtusrprf eve", "Not authorized")
    check("*SECADM" in t.line(24), "CRTUSRPRF needs *SECADM special authority", t)
    t.fkey(12)
    command(t, "chgpwd", "Change Password")
    t.fkey(12)
    t.wait_for("Password not changed.")
    check(True, "CHGPWD shows Change Password; F12 leaves the password as it was", t)
    command(t, "signoff", "Sign On")
    t.type("qsecofr"); t.tab(); t.type(NEWPW); t.enter()
    t.wait_for("Command Entry")
    command(t, "dltusrprf bob", "User profile BOB deleted.")
    check(True, "DLTUSRPRF deletes the profile", t)
    command(t, "signoff", "Sign On")

print(f"\nAll {passed} checks passed.")
