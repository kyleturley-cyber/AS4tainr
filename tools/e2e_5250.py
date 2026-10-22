#!/usr/bin/env python3
"""End-to-end test of the as4tainr 5250 server with the real tn5250 client.

Expects a fresh database (QSECOFR with its initial, expired password) and
the sample objects in QGPL (scripts/samples.sh).

Command Entry follows IBM i 7.5 as captured on PUB400 (tools/golden/screens.txt,
docs/golden-outputs.md): no user name on it, the previous commands with their
messages as texts without message IDs, nothing on line 24 after a command, a
command line on rows 18-21, and a command missing a required parameter is
prompted. So checks look for message texts in the log, not IDs on line 24.

Usage:

    TN5250=/path/to/tn5250 python3 tools/e2e_5250.py 127.0.0.1:2323
"""
import os
import sys
import time

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


def qcmd(t):
    """From the initial menu (MAIN) to Command Entry, as CALL QCMD does."""
    t.wait_for("as4tainr Main Menu", 30)
    command(t, "call qcmd", "Command Entry")


with Tn5250(HOST) as t:
    t.wait_for("Sign On")
    check("AS4TAINR" in t.line(2) and "QPADEV" in t.line(4), "sign-on screen shows system and display names", t)

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
    # (IBM's shipped value, as4tainr's default) passwords are not case
    # sensitive.
    t.type("qsecofr"); t.tab(); t.type("   " + NEWPW.lower()); t.enter()
    # The initial menu: CRTUSRPRF INLMNU is MAIN unless changed (IBM i CL
    # reference), and QSECOFR is shipped with INLMNU(MAIN).
    t.wait_for("as4tainr Main Menu")
    # IBM i 7.5's menus show "System:" on line 2, not the user (PUB400, MAIN).
    check("MAIN" in t.line(1) and "System:   AS4TAINR" in t.line(2) and "Selection or command" in t.text()
          and "90. Sign off" in t.text(),
          "signed on (blanks ignored, password not case sensitive): the initial menu MAIN is shown", t)
    command(t, "call qcmd", "Command Entry")
    check("Request level:" in t.line(2) and "(No previous commands or messages)" in t.text(),
          "CALL QCMD from the menu's command line shows Command Entry with no previous commands", t)

    command(t, "crtlib mylib text('My first library')", "Library MYLIB was created.")
    check("> crtlib mylib" in t.text() and "Library MYLIB was created." in t.text() and t.line(24).strip() == "",
          "CRTLIB creates a library: the command and its message in the log, line 24 empty", t)

    command(t, "CRTLIB LIB(MYLIB)", "already exists")
    check("Library MYLIB already exists." in t.text() and "CPF2111" not in t.text()
          and "CRTLIB LIB(MYLIB)" in t.line(18),
          "CRTLIB of an existing library fails (CPF2111, shown as text); the command stays on the command line", t)
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
    # IBM i 7.5's DSPLIBL (PUB400): "System:" on line 2, the options on
    # lines 5-6, "ASP" and the column headings on lines 8-9, the list from 10.
    check(t.line(10).split()[:2] == ["QSYS", "SYS"] and t.line(13).split()[:2] == ["QUSRSYS", "SYS"]
          and "Device" in t.line(9) and "ASP" in t.line(8),
          "DSPLIBL starts with the system part of the library list", t)
    check(t.line(14).split()[:2] == ["QGPL", "USR"] and t.line(15).split()[:2] == ["QTEMP", "USR"] and " CUR " not in t.text(),
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
    check(t.line(14).split()[:2] == ["MYLIB", "CUR"], "CHGCURLIB puts the current library between the system and user parts", t)
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
    t.wait_for("NOSUCH not found.")
    check("SQL0204" not in t.text(), "an unknown table gives SQL0204, shown as its text", t)
    t.fkey(3)
    t.wait_for("Command Entry")
    # RUNSQL sends no completion message; a statement it cannot run ends
    # with SQL9010 (observed on IBM i 7.5).
    command(t, "runsql sql('update custmast set cusctY = ''Ames'' where cusno = 1')", "where cusno = 1')")
    command(t, "dsppfm custmast", "Display Physical File Member")
    check("Ames" in t.text(), "RUNSQL runs an UPDATE", t)
    t.fkey(3); t.wait_for("Command Entry")
    command(t, "runsql sql('update custmast set cusctY = ''Des Moines'' where cusno = 1')", "where cusno = 1')")
    command(t, "runsql sql('select * from custmast')", "RUNSQL failed")
    check(True, "RUNSQL does not run SELECT (SQL9010)", t)
    t.fkey(12)

    command(t, "dsplib nosuch", "Library NOSUCH does not exist.")
    check(True, "DSPLIB of a missing library fails (CPF2110)", t)
    t.fkey(12)

    command(t, "foo bar", "Command FOO was not found")
    check("The command has errors and was not run." in t.text(),
          "an unknown command: its diagnostic (CPD0030), then CPF0006, both in the log", t)
    t.fkey(12)

    command(t, "call custlist parm('3')", "Found Bluebird Supply in Omaha", timeout=90)
    check("DSPLY" in t.text(), "CALL runs an RPG program and shows its DSPLY output", t)

    t.fkey(9)
    t.pump(0.5)
    check("call custlist" in t.line(18), "F9 retrieves the previous command", t)
    t.fkey(12)

    # DSPLY with a response asks the operator.
    command(t, "call askname", "Display Program Messages", timeout=90)
    check("What is your name?" in t.line(3) and "Reply" in t.line(23),
          "DSPLY with a response shows the message and a reply line", t)
    t.type("Ada"); t.enter()
    t.wait_for("Hello, Ada")
    check("Command Entry" in t.text(), "the reply goes into the response field", t)

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

    # The order entry application (Phase 2 gate): a CL driver, an RPG
    # program with an input subfile, five files and a data area.
    command(t, "call ordentc", "Order Entry", timeout=90)
    check("Customer number" in t.line(5), "ORDENTC calls ORDENT, which asks for a customer", t)
    t.enter()
    t.wait_for("Type a customer number.")
    check(True, "Enter with no customer number is refused (ERRMSG)", t)
    t.type("42"); t.send(FIELD_EXIT); t.enter()
    t.wait_for("Customer not found.")
    check(True, "an unknown customer is refused", t)
    t.type("2"); t.send(FIELD_EXIT); t.enter()
    t.wait_for("Midwest Fasteners")
    check("Chicago" in t.line(4) and "Description" in t.line(8) and "More..." in t.line(19),
          "the order screen shows the customer and an empty input subfile", t)
    check(t.line(9)[18:23].strip() == "", "a zero quantity shows as blanks (EDTCDE(Z) on an input field)", t)
    t.fkey(6)
    t.wait_for("Type at least one item")
    check(True, "F6 with no lines is refused (SFLMSG)", t)
    t.type("bolt-m8"); t.tab(); t.type("40"); t.send(FIELD_EXIT)
    t.type("nosuch"); t.tab(); t.type("1"); t.send(FIELD_EXIT)
    t.enter()
    t.wait_for("Item not found.")
    check("Hex bolt M8 x 30" in t.line(9) and "6.00" in t.line(9) and "NOSUCH" in t.line(10),
          "Enter prices the good line from ITEMMAST and flags the unknown item", t)
    check("6.00" in t.line(21), "the order total so far is shown", t)
    t.type("pump-200"); t.send(FIELD_EXIT)  # the cursor is on the wrong item
    t.tab()
    t.type("nut-m8"); t.send(FIELD_EXIT)
    t.enter()
    t.wait_for("Quantity must be more than zero.")
    check("Transfer pump" in t.line(10) and "189.00" in t.line(10),
          "the corrected item is priced; a line without a quantity is refused", t)
    t.type("100"); t.send(FIELD_EXIT)  # the cursor is on the missing quantity
    t.enter()
    t.wait_for("200.00")
    check("5.00" in t.line(11) and "200.00" in t.line(21),
          "all three lines priced: 6.00 + 189.00 + 5.00 = 200.00", t)
    t.fkey(6)
    t.wait_for("created")
    check("Order    1004 created, total       200.00" in t.line(21),
          "F6 creates order 1004 (number from data area ORDNXT) and returns to the prompt", t)
    t.fkey(3)
    t.wait_for("next order number")
    check("The next order number is 1005." in t.text(),
          "F3 ends ORDENT; ORDENTC reads the data area with RTVDTAARA", t)
    command(t, "dsppfm ordlin", "Display Physical File Member")
    screen = t.text()
    check("PUMP-200" in screen and "189.00" in screen and "NUT-M8" in screen,
          "the order lines are in ORDLIN", t)
    t.fkey(3); t.wait_for("Command Entry")
    command(t, "dsppfm itemmast", "Display Physical File Member")
    check("4960" in t.text() and "7900" in t.text(), "the quantities were taken off ITMQOH", t)
    t.fkey(3); t.wait_for("Command Entry")
    command(t, "dspdtaara ordnxt", "Display Data Area")
    check("*DEC" in t.text() and "1005" in t.text() and "Next order number" in t.text(),
          "DSPDTAARA shows the data area", t)
    t.fkey(3); t.wait_for("Command Entry")

    # Spooled files: a printed RPG report (O specs, EXCEPT) and a display
    # command in batch both become spooled files QSYSPRT.
    command(t, "sbmjob cmd(call custprt) job(custprt)", "was put on job queue QBATCH")
    command(t, "sbmjob cmd(dsppfm custmast) job(listing)", "was put on job queue QBATCH")
    command(t, "wrksplf", "Work with All Spooled Files")
    for _ in range(60):
        if t.text().count("QSYSPRT") >= 2:
            break
        t.fkey(5)
        t.pump(1.5)
    check(t.text().count("QSYSPRT") >= 2 and "CUSTPRT" in t.text(),
          "WRKSPLF lists the report (user data CUSTPRT) and the DSPPFM listing", t)
    t.type("5"); t.enter()
    t.wait_for("Display Spooled File")
    screen = t.text()
    check("Customer Balance Report" in screen and "Midwest Fasteners" in screen and "Customers:     5" in screen,
          "DSPSPLF shows the report as printed: heading, detail lines, totals", t)
    t.fkey(20)
    t.pump(1)
    t.wait_for("75.25-*")
    check("Page    1" in t.text(),
          "F20 shows the rest of the 132-column lines: page number, edit code J and the conditioned '*'", t)
    t.fkey(12); t.wait_for("Work with All Spooled Files")
    t.tab(); t.type("4"); t.enter()
    t.wait_for("deleted")
    # WRKSPLF's first view shows no job names (IBM i 7.5, PUB400).
    check(sum("QSYSPRT" in t.line(i) for i in range(8, 21)) == 1 and "CUSTPRT" in t.text(),
          "option 4 deletes the listing (DLTSPLF); the list is refreshed", t)
    t.fkey(3); t.wait_for("Command Entry")

    # F4 prompting, from the command definitions.
    t.type("crtlib"); t.fkey(4)
    t.wait_for("Create Library (CRTLIB)")
    check("Library" in t.line(5) and "*PROD" in t.line(6), "F4 prompts CRTLIB with its parameters and defaults", t)
    t.type("prmlib"); t.tab(); t.tab(); t.type("Made with F4"); t.enter()
    t.wait_for("Library PRMLIB was created.")
    check("CRTLIB LIB(PRMLIB) TEXT('Made with F4')" in t.text(),
          "Enter on the prompt runs the command it built", t)
    t.type("dspusrprf"); t.fkey(4)
    t.wait_for("Display User Profile (DSPUSRPRF)")
    t.enter()
    t.wait_for("needs a value")
    check(True, "the prompt checks required parameters", t)
    t.fkey(12)
    t.wait_for("Command Entry")
    # IBM i 7.5 (PUB400): F12 on a prompt logs the command and CPF6801,
    # and the command line is empty.
    check("> DSPUSRPRF" in t.text() and "Command prompting was ended" in t.text()
          and t.line(18).strip() == "===>",
          "F12 on the prompt: the command and 'prompting ended' in the log, the command line empty", t)
    # A command without a required parameter is prompted (CRTDTAARA
    # without TYPE on PUB400 shows the prompt, the cursor on TYPE).
    command(t, "dspusrprf", "Display User Profile (DSPUSRPRF)")
    check("needs a value" in t.line(24), "a command missing a required parameter is prompted, with the message", t)
    t.fkey(12); t.wait_for("Command Entry")
    # A command with more parameters than one screen holds: Page Down and
    # Page Up move between the prompt's pages.
    t.type("crtprtf"); t.fkey(4)
    # "More..." or "Bottom" on row 21 of a prompt (IBM i 7.5, PUB400 CRTDTAARA prompt).
    t.wait_for("Create Printer File (CRTPRTF)")
    # CRTPRTF's first screen has FILE, SRCFILE, SRCMBR, GENLVL, FLAG, DEV,
    # DEVTYPE and TEXT; F10 adds the others (IBM i 7.5, PUB400).
    check("Bottom" in t.line(21) and "Printer device type" in t.text() and "Page size" not in t.text()
          and "F10=Additional parameters" in t.text(),
          "CRTPRTF's prompt shows its main parameters first", t)
    t.fkey(10)
    t.wait_for("Additional Parameters")
    check("More..." in t.line(21) and "Replace file" not in t.text(),
          "F10 adds the additional parameters: the prompt is now longer than a screen", t)
    t.page_down()
    t.wait_for("Replace file")
    check("Bottom" in t.line(21), "Page Down shows the prompt's next page", t)
    t.page_up()
    t.wait_for("More...")
    check("Source file" in t.text(), "Page Up goes back to the first page", t)
    t.fkey(12)
    t.wait_for("Command Entry")
    t.fkey(12)
    t.fkey(4)
    t.wait_for("Command Groups")
    check("4. User profile commands" in t.text() and "Selection or command" in t.text(),
          "F4 on an empty command line shows a numbered menu of command groups", t)
    t.type("99"); t.enter()  # the menu has fewer groups than that
    t.wait_for("Selection 99 is not valid")
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
    check("More..." in t.line(18), "a menu longer than one screen shows More...", t)
    t.page_down()
    t.wait_for("Bottom")
    check("16. Session commands" in t.text(), "Page Down shows the rest of the menu", t)
    t.page_up()
    t.wait_for("More...")
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

    # Phase 3: two users signed on at once, and a batch job.
    with Tn5250(HOST) as t2:
        t2.wait_for("Sign On")
        t2.type("bob"); t2.tab(); t2.type("secret42"); t2.enter()
        t2.wait_for("as4tainr Main Menu", 30)
        check("Selection or command" in t2.text(), "a second session signs on as BOB while QSECOFR is signed on", t2)
        command(t2, "call qcmd", "Command Entry")
        command(t2, "sbmjob cmd(call pgm(custrpt) parm(L)) job(rpt)", "was put on job queue QBATCH")
        check("/BOB/RPT was put on job queue QBATCH" in t2.text(),
              "SBMJOB puts job RPT on job queue QBATCH (CPC1221)", t2)
        command(t, "wrkactjob", "Work with Active Jobs")
        screen = t.text()
        check("QINTER" in screen and "QBATCH" in screen and "BOB" in screen and "CMD-WRKACTJOB" in screen,
              "WRKACTJOB in QSECOFR's session lists BOB's interactive job under QINTER", t)
        t.fkey(3); t.wait_for("Command Entry")
        command(t2, "wrksbmjob", "Work with Submitted Jobs")
        for _ in range(60):
            if "OUTQ" in t2.text():
                break
            t2.fkey(5)
            t2.pump(1.5)
        check("RPT" in t2.text() and "OUTQ" in t2.text() and "abnormally" not in t2.text(),
              "QBATCH runs the job: WRKSBMJOB shows it ended (OUTQ); F5 refreshes", t2)
        t2.type("5"); t2.enter()
        t2.wait_for("Display Job Log")
        screen = t2.text()
        check("> call pgm(custrpt) parm(L)" in screen and "started in subsystem QBATCH" in screen,
              "option 5 shows the batch job's log: the command it ran", t2)
        # Job logs and message queues show message texts, not IDs (IBM i 7.5
        # DSPJOBLOG and DSPMSG on PUB400); CPF1241 is "... completed normally".
        for _ in range(5):
            if "completed normally" in t2.text():
                break
            t2.page_down(); t2.pump(0.5)
        check("Report done for BOB" in t2.text() and "completed normally" in t2.text(),
              "the job log has the CL program's messages and the completion message (CPF1241)", t2)
        t2.fkey(3); t2.wait_for("Command Entry")
        command(t2, "dspmsg", "Display Messages")
        check("/BOB/RPT completed normally" in t2.text() and "From  . . . :   QSYS" in t2.text(),
              "BOB gets CPF1241 on his message queue when the job ends", t2)
        t2.fkey(3); t2.wait_for("Command Entry")
        command(t2, "sndmsg msg('Report is ready') tousr(qsecofr)", "Message sent to QSECOFR.")
        check(True, "SNDMSG sends a message to another user", t2)
        command(t, "dsplibl", "Display Library List")
        t.fkey(3)
        t.wait_for("new message")
        check("DSPMSG" in t.line(24), "QSECOFR's session says a new message is waiting", t)
        command(t, "dspmsg", "Display Messages")
        check("Report is ready" in t.text() and "From  . . . :   BOB" in t.text(), "DSPMSG shows BOB's message", t)
        t.fkey(3); t.wait_for("Command Entry")
        # Record locks: QSECOFR changes customer 3, which locks its record
        # until the change is made; BOB's CHAIN of it waits.
        command(t, "call custmnt", "Change Customer", timeout=90)
        t.type("3"); t.send(FIELD_EXIT); t.enter()
        t.wait_for("Type changes")
        check("Bluebird Supply" in t.line(6), "CUSTMNT shows customer 3 for change (record locked)", t)
        command(t2, "call custmnt", "Change Customer", timeout=90)
        started = time.time()
        t2.type("3"); t2.send(FIELD_EXIT); t2.enter()
        t2.wait_for("in another job", timeout=90)
        check(time.time() - started >= 3,
              "BOB's CHAIN of the locked record waits, then sets the error indicator (CPF5027)", t2)
        t2.type("3"); t2.send(FIELD_EXIT); t2.enter()  # waits again
        time.sleep(1)
        t.tab(); t.type("Kansas City"); t.send(FIELD_EXIT); t.enter()
        t.wait_for("Customer changed.")
        t2.wait_for("Kansas City", timeout=90)
        check("Type changes" in t2.text(),
              "UPDATE releases the lock: BOB's waiting CHAIN gets the record with QSECOFR's change", t2)
        t2.fkey(12); t2.wait_for("Type a customer number")
        t.type("3"); t.send(FIELD_EXIT); t.enter()
        t.wait_for("Type changes")
        check("Kansas City" in t.line(7),
              "F12 (UNLOCK) released BOB's lock: QSECOFR can change customer 3 again", t)
        t.fkey(12); t.wait_for("Type a customer number")
        t.fkey(3); t.wait_for("Command Entry")
        t2.fkey(3); t2.wait_for("Command Entry")
        # Object authority: with *PUBLIC *USE, BOB cannot open CUSTMAST
        # for update; a private *CHANGE lets him.
        command(t, "grtobjaut obj(custmast) objtype(*file) user(*public) aut(*use)", "*PUBLIC has authority *USE")
        command(t2, "call custmnt", "Not authorized", timeout=90)
        check("Not authorized" in t2.text(),
              "with *PUBLIC *USE, BOB's program cannot open CUSTMAST for update (CPF9802)", t2)
        command(t, "grtobjaut custmast *file user(bob) aut(*change)", "BOB has authority *CHANGE")
        command(t, "dspobjaut custmast *file", "Display Object Authority")
        check("BOB" in t.text() and "*CHANGE" in t.text() and "*USE" in t.text(),
              "DSPOBJAUT shows the owner, BOB's private authority and *PUBLIC", t)
        t.fkey(3); t.wait_for("Command Entry")
        t2.fkey(12)
        command(t2, "call custmnt", "Change Customer", timeout=90)
        check(True, "a private *CHANGE authority beats *PUBLIC *USE: BOB's program runs", t2)
        t2.fkey(3); t2.wait_for("Command Entry")
        command(t, "grtobjaut custmast *file user(*public) aut(*change)", "*PUBLIC has authority *CHANGE")
        command(t, "rvkobjaut custmast *file user(bob)", "revoked")
        command(t2, "signoff", "Sign On")
    command(t, "dspjoblog", "Display Job Log")
    check("> crtlib mylib text('My first library')" in t.text() and "QINTER" in t.text(),
          "DSPJOBLOG shows this session's own job log, oldest first", t)
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
    t.wait_for("as4tainr Main Menu", 30)
    check("Selection or command" in t.text(), "the new profile signs on", t)
    command(t, "call qcmd", "Command Entry")
    command(t, "crtusrprf eve", "Not authorized")
    check("*SECADM" in t.text(), "CRTUSRPRF needs *SECADM special authority", t)
    t.fkey(12)
    command(t, "chgpwd", "Change Password")
    t.fkey(12)
    t.wait_for("Password not changed.")
    check(True, "CHGPWD shows Change Password; F12 leaves the password as it was", t)
    command(t, "signoff", "Sign On")
    t.type("qsecofr"); t.tab(); t.type(NEWPW); t.enter()
    qcmd(t)
    command(t, "dltusrprf bob", "User profile BOB deleted.")
    check(True, "DLTUSRPRF deletes the profile", t)

    # Menus. Expected behavior from IBM's CL reference (GO, CRTMNU,
    # CRTUSRPRF INLPGM/INLMNU): a menu takes an option number or a command
    # on its "Selection or command" line; F12 goes back one display, F3
    # leaves the menus; option 90 signs off; INLPGM runs at sign-on, and
    # INLMNU(*SIGNOFF) signs off when it ends; ending the initial menu
    # signs off.
    command(t, "go main", "as4tainr Main Menu")
    check("Select one of the following:" in t.text() and " 1. User tasks" in t.text(),
          "GO MAIN shows the main menu from Command Entry", t)
    command(t, "1", "User Tasks")
    check("USER" in t.line(1) and "90. Sign off" in t.text(), "option 1 goes to the User Tasks menu (GO USER)", t)
    command(t, "5", "Display Job Log")
    t.fkey(3); t.wait_for("User Tasks")
    check(True, "a menu option runs its command; leaving the display returns to the menu", t)
    t.fkey(12); t.wait_for("as4tainr Main Menu")
    check(True, "F12 goes back one menu", t)
    command(t, "99", "not one of the options")
    check("99" in t.line(24), "a number that is not an option is rejected on the message line", t)
    command(t, "crtlib menulib", "Library MENULIB was created.")
    check("as4tainr Main Menu" in t.text(), "a command typed on a menu's command line runs there, the menu stays", t)
    command(t, "6", "Go to Menu (GO)")
    check(True, "option 6 prompts GO (a ? in front of the command prompts it)", t)
    t.type("security"); t.enter()
    t.wait_for("Change object owner")
    check("SECURITY" in t.line(1), "the prompted GO shows the Security menu", t)
    t.fkey(3); t.wait_for("Command Entry")
    check(True, "F3 leaves the menus and returns to Command Entry", t)

    # A user menu: CRTMNU TYPE(*PGM), the program shows the menu.
    command(t, "crtmnu menulib/orders type(*pgm) pgm(qgpl/ordentc) text('Order entry')",
            "Menu ORDERS created in library MENULIB.")
    check(True, "CRTMNU TYPE(*PGM) creates a menu", t)
    command(t, "go menulib/orders", "Customer number", timeout=90)
    check(True, "GO of a *PGM menu calls its program", t)
    t.fkey(3); t.wait_for("next order number", 60)
    check("Command Entry" in t.text(), "when the menu program ends, the display the GO came from returns", t)
    command(t, "crtmnu menulib/plain", "not supported yet")
    t.fkey(12)

    # Initial program and initial menu.
    command(t, "crtusrprf carl password(Menus123) inlpgm(qgpl/ordentc) inlmnu(*signoff)",
            "User profile CARL created.")
    command(t, "dspusrprf carl", "Display User Profile")
    # IBM i 7.5's DSPUSRPRF (PUB400): the program, its library on the next
    # line ("Initial program ... SETLANG", "Library ... PUB400SYS").
    check("ORDENTC" in t.text() and "QGPL" in t.text() and "*SIGNOFF" in t.text(),
          "DSPUSRPRF shows the initial program and the initial menu", t)
    t.fkey(3); t.wait_for("Command Entry")
    with Tn5250(HOST) as t2:
        t2.wait_for("Sign On")
        t2.type("carl"); t2.tab(); t2.type("menus123"); t2.enter()
        t2.wait_for("Customer number", 90)
        check(True, "sign-on runs the initial program (INLPGM)", t2)
        t2.fkey(3)
        t2.wait_for("Sign On", 60)
        check("CARL signed off" in t2.line(24), "INLMNU(*SIGNOFF): the user is signed off when the initial program ends", t2)
        command(t, "chgusrprf carl inlpgm(*none) inlmnu(security)", "User profile CARL changed.")
        t2.type("carl"); t2.tab(); t2.type("menus123"); t2.enter()
        t2.wait_for("Change object owner")
        check("SECURITY" in t2.line(1), "CHGUSRPRF INLMNU: the next sign-on shows that menu", t2)
        t2.fkey(3)
        t2.wait_for("Sign On")
        check(True, "F3 on the initial menu signs off", t2)
        t2.type("carl"); t2.tab(); t2.type("menus123"); t2.enter()
        t2.wait_for("Change object owner")
        command(t2, "90", "Sign On")
        check(True, "option 90 signs off", t2)
    command(t, "dltmnu menulib/orders", "Menu ORDERS deleted from library MENULIB.")
    command(t, "go menulib/orders", "not found")
    check(True, "DLTMNU deletes the menu; GO then says it is not found", t)
    t.fkey(12)
    command(t, "dltusrprf carl", "User profile CARL deleted.")

    # An imported application (scripts/samples.sh: as4tainr import
    # samples/apps/contacts --lib CONTACTS). Expected behavior from IBM's
    # RPG reference: READ sets its positions 75-76 indicator at end of
    # file; CHAIN sets positions 71-72 when no record is found; CA03
    # without a response indicator sets *INKC.
    command(t, "addlible contacts", "Library CONTACTS added")
    command(t, "call contacts", "Contact List", timeout=90)
    screen = t.text()
    check("5  contacts" in screen and "Grace Hopper" in screen and "Ken Thompson" in screen,
          "an imported CL program calls an imported RPG subfile program (READ loop ends at end of file)", t)
    t.fkey(3)
    t.wait_for("Contact list ended (CONTACTS).", 60)
    check("Command Entry" in t.text(), "F3 sets *INKC and ends the program; the CL INCLUDE declared &APP", t)
    command(t, "dspfd contacts/qrpglesrc type(*mbrlist)", "Display File Description")
    check("CONTLST" in t.text() and "List contacts" in t.text() and "RPGLE" in t.text(),
          "DSPFD lists a source file's members, types and texts", t)
    t.fkey(3); t.wait_for("Command Entry")
    command(t, "dsppfm contacts/qrpglesrc mbr(contcnt)", "Display Physical File Member")
    check("1.00" in t.text() and "CONTCNT" in t.text(), "DSPPFM shows a source member's lines", t)
    t.fkey(3); t.wait_for("Command Entry")
    command(t, "crtbndrpg contacts/contcnt", "Program CONTCNT was created in library CONTACTS.")
    check(True, "CRTBNDRPG creates a program from QRPGLESRC (SRCMBR(*PGM))", t)
    command(t, "call contacts/contcnt", "DSPLY  Barbara Liskov", timeout=90)
    check("DSPLY  5" in t.text() and "DSPLY  Grace Hopper" in t.text(),
          "the RPG program object runs: READ to end of file, CHAIN found and not found", t)
    command(t, "rmvlible contacts", "removed from library list")
    command(t, "signoff", "Sign On")

print(f"\nAll {passed} checks passed.")
