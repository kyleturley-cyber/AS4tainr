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
# - RPGUnit (CPL-1.0): its members copied into the source files of
#   library RPGUNIT (CPYFRMSTMF), built by its own MKMETADATA and
#   MKRPGUNIT, then its self-test MKRPGUNITT in a batch job: the sanity
#   check (TESTPGM03 must fail), the unit test suites (MKUT), the
#   acceptance tests.
# - QshOni (MIT): built by its own build.sh (bash, its `system` calls run
#   by as4tainr's `system`) from a clone put in the IFS at the same path,
#   then QSHEXEC runs a shell command (Qshell needs AS4TAINR_QSH=1) and
#   its output is in the outfile.
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
clone https://github.com/RPGUnit/RPGUnit.git rpgunit
clone https://github.com/richardschoen/QshOni.git qshoni

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

step "RPGUnit (README: install, then the self-test)"
"$bin" put "$work/rpgunit" /home/rpgunit >/dev/null
run "CRTLIB RPGUNIT" | tail -1
for d in "$work"/rpgunit/*.FILE; do
  f=$(basename "$d" .FILE)
  run "CRTSRCPF FILE(RPGUNIT/$f) RCDLEN(112)" >/dev/null
  for m in "$d"/*.MBR; do
    n=$(basename "$m" .MBR)
    run "CPYFRMSTMF FROMSTMF('/home/rpgunit/$f.FILE/$n.MBR') TOMBR('/QSYS.LIB/RPGUNIT.LIB/$f.FILE/$n.MBR') MBROPT(*REPLACE)" >/dev/null
  done
done
run "CRTBNDCL PGM(RPGUNIT/MKMETADATA) SRCFILE(RPGUNIT/RPGUNIT1)" | tail -1
"$bin" --libl RPGUNIT system "CALL RPGUNIT/MKMETADATA PARM(RPGUNIT)" | tail -1
run "CRTBNDCL PGM(RPGUNIT/MKRPGUNIT) SRCFILE(RPGUNIT/RPGUNIT1)" | tail -1
"$bin" --libl RPGUNIT system "CALL RPGUNIT/MKRPGUNIT PARM(RPGUNIT)" | tail -1
# The fixtures EXTPRCT reads exist already where RPGUnit is developed;
# its scripts do not build them.
for n in SRVPGM1 SRVPGM2; do
  "$bin" --libl RPGUNIT system "CRTRPGMOD MODULE(RPGUNIT/$n) SRCFILE(RPGUNIT/RPGUNITF1)" >/dev/null
  "$bin" --libl RPGUNIT system "CRTSRVPGM SRVPGM(RPGUNIT/$n) EXPORT(*ALL)" | tail -1
done
run "CRTBNDCL PGM(RPGUNIT/MKRPGUNITT) SRCFILE(RPGUNIT/RPGUNITT1)" | tail -1
# The self-test in a batch job (`as4tainr batch`, as SBMJOB and QBATCH
# would run it): its acceptance tests read RPGUnit's report from the
# spooled file, and PASE `system` keeps no spooled files. Each suite's
# result line (Success. / FAILURE. / ERROR.), and how the job ended.
"$bin" --libl RPGUNIT batch "CALL RPGUNIT/MKRPGUNITT PARM(RPGUNIT)" 2>&1 |
  grep -E "^(Success|FAILURE|ERROR)\.|CPF124[01]"

step "QshOni (build.sh, then QSHEXEC)"
shim="$work/shim"
mkdir -p "$shim"
printf '#!/bin/sh\nexec "%s" system "$@"\n' "$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")" > "$shim/system"
chmod +x "$shim/system"
"$bin" put "$work/qshoni" "$work/qshoni" >/dev/null
(cd "$work/qshoni" && PATH="$shim:$PATH" bash build.sh 2>&1) |
  grep -E "^(CPF|Error in)|was created and programs compiled" | grep -v CPF0006
AS4TAINR_QSH=1 "$bin" --libl QSHONI system "QSHEXEC CMDLINE('echo QshOni works') PRTOUTQ(*SAME) OUTFILE(QTEMP/STDOUTQSH) LOGSTDOUT(*NO)" | grep -E "QSH0005|CPF"
