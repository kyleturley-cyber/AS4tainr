#!/usr/bin/env bash
# Install real IBM i applications the way their READMEs say, and report
# what works. The repositories are cloned at test time and never added to
# as4tainr (check their licenses before using them for anything else):
#
# - Northwind for IBM i (GPL-3.0): the NORTHWIND database from its
#   "Generate SQL" scripts (RUNSQLSTM SRCSTMF), then the application built
#   by its own CL program NWBUILD from the IFS (CPYFRMSTMF, CRTSQLRPGI,
#   CRTSRVPGM, CRTDSPF, CRTBNDRPG, CRTBNDCL).
# - COM order management (no license file: test use only), imported into
#   the library its SQL scripts name (COM022LIB), then its README's steps
#   (NXTORDNO, CRTCOMMSG, LOADDATA, COMBNDDIR, the programs bound to it).
#   Start it with CALL COM022LIB/COMSTART.
#
#   tools/legacy.sh
#
# Env: AS4TAINR_BIN (default target/debug/as4tainr), AS4TAINR_DATABASE_URL,
# LEGACY_DIR (where the clones go; default a new temporary directory).
set -uo pipefail
bin="${AS4TAINR_BIN:-target/debug/as4tainr}"
work="${LEGACY_DIR:-$(mktemp -d)}"
clone() {
  if [ ! -d "$work/$2/.git" ]; then
    git clone -q --depth 1 "$1" "$work/$2"
  fi
}
clone https://github.com/ZodandSons/northwind-db2-sql-database.git nwdb
clone https://github.com/ZodandSons/northwind-ibmi-rpgle-application.git nwapp
clone https://github.com/ARNAV2684/as400-order-management-modernization.git com

step() {
  echo "== $*"
}
run() {
  "$bin" system "$1"
}

"$bin" init >/dev/null

step "Northwind database"
"$bin" put "$work/nwdb/create-northwind-schema.sql" /home/northwind/create.sql
"$bin" put "$work/nwdb/insert-data-northwind-db2.sql" /home/northwind/insert.sql
run "RUNSQLSTM SRCSTMF('/home/northwind/create.sql') COMMIT(*NONE) NAMING(*SQL) ERRLVL(30)" | tail -1
run "RUNSQLSTM SRCSTMF('/home/northwind/insert.sql') COMMIT(*NONE) NAMING(*SQL) ERRLVL(30)" | tail -1

step "Northwind application (README: Install)"
"$bin" put "$work/nwapp" /home/me/northwind-ibmi
run "RUNSQLSTM SRCSTMF('/home/me/northwind-ibmi/qsqlsrc/nwidrst.sql') COMMIT(*NONE)" | tail -1
run "CRTLIB NWAPP"
run "CRTSRCPF NWAPP/QCLLESRC RCDLEN(92)"
run "CPYFRMSTMF FROMSTMF('/home/me/northwind-ibmi/qcllesrc/nwbuild.clle') TOMBR('/QSYS.LIB/NWAPP.LIB/QCLLESRC.FILE/NWBUILD.MBR') MBROPT(*REPLACE)"
run "CRTBNDCL NWAPP/NWBUILD SRCFILE(NWAPP/QCLLESRC)"
run "CALL NWAPP/NWBUILD PARM(('/home/me/northwind-ibmi' (*CHAR 128)) 'NWAPP' 'NORTHWIND')"

step "COM order management (README: Run it yourself)"
# The import creates the files, the SQL objects and what compiles on its
# own; then the README's order: NXTORDNO and CRTCOMMSG, LOADDATA, the
# binding directory for COMUTIL, the programs that bind through it.
"$bin" import "$work/com/ibmi" --lib COM022LIB
run "CRTDTAARA DTAARA(COM022LIB/NXTORDNO) TYPE(*DEC) LEN(7 0)"
run "CALL COM022LIB/CRTCOMMSG" | tail -1
"$bin" --libl COM022LIB system "CALL COM022LIB/LOADDATA" | tail -1
run "CRTBNDDIR BNDDIR(COM022LIB/COMBNDDIR)"
run "ADDBNDDIRE BNDDIR(COM022LIB/COMBNDDIR) OBJ((COM022LIB/COMUTIL *SRVPGM))"
for p in CUSTPGM ITEMPGM MENUPGM ORDPGM; do
  "$bin" --libl COM022LIB system "CRTBNDRPG PGM(COM022LIB/$p) SRCFILE(COM022LIB/QRPGLESRC)"
done
"$bin" --libl COM022LIB system "CRTSQLRPGI OBJ(COM022LIB/ORDINQ) SRCFILE(COM022LIB/QRPGLESRC)"
# Journaling and the audit trigger (README: SETUPJRN, ADDTRG), then an
# order changed: the trigger program ORDTRG writes ORDAUD, and the journal
# view ORDAUDJV shows the same change.
"$bin" --libl COM022LIB system "CALL COM022LIB/SETUPJRN" | tail -1
"$bin" --libl COM022LIB system "CALL COM022LIB/ADDTRG" | tail -1
"$bin" --libl COM022LIB system "RUNSQLSTM SRCFILE(COM022LIB/QSQLSRC) SRCMBR(ORDAUDJV) COMMIT(*NONE)" | tail -1
"$bin" --libl COM022LIB sql "UPDATE ORDHDR SET STATUS = STATUS WHERE ORDNO = 10001"
"$bin" --libl COM022LIB sql "SELECT ORDNO, ACTION FROM ORDAUD"
"$bin" --libl COM022LIB sql "SELECT ORDNO, ACTION, AUDSRC FROM ORDAUDJV"
