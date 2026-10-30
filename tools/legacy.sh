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
#   sets; then MYPGM (printf and DSPLY); after iRPGUnit is installed, its
#   unit test (qtestsrc) made with RUCRTRPG and run with RUCALLTST.
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
# - iMoq (MIT): its BUILD program's steps (the repository's files copied
#   into source members with CPYFRMSTMF instead of BUILD's Qshell loop,
#   which would run its shell code on the host), its installer IMOQINST,
#   then its self-tests IMOQTEST, its demo IMOQDEMO and its examples
#   EXAMPLES in batch jobs.
# - IBMiUnit (GPL-3.0): its build script's steps with -s IBMIUNIT -t (the
#   source copied into IBMIUNIT's source files, the test programs' /copy
#   lines rewritten as its sed does), on the OSSILE pieces its README
#   assumes (its own copies of OSSILE's MESSAGE and ARRAYLIST service
#   programs, binding directories MESSAGE, ARRAYLIST and OSSILE/OSSILE,
#   OSSILE's message_h.rpgle in /QIBM/UserData); then its test suites
#   through RUNTEST UI(*DSPLY) in batch jobs.
# - OSSILE (MIT): imported, then its arraylist and linked list unit tests
#   (RPGUnit test service programs: ALIST01 made with RUCRTTST, LLIST_UT_1
#   as its Makefile makes it) run with RUCALLTST in batch jobs.
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
clone https://github.com/OSSILE/OSSILE.git ossile
clone https://github.com/danlong005/IBMiMock.git imoq
clone https://github.com/MarinaSchwenk/IBMiUnit.git ibmiunit

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
"$bin" system "RUNSQLSTM SRCSTMF('/home/northwind/create.sql') COMMIT(*NONE) NAMING(*SQL) ERRLVL(30)" 2>&1 | grep -E "level severity errors|^SQL9010" | tail -1
"$bin" system "RUNSQLSTM SRCSTMF('/home/northwind/insert.sql') COMMIT(*NONE) NAMING(*SQL) ERRLVL(30)" 2>&1 | grep -E "level severity errors|^SQL9010" | tail -1

step "Northwind application (README: Install)"
"$bin" put "$work/nwapp" /home/me/northwind-ibmi
"$bin" system "RUNSQLSTM SRCSTMF('/home/me/northwind-ibmi/qsqlsrc/nwidrst.sql') COMMIT(*NONE)" 2>&1 | grep -E "level severity errors|^SQL9010" | tail -1
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
"$bin" --libl COM022LIB system "RUNSQLSTM SRCFILE(COM022LIB/QSQLSRC) SRCMBR(ORDAUDJV) COMMIT(*NONE)" 2>&1 | grep -E "level severity errors|^SQL9010" | tail -1
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

step "OSSILE (import, then the arraylist and linked list unit tests with RPGUnit)"
"$bin" import "$work/ossile" --lib OSSILE | tail -1
"$bin" put "$work/ossile" /home/ossile >/dev/null
oss() { "$bin" --libl OSSILE,RPGUNIT,QGPL system "$1" 2>&1 | grep -E "^(CP[CDF]|RNF|RNS)" | tail -1; }
# The unit tests call procedures the libraries' binder sources do not
# export (arraylist_sublist; llist.bnd has two PGMLVL(*CURRENT) blocks),
# so they are bound to service programs that export every procedure.
oss "CRTSRVPGM SRVPGM(OSSILE/ALISTALL) MODULE(OSSILE/ARRAYLIST) EXPORT(*ALL)"
oss "CRTSRVPGM SRVPGM(OSSILE/LLISTALL) MODULE(OSSILE/LLIST OSSILE/LLIST_SORT) EXPORT(*ALL)"
# ALIST01 copies the prototypes as member ALIST_H; RUCRTTST compiles it.
oss "CPYFRMSTMF FROMSTMF('/home/ossile/main/arraylist/arraylist_h.rpgle') TOMBR('/QSYS.LIB/OSSILE.LIB/QRPGLESRC.FILE/ALIST_H.MBR') MBROPT(*REPLACE)"
oss "CPYFRMSTMF FROMSTMF('/home/ossile/main/arraylist/unittests/alist01.rpgle') TOMBR('/QSYS.LIB/OSSILE.LIB/QRPGLESRC.FILE/ALIST01.MBR') MBROPT(*REPLACE)"
"$bin" --libl OSSILE,RPGUNIT,QGPL system "RUCRTTST TSTPGM(OSSILE/ALIST01) SRCFILE(OSSILE/QRPGLESRC) BNDSRVPGM(OSSILE/ALISTALL)" 2>&1 | grep -E "^(CPF|RNF)"
# LLIST_UT_1 as its Makefile builds it (llist_h.rpgle from INCDIR).
oss "CRTRPGMOD MODULE(OSSILE/LLIST_UT_1) SRCSTMF('/home/ossile/main/linkedlist/unittest/llist_ut_1.rpgle') INCDIR('/home/ossile/main/linkedlist')"
oss "CRTSRVPGM SRVPGM(OSSILE/LLIST_UT_1) EXPORT(*ALL) BNDSRVPGM(RUTESTCASE LLISTALL)"
for t in ALIST01 LLIST_UT_1; do
  "$bin" --libl OSSILE,RPGUNIT,QGPL batch "RPGUNIT/RUCALLTST TSTPGM(OSSILE/$t)" 2>&1 |
    grep -E "^(Success|FAILURE|ERROR)\.|CPF124[01]"
done

step "iMoq (BUILD's steps: members, IMOQINST; then IMOQTEST, IMOQDEMO and EXAMPLES)"
"$bin" put "$work/imoq" /home/imoq >/dev/null
run "CRTLIB LIB(IMOQ)" | tail -1
for f in QRPGLESRC QCLLESRC QCMDSRC QSRVSRC; do run "CRTSRCPF FILE(IMOQ/$f) RCDLEN(112)" >/dev/null; done
for d in QRPGLESRC QCLLESRC QCMDSRC QSRVSRC examples/QRPGLESRC examples/QCLLESRC examples/QSRVSRC; do
  s=$(basename $d)
  for p in "$work"/imoq/$d/*.*; do
    [ -f "$p" ] || continue
    b=$(basename "$p"); m=$(echo "${b%.*}" | tr a-z A-Z); t=$(echo "${b##*.}" | tr a-z A-Z)
    run "ADDPFM FILE(IMOQ/$s) MBR($m) SRCTYPE($t)" >/dev/null
    run "CPYFRMSTMF FROMSTMF('/home/imoq/$d/$b') TOMBR('/QSYS.LIB/IMOQ.LIB/$s.FILE/$m.MBR') MBROPT(*REPLACE) STMFCCSID(1208) DBFCCSID(*FILE)" >/dev/null
    run "CHGPFM FILE(IMOQ/$s) MBR($m) SRCTYPE($t)" >/dev/null
  done
done
run "CRTBNDCL PGM(IMOQ/IMOQINST) SRCFILE(IMOQ/QCLLESRC) SRCMBR(IMOQINST)" | grep -E "^CP[CDF]" | tail -1
"$bin" --libl IMOQ,QGPL batch "CALL PGM(IMOQ/IMOQINST) PARM(IMOQ IMOQ)" 2>&1 | grep -E "^CPF9897|CPF124[01]"
for t in IMOQTEST IMOQDEMO EXAMPLES; do
  run "CRTBNDCL PGM(IMOQ/$t) SRCFILE(IMOQ/QCLLESRC) SRCMBR($t)" | grep -E "^CPF" | tail -1
  parm="PARM(IMOQ IMOQ)"; [ $t = EXAMPLES ] && parm="PARM(IMOQ)"
  "$bin" --libl QGPL,QTEMP,IMOQ batch "CALL PGM(IMOQ/$t) $parm" 2>&1 |
    grep -a -E "^CPF989[78]: ([0-9]+ passed|FAIL|EX[A-Z]+ passed|IMOQTEST|IMOQDEMO|EXAMPLES)|CPF124[01]" | cut -c1-160
done

step "IBMiUnit (build -s IBMIUNIT -t as CL commands; then its test suites)"
iu() { "$bin" --libl IBMIUNIT,OSSILE,QGPL --curlib IBMIUNIT system "$1" 2>&1; }
iuwork=$(mktemp -d)
mkdir -p "$iuwork/test"
for f in "$work"/ibmiunit/src/test/QRPGLESRC/*.rpgle; do
  python3 -I -c 'import re,sys; t=open(sys.argv[1],encoding="utf-8",errors="replace").read(); print(re.sub(r"/copy \.\./\.\./main/QRPGLESRC/(.*)_h\.rpgle", r"/copy IBMIUNIT/QRPGLESRC,\1_h", t), end="")' "$f" > "$iuwork/test/$(basename "$f")"
done
"$bin" put "$work/ibmiunit/src/main" /home/ibmiunit/main >/dev/null
"$bin" put "$iuwork/test" /home/ibmiunit/test >/dev/null
"$bin" put "$work/ossile/main/message" /QIBM/UserData/OSSILE-master/main/message >/dev/null
# TEST_SETUP copies it from its author's own directory.
"$bin" put "$work/ossile/main/message" /QIBM/UserData/Stevers/OSSILE-master/main/message >/dev/null
iu "CRTLIB LIB(IBMIUNIT)" | tail -1
iu "CRTLIB LIB(OSSILE)" >/dev/null
for f in QCMDSRC QSRVSRC; do iu "CRTSRCPF FILE(IBMIUNIT/$f)" >/dev/null; done
iu "CRTSRCPF FILE(IBMIUNIT/QRPGLESRC) RCDLEN(112)" >/dev/null
iucp() { # file member stmf type
  iu "CPYFRMSTMF FROMSTMF('$3') TOMBR('/QSYS.LIB/IBMIUNIT.LIB/$1.FILE/$2.MBR') MBROPT(*REPLACE) STMFCCSID(*PCASCII) DBFCCSID(37)" | grep -v "^CPCA081"
  iu "CHGPFM FILE(IBMIUNIT/$1) MBR($2) SRCTYPE($4)" | grep -v "^CPC3201"
}
iucp QCMDSRC RUNTEST /home/ibmiunit/main/QCMDSRC/RUNTEST.cmd CMD
for p in "$work"/ibmiunit/src/main/QRPGLESRC/* "$work"/ibmiunit/src/main/QSRVSRC/* "$iuwork"/test/*.rpgle; do
  b=$(basename "$p"); m=$(echo "${b%.*}" | tr a-z A-Z)
  case "$p" in
    */QSRVSRC/*) iucp QSRVSRC "$m" "/home/ibmiunit/main/QSRVSRC/$b" BND ;;
    "$iuwork"/*) iucp QRPGLESRC "$m" "/home/ibmiunit/test/$b" RPGLE ;;
    *) iucp QRPGLESRC "$m" "/home/ibmiunit/main/QRPGLESRC/$b" RPGLE ;;
  esac
done
rm -rf "$iuwork"
for m in MESSAGE ARRAYLIST; do
  iu "CRTRPGMOD MODULE(IBMIUNIT/$m) SRCFILE(IBMIUNIT/QRPGLESRC)" | grep -E "^RN[SF]" | tail -1
  iu "CRTSRVPGM SRVPGM(IBMIUNIT/$m) MODULE(IBMIUNIT/$m) EXPORT(*SRCFILE) SRCFILE(IBMIUNIT/QSRVSRC) SRCMBR($m)" | tail -1
  iu "CRTBNDDIR BNDDIR(IBMIUNIT/$m)" >/dev/null
  iu "ADDBNDDIRE BNDDIR(IBMIUNIT/$m) OBJ((IBMIUNIT/$m))" >/dev/null
done
iu "CRTBNDDIR BNDDIR(OSSILE/OSSILE)" >/dev/null
iu "ADDBNDDIRE BNDDIR(OSSILE/OSSILE) OBJ((IBMIUNIT/MESSAGE) (IBMIUNIT/ARRAYLIST))" >/dev/null
iu "CRTMSGF MSGF(IBMIUNIT/IBMIUNIT) TEXT('IBMiUnit messages')" | tail -1
grep -o 'ADDMSGD [^"]*' "$work/ibmiunit/src/main/crtmsgf" | sed 's/\$TGTLIB/IBMIUNIT/g; s/\$BUILD/test/g' |
  while read -r c; do iu "$c" >/dev/null; done
iu "CRTBNDDIR BNDDIR(IBMIUNIT/IBMIUNIT) TEXT('IBMiUnit binding directory')" >/dev/null
iu "ADDBNDDIRE BNDDIR(IBMIUNIT/IBMIUNIT) OBJ((IBMIUNIT/IBMIUNIT))" >/dev/null
iu "CRTRPGMOD MODULE(IBMIUNIT/IBMIUNIT) SRCFILE(IBMIUNIT/QRPGLESRC) TGTRLS(V7R1M0) DBGVIEW(*SOURCE)" | grep -E "^RN[SF]" | tail -1
iu "CRTSRVPGM SRVPGM(IBMIUNIT/IBMIUNIT) SRCFILE(IBMIUNIT/QSRVSRC) TEXT('IBMiUnit testing framework') TGTRLS(V7R1M0) USRPRF(*USER)" | tail -1
iu "CRTCMD CMD(IBMIUNIT/RUNTEST) PGM(IBMIUNIT/RUNTEST) SRCFILE(IBMIUNIT/QCMDSRC)" | grep "^CP" | tail -1
iuopt="TGTRLS(V7R1M0) DBGVIEW(*SOURCE) USRPRF(*USER)"
iu "CRTBNDRPG PGM(IBMIUNIT/RUNTEST) SRCFILE(IBMIUNIT/QRPGLESRC) BNDDIR(IBMIUNIT/IBMIUNIT) DFTACTGRP(*NO) $iuopt" | grep -E "^RN[SF]" | tail -1
for p in IBMIUIDSP IBMIUIINT; do
  iu "CRTBNDRPG PGM(IBMIUNIT/$p) SRCFILE(IBMIUNIT/QRPGLESRC) $iuopt" | grep -E "^RN[SF]|^CPF" | tail -1
done
iutests="TEST TEST_ACHAR TEST_ADATE TEST_AFLOA TEST_AIND TEST_ANULL TEST_ANUM TEST_ASSER TEST_ATIME TEST_ATS TEST_ERROR TEST_FAIL TEST_NUM0 TEST_NUM1 TEST_NUM2 TEST_NUM3 TEST_NUM4 TEST_NUM5 TEST_NUM6 TEST_SETUP"
for t in $iutests; do
  iu "CRTBNDRPG PGM(IBMIUNIT/$t) SRCFILE(IBMIUNIT/QRPGLESRC) BNDDIR(IBMIUNIT/IBMIUNIT) DFTACTGRP(*NO) $iuopt" | grep -E "^RNS9310|^CPF" | tail -1
done
for t in $iutests; do
  echo "$t: $("$bin" --libl IBMIUNIT,OSSILE,QGPL --curlib IBMIUNIT batch "RUNTEST SUITE(IBMIUNIT/$t) UI(*DSPLY)" 2>&1 |
    grep -E "DSPLY  Done:|CPF124[01]" | sed 's/^DSPLY  //; s/ Job [0-9]*\/.*//' | tr '\n' ' ')"
done

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
  # RUNSQLSTM: its listing's severity line (it sends no completion message).
  r() { "$b" system "$1" 2>&1 | grep -E "level severity errors|^SQL9010" | tail -1; }
  s() { "$b" system -s "$1" 2>&1 | grep -E "^(CPF|CPD|RNS9310|SQL[0-9])|created in|with errors" | grep -v "^ "; }
  s "CRTLIB LIB(CMPSYS)"
  r "RUNSQLSTM SRCSTMF('qsqlsrc/employee.table') COMMIT(*NONE)"
  r "RUNSQLSTM SRCSTMF('qsqlsrc/department.table') COMMIT(*NONE)"
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
  r "RUNSQLSTM SRCSTMF('qsqlsrc/popdept.sqlprc') COMMIT(*NONE)"
  r "RUNSQLSTM SRCSTMF('qsqlsrc/popemp.sqlprc') COMMIT(*NONE)"
  # `call`, not `system`: under PASE `system` a DSPLY line goes to
  # QSYSOPR, not to standard output (docs/fidelity-gaps.md).
  "$b" call CMPSYS/MYPGM
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
# RUCRTRPG says so with DSPLY, which goes to QSYSOPR under `system`.
irpg "CHKOBJ OBJ(IRPGUNIT/ASSERTT) OBJTYPE(*SRVPGM)" && echo "Test service program ASSERTT is in library IRPGUNIT."
irpg "RUCALLTST TSTPGM(IRPGUNIT/ASSERTT) TSTPRC(TESTASSERTCLEAR TESTASSERTCOUNT TESTASSERTEQUAL_DATE TESTASSERTEQUAL_IND TESTASSERTEQUAL_INTEGER TESTASSERTEQUAL_STRING TESTASSERTEQUAL_TIME TESTASSERTEQUAL_TIMESTAMP TESTASSERTWITHFAILURE TESTASSERTWITHSUCCESS TESTBIGINTEGEREQUALITY TESTCLEARASSERTFAILEVENT TESTGETASSERTFAILEVT TESTGETLOGVALUE TESTGOODBYEISNOTHELLO TESTHELLOEQUALSHELLO TESTTOIND TESTTWOANDTWOEQUALSFOUR TESTTWOANDTWOISNOTEQUALTOFIVE)" |
  grep -E "^ *(Success|FAILURE|ERROR)\."
irpg "RUCALLTST TSTPGM(IRPGUNIT/ASSERTT)" | grep -a -E "^ *(Success|FAILURE|ERROR)\."

step "Company System's unit test (qtestsrc: RUCRTRPG with testing.json's options, RUCALLTST)"
# EMPDET's test (SQLRPGLE from the IFS) mocks EMPLOYEE and DEPARTMENT
# with CRTDUPOBJ and OVRDBF OVRSCOPE(*JOB) in QTEMP; ctl-opt BNDDIR('APP')
# binds it to EMPDET.
cs() { "$bin" --libl CMPSYS,IRPGUNIT,QGPL --curlib CMPSYS batch "$1" 2>&1; }
cs "IRPGUNIT/RUCRTRPG TSTPGM(CMPSYS/EMPDETT) SRCSTMF('$work/cmpsys/qtestsrc/empdet.test.sqlrpgle') INCDIR('$work/cmpsys') RPGPPOPT(*LVL2) TGTCCSID(*JOB) DBGVIEW(*SOURCE)" |
  grep -a -E "^(Test program|CPF9897)"
cs "IRPGUNIT/RUCALLTST TSTPGM(CMPSYS/EMPDETT)" | grep -a -E "^ *(Success|FAILURE|ERROR)\.|CPF124[01]"

step "IBMi-topics (import, then SPLFDLT *LOGONLY and *BOTH in batch jobs)"
"$bin" import "$work/topics" --lib TOPICS 2>/dev/null | tail -1
"$bin" batch "DSPLIBL OUTPUT(*PRINT)" >/dev/null 2>&1
topics() { "$bin" --libl TOPICS,QGPL --curlib TOPICS batch "$1" 2>&1 | grep -E "spool files|USP0|CPF1240"; }
topics "SPLFDLT ACTION(*LOGONLY)"
topics "SPLFDLT"
