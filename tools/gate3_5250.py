#!/usr/bin/env python3
"""Phase 3 gate: a multi-user interactive and batch workload.

Three order-entry clerks, each in their own 5250 session, enter orders at
the same time with the order entry application (ORDENTC/ORDENT), on the
same customers and items, while QSECOFR's batch jobs print reports. When
everyone is done, the files must add up:

- every order has its own number (the data area ORDNXT is read and
  increased under IN *LOCK);
- each item's quantity on hand went down by exactly the quantities
  ordered, and each customer's balance went up by exactly their order
  totals (CHAIN and UPDATE under record locks: no update is lost);
- the batch jobs ended normally and printed their spooled files.

Expects a running `ibmish tn5250` with the sample objects, after
tools/e2e_5250.py (QSECOFR's password is then Green5creen!; override with
QSECOFR_PASSWORD). The files are checked with `ibmish sql` (IBMISH_BIN,
IBMISH_DATABASE_URL). Usage:

    TN5250=/path/to/tn5250 python3 tools/gate3_5250.py 127.0.0.1:2323
"""
import os
import subprocess
import sys
import threading
import time
from decimal import Decimal

sys.path.insert(0, os.path.dirname(__file__))
from tn5250_driver import Tn5250  # noqa: E402

HOST = sys.argv[1] if len(sys.argv) > 1 else "127.0.0.1:2323"
PW = os.environ.get("QSECOFR_PASSWORD", "Green5creen!")
BIN = os.environ.get("IBMISH_BIN", "target/debug/ibmish")
FE = "\x0b"  # Field Exit
DESCRIPTIONS = {"BOLT-M8": "Hex bolt", "NUT-M8": "Hex nut", "WIDGET-A": "Widget type A",
                "GASKET-12": "Gasket 12 in"}
PRICES = {"BOLT-M8": Decimal("0.15"), "NUT-M8": Decimal("0.05"), "WIDGET-A": Decimal("4.50"),
          "GASKET-12": Decimal("12.99")}

# Each clerk enters two orders: (customer, [(item, quantity), ...]).
CLERKS = {
    "CLERK1": [(1, [("BOLT-M8", 10), ("WIDGET-A", 2)]), (3, [("NUT-M8", 40)])],
    "CLERK2": [(1, [("BOLT-M8", 7), ("GASKET-12", 1)]), (2, [("WIDGET-A", 3), ("NUT-M8", 5)])],
    "CLERK3": [(3, [("BOLT-M8", 20)]), (1, [("GASKET-12", 2), ("WIDGET-A", 1)])],
}

passed = 0


def check(cond, what, detail=""):
    global passed
    if not cond:
        print(f"FAIL: {what}\n{detail}")
        sys.exit(1)
    passed += 1
    print(f"ok   {what}")


def sql(stmt):
    """Rows of an `ibmish sql` SELECT, as lists of strings."""
    out = subprocess.run([BIN, "sql", stmt], capture_output=True, text=True, check=True).stdout
    lines = [l for l in out.splitlines() if l.strip()]
    return [l.split() for l in lines[1:] if not l.endswith("selected.")]


def sign_on(t, user, password):
    t.wait_for("Sign On")
    t.type(user); t.tab(); t.type(password); t.enter()
    t.wait_for("ibmish Main Menu", 30)  # the initial menu
    command(t, "call qcmd", "Command Entry")


def command(t, cmd, expect, timeout=30):
    t.type(cmd)
    t.enter()
    return t.wait_for(expect, timeout)


def clerk(t, orders, errors, name):
    try:
        command(t, "call ordentc", "Type a customer number", timeout=120)
        for customer, lines in orders:
            t.type(str(customer)); t.send(FE); t.enter()
            t.wait_for("Order total", 60)
            for item, qty in lines:
                t.type(item); t.tab(); t.type(str(qty)); t.send(FE)
            t.enter()
            t.wait_for(DESCRIPTIONS[lines[-1][0]], 60)
            t.fkey(6)
            t.wait_for("created", 120)
        t.fkey(3)
        t.wait_for("next order number", 120)
    except Exception as e:  # noqa: BLE001 - reported by the main thread
        errors.append(f"{name}: {e}")


def main():
    before_items = {r[0]: int(r[1]) for r in sql("SELECT ITEM, ITMQOH FROM ITEMMAST")}
    before_cust = {int(r[0]): Decimal(r[1]) for r in sql("SELECT CUSNO, CUSBAL FROM CUSTMAST")}
    before_orders = {int(r[0]) for r in sql("SELECT ORDNO FROM ORDHDR")}

    with Tn5250(HOST) as sec:
        sign_on(sec, "qsecofr", PW)
        for user in CLERKS:
            # Left from an earlier run? Start clean.
            command(sec, f"dltusrprf {user}", f"> dltusrprf {user}")
            sec.pump(1)
            if f"User profile {user} deleted." not in sec.text() and "CPF2204" not in sec.text():
                sec.wait_for("CPF2204", 10)
            command(sec, f"crtusrprf {user} password(Order{user[-1]}x) text('Order clerk')", f"{user} created")
        check(True, "QSECOFR creates three clerk profiles")

        sessions = {}
        try:
            for user in CLERKS:
                t = Tn5250(HOST).__enter__()
                sessions[user] = t
                sign_on(t, user.lower(), f"order{user[-1]}x")
            check(True, "three clerks are signed on at the same time as QSECOFR")

            # Batch work while the clerks type.
            command(sec, "sbmjob cmd(call custprt) job(rpt1)", "submitted to job queue QBATCH")
            command(sec, "sbmjob cmd(dsppfm itemmast) job(stock)", "submitted to job queue QBATCH")
            command(sec, "sbmjob cmd(call custprt) job(rpt2)", "submitted to job queue QBATCH")

            errors = []
            threads = [threading.Thread(target=clerk, args=(sessions[u], CLERKS[u], errors, u))
                       for u in CLERKS]
            started = time.time()
            for th in threads:
                th.start()
            time.sleep(4)
            command(sec, "wrkactjob", "Work with Active Jobs")
            screen = sec.text()
            check(all(u in screen for u in CLERKS) and "QBATCH" in screen,
                  "WRKACTJOB shows the clerks' interactive jobs while they work", screen)
            sec.fkey(3); sec.wait_for("Command Entry")
            for th in threads:
                th.join(600)
            check(not errors and not any(th.is_alive() for th in threads),
                  f"each clerk entered two orders at the same time ({time.time() - started:.0f}s)",
                  "\n".join(errors))
        finally:
            for t in sessions.values():
                t.__exit__(None, None, None)

        # The batch jobs.
        command(sec, "wrksbmjob", "Work with Submitted Jobs")
        for _ in range(60):
            text = sec.text()
            if all(f"{j}" in text for j in ("RPT1", "STOCK", "RPT2")) and "JOBQ" not in text and "ACTIVE" not in text:
                break
            sec.fkey(5)
            sec.pump(2)
        text = sec.text()
        check("JOBQ" not in text and "ACTIVE" not in text and "abnormally" not in text,
              "the batch jobs ran while the clerks worked and ended normally", text)
        sec.fkey(3); sec.wait_for("Command Entry")
        command(sec, "wrksplf", "Work with All Spooled Files")
        for _ in range(3):
            if all(f"/{j}" in sec.text() for j in ("RPT1", "STOCK", "RPT2")):
                break
            sec.page_down(); sec.pump(1)
        check(all(f"/{j}" in sec.text() for j in ("RPT1", "STOCK", "RPT2")),
              "the batch jobs' reports and listing are spooled files (WRKSPLF)", sec.text())
        sec.fkey(3); sec.wait_for("Command Entry")
        command(sec, "signoff", "Sign On")

    # The files add up.
    orders = sql("SELECT ORDNO, CUSNO, ORDLNS, ORDTOT FROM ORDHDR")
    new = [o for o in orders if int(o[0]) not in before_orders]
    numbers = [int(o[0]) for o in new]
    check(len(new) == 6 and len(set(numbers)) == 6,
          f"six new orders with six different numbers {sorted(numbers)} (ORDNXT under IN *LOCK)", str(orders))
    check(sorted(numbers) == list(range(min(numbers), min(numbers) + 6)),
          "the order numbers have no gaps", str(numbers))
    lines = sql("SELECT ORDNO, ITEM, QTY, PRICE FROM ORDLIN")
    new_lines = [l for l in lines if int(l[0]) in numbers]
    check(len(new_lines) == sum(len(ls) for orders in CLERKS.values() for _, ls in orders),
          "every order line is in ORDLIN")

    ordered = {}
    for orders in CLERKS.values():
        for _, ls in orders:
            for item, qty in ls:
                ordered[item] = ordered.get(item, 0) + qty
    after_items = {r[0]: int(r[1]) for r in sql("SELECT ITEM, ITMQOH FROM ITEMMAST")}
    lost = {i: (before_items[i] - after_items[i], q) for i, q in ordered.items()
            if before_items[i] - after_items[i] != q}
    check(not lost, "quantity on hand went down by exactly what was ordered (no lost updates)", str(lost))

    expected_total = {}
    for orders in CLERKS.values():
        for cust, ls in orders:
            expected_total[cust] = expected_total.get(cust, Decimal(0)) + sum(PRICES[i] * q for i, q in ls)
    after_cust = {int(r[0]): Decimal(r[1]) for r in sql("SELECT CUSNO, CUSBAL FROM CUSTMAST")}
    wrong = {c: (after_cust[c] - before_cust[c], t) for c, t in expected_total.items()
             if after_cust[c] - before_cust[c] != t}
    check(not wrong, "each customer's balance went up by exactly their order totals", str(wrong))
    by_order = {int(o[0]): Decimal(o[3]) for o in new}
    check(sum(by_order.values()) == sum(expected_total.values()),
          f"the order totals add up to {sum(expected_total.values())}")
    print(f"\nPhase 3 gate: all {passed} checks passed.")


if __name__ == "__main__":
    main()
