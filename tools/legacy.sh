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
# - IBM's Company System (Apache-2.0): built as its makefile does (GNU
#   Make: `liblist -c` and `system` from a shell in the clone, which is
#   in the IFS at the same path, so relative SRCSTMF paths work), each
#   step run here with the current library and library list liblist
#   sets; then MYPGM (printf and DSPLY).
# - iRPGUnit (CPL-1.0): its members copied into the source files of
#   library IRPGUNIT (CPYFRMSTMF; RPGUnit has RPGUNIT), built by its own
#   REXX procedure A_INSTALL (STRREXPRC, as its README says) into that
#   library; then its self-test suite
#   ASSERTT made with RUCRTRPG and run with RUCALLTST: the test
#   procedures that pass here, then the whole suite's result line.
# - IBMi-topics (MarcoDeSenas, GPL-3.0): imported (a TOBi-like project:
#   sources built from the IFS, relative INCLUDE SRCSTMF paths, commands
#   with validity checkers that look for QCATRS on the call stack,
#   RTNVAL parameters), then its SPLFDLT command, which lists the user's
#   spooled files with QUSLSPL SPLF0300 in a user space and deletes them.
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
clone https://github.com/IBM/ibmi-company_system.git cmpsys
clone https://github.com/tools-400/irpgunit.git irpgunit
clone https://github.com/MarcoDeSenas/IBMi-topics-thanks-to-pub400.git topics

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

step "Company System (makefile: BIN_LIB=CMPSYS)"
"$bin" put "$work/cmpsys" "$work/cmpsys" >/dev/null
(
  cd "$work/cmpsys" || exit 1
  export AS4TAINR_CURLIB=CMPSYS AS4TAINR_LIBL=CMPSYS,QGPL,QTEMP
  b="$(cd "$OLDPWD" && cd "$(dirname "$bin")" && pwd)/$(basename "$bin")"
  s() { "$b" system "$1" 2>&1 | grep -E "^(CPF|CPD|RNS9310|SQL[0-9])|created in|with errors" | grep -v "^ "; }
  s "CRTLIB LIB(CMPSYS)"
  s "RUNSQLSTM SRCSTMF('qsqlsrc/employee.table') COMMIT(*NONE)"
  s "RUNSQLSTM SRCSTMF('qsqlsrc/department.table') COMMIT(*NONE)"
  s "CRTSQLRPGI OBJ(CMPSYS/EMPDET) SRCSTMF('qrpglesrc/empdet.sqlrpgle') COMMIT(*NONE) DBGVIEW(*SOURCE) COMPILEOPT('TGTCCSID(*JOB)') RPGPPOPT(*LVL2) OPTION(*EVENTF) OBJTYPE(*MODULE)"
  s "CRTBNDDIR BNDDIR(CMPSYS/APP)"
  s "CRTSRVPGM SRVPGM(CMPSYS/EMPDET) MODULE(EMPDET) SRCSTMF('qrpglesrc/empdet.bnd') BNDDIR((CMPSYS/APP)) REPLACE(*YES)"
  s "ADDBNDDIRE BNDDIR(CMPSYS/APP) OBJ((*LIBL/EMPDET *SRVPGM *IMMED))"
  s "CRTSRCPF FILE(CMPSYS/QTMPSRC) RCDLEN(112) CCSID(*JOB)"
  for d in depts emps nemp; do
    u=$(echo "$d" | tr a-z A-Z)
    s "CPYFRMSTMF FROMSTMF('qddssrc/$d.dspf') TOMBR('/QSYS.LIB/CMPSYS.LIB/QTMPSRC.FILE/$u.MBR') MBROPT(*REPLACE)" >/dev/null
    s "CRTDSPF FILE(CMPSYS/$u) SRCFILE(CMPSYS/QTMPSRC) SRCMBR($u) OPTION(*EVENTF)"
  done
  for p in employees newemp depts; do
    u=$(echo "$p" | tr a-z A-Z)
    s "CRTSQLRPGI OBJ(CMPSYS/$u) SRCSTMF('qrpglesrc/$p.pgm.sqlrpgle') COMMIT(*NONE) DBGVIEW(*SOURCE) OPTION(*EVENTF) RPGPPOPT(*LVL2) COMPILEOPT('TGTCCSID(*JOB) BNDDIR(APP) DFTACTGRP(*no)')"
  done
  s "CRTBNDRPG PGM(CMPSYS/MYPGM) SRCSTMF('qrpglesrc/mypgm.pgm.rpgle') OPTION(*EVENTF) DBGVIEW(*SOURCE) TGTRLS(*CURRENT) TGTCCSID(*JOB) BNDDIR(APP) DFTACTGRP(*NO)"
  s "RUNSQLSTM SRCSTMF('qsqlsrc/popdept.sqlprc') COMMIT(*NONE)"
  s "RUNSQLSTM SRCSTMF('qsqlsrc/popemp.sqlprc') COMMIT(*NONE)"
  "$b" system "CALL CMPSYS/MYPGM"
)

step "iRPGUnit (STRREXPRC A_INSTALL, then RUCRTRPG and RUCALLTST of ASSERTT)"
src="$work/irpgunit/host/iRPGUnit"
"$bin" put "$src" /home/irpgunit >/dev/null
run "CRTLIB IRPGUNIT" >/dev/null
for d in "$src"/Q*; do
  f=$(basename "$d")
  run "CRTSRCPF FILE(IRPGUNIT/$f) RCDLEN(112)" >/dev/null
  for m in "$d"/*; do
    b=$(basename "$m"); n="${b%%.*}"; t="${b##*.}"
    [ ${#n} -le 10 ] || continue
    run "CPYFRMSTMF FROMSTMF('/home/irpgunit/$f/$b') TOMBR('/QSYS.LIB/IRPGUNIT.LIB/$f.FILE/$n.MBR') MBROPT(*REPLACE)" >/dev/null
    run "CHGPFM FILE(IRPGUNIT/$f) MBR($n) SRCTYPE($t)" >/dev/null
  done
done
irpg() { "$bin" --libl IRPGUNIT,QGPL,QTEMP system "$1" 2>&1; }
irpg "STRREXPRC SRCMBR(A_INSTALL) SRCFILE(IRPGUNIT/QBUILD) PARM('INSTALL IRPGUNIT')" | grep -E "^(CPF|REX)" | grep -v CPF2105
# As its A_SELFTEST does: the modules again (CMPMOD), then the suite.
irpg "STRREXPRC SRCMBR(A_INSTALL) SRCFILE(IRPGUNIT/QBUILD) PARM('CMPMOD IRPGUNIT')" | grep -E "^(CPF|REX)" | grep -v CPF2105
irpg "RUCRTRPG TSTPGM(IRPGUNIT/ASSERTT) SRCFILE(IRPGUNIT/QUNITTEST) BNDDIR(IRPGUNIT/IRPGUNIT) BOPTION(*DUPPROC *DUPVAR)" | grep -a -E "Test program|CPF9897"
irpg "RUCALLTST TSTPGM(IRPGUNIT/ASSERTT) TSTPRC(TESTASSERTCLEAR TESTASSERTCOUNT TESTASSERTEQUAL_DATE TESTASSERTEQUAL_IND TESTASSERTEQUAL_INTEGER TESTASSERTEQUAL_STRING TESTASSERTEQUAL_TIME TESTASSERTEQUAL_TIMESTAMP TESTASSERTWITHFAILURE TESTASSERTWITHSUCCESS TESTBIGINTEGEREQUALITY TESTCLEARASSERTFAILEVENT TESTGETASSERTFAILEVT TESTGETLOGVALUE TESTGOODBYEISNOTHELLO TESTHELLOEQUALSHELLO TESTTOIND TESTTWOANDTWOEQUALSFOUR TESTTWOANDTWOISNOTEQUALTOFIVE)" |
  grep -E "^ *(Success|FAILURE|ERROR)\."
irpg "RUCALLTST TSTPGM(IRPGUNIT/ASSERTT)" | grep -a -E "^ *(Success|FAILURE|ERROR)\."

step "IBMi-topics (import, then SPLFDLT *LOGONLY and *BOTH in batch jobs)"
"$bin" import "$work/topics" --lib TOPICS 2>/dev/null | tail -1
"$bin" batch "DSPLIBL OUTPUT(*PRINT)" >/dev/null 2>&1
topics() { "$bin" --libl TOPICS,QGPL --curlib TOPICS batch "$1" 2>&1 | grep -E "spool files|USP0|CPF1240"; }
topics "SPLFDLT ACTION(*LOGONLY)"
topics "SPLFDLT"
