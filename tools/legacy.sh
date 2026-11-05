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
# - TOBi example (IBM, Apache-2.0): imported (a TOBi project with DDS
#   reference files, ARCAD object scripts, SQL triggers); its unit tests
#   VAT300 and CUS300 made with RUCRTRPG and run with RUCALLTST.
# - IBM i Packager (JDubbTX, Apache-2.0): imported; its unit tests
#   (pkgtest.test.sqlrpgle: PACK and UNPACK of a test library) made with
#   RUCRTRPG as its Makefile's test target does and run with RUCALLTST.
# - IUNIT (i-unit/iunit, MIT): its build.sh's steps (source files and
#   members from the clone, CRTBNDCL and CALL of its I_BUILD), then its
#   README's tests RUNONE IUNIT PASS and RUNALL IUNIT in batch jobs (FAIL
#   is its example of a failing test: 3 run, 1 failed, the job ends
#   abnormally); the job log shows each test as I_RUNALL logs it.
# - CommonLogger (Damian-Pietron, Apache-2.0): its makefile's CL
#   commands (modules, service program, example program), then its unit
#   tests (comLog.test.sqlrpgle) made with RUCRTRPG and run with RUCALLTST.
# - IBM i Unit (ZodandSons VSCode-IBM-i-RPGLE-Unit-Tester-Extension,
#   GPL-3.0): the extension's framework install and sample build
#   (setup.ts's commands), then each sample suite as its runner.ts runs it
#   (CRTRPGMOD, CRTSRVPGM, CALL RUNTEST with the exported test names,
#   the results read from IUNITRES); demo_failures fails on purpose.
# - XMLSERVICE (IBM, BSD-3-Clause): its Makefile's build-procedure target
#   (configure's defaults: library XMLSERVICE; the modules, service
#   program XMLSTOREDP, the SQL procedures of xmlstoredp.sql) and its
#   build-test target's RPG programs, then a command through iPLUG4K and
#   the XML of its PHP tests (tools/xmlservice_tests.py: the tests read as
#   data, in stateless mode).
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
# LEGACY_DIR (where the clones go; default a new temporary directory),
# XMLSERVICE_TESTS=0 (XMLSERVICE's tests left out).
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
clone https://github.com/IBM/tobi-example.git tobi
clone https://github.com/JDubbTX/ibm-i-packager.git pkg
clone https://github.com/Damian-Pietron/CommonLogger.git clog
clone https://github.com/i-unit/iunit.git iunit
clone https://github.com/ZodandSons/VSCode-IBM-i-RPGLE-Unit-Tester-Extension.git zunit
clone https://github.com/IBM/xmlservice.git xmlservice

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

step "TOBi example (import, then its unit tests with iRPGUnit: RUCRTRPG with testing.json's options, RUCALLTST)"
# IBM's sample project for its build tools (Apache-2.0): DDS files that
# refer to a field reference file, binder sources, ARCAD object scripts
# (.ILEPGM, .ILESRVPGM, .BNDDIR), SQL triggers, sequences, printer files.
# Its tests (QTESTSRC) insert their own records, as VAT300 and CUS300's
# setUpSuite do, and call the service programs FVAT and FCUSTOMER.
"$bin" import "$work/tobi" --lib TOBI 2>/dev/null | tail -1
"$bin" put "$work/tobi" "$work/tobi" >/dev/null
tb() { "$bin" --libl TOBI,IRPGUNIT,QGPL --curlib TOBI batch "$1" 2>&1; }
for t in vat300 cus300; do
  u="$(echo "$t" | tr a-z A-Z)T"
  tb "IRPGUNIT/RUCRTRPG TSTPGM(TOBI/$u) SRCSTMF('$work/tobi/QTESTSRC/$t.test.sqlrpgle') INCDIR('$work/tobi') RPGPPOPT(*LVL2) TGTCCSID(*JOB) DBGVIEW(*SOURCE)" |
    grep -a -E "^(Test program|CPF9897)"
  tb "IRPGUNIT/RUCALLTST TSTPGM(TOBI/$u)" | grep -a -E "^ *(Success|FAILURE|ERROR)\.|CPF124[01]"
done

step "IBM i Packager (import, then its unit tests with iRPGUnit: RUCRTRPG as its Makefile's test target, RUCALLTST)"
# JDubbTX/ibm-i-packager (Apache-2.0): PACK and UNPACK commands that
# stamp objects with a version (QLICOBJD), save them to a save file or
# stream file, list it (QSRLSAVF) and restore it into libraries mapped
# by type (a JSON file read with JSON_TABLE), keeping authorities
# (GRTOBJAUT REFOBJ) and records (RNMOBJ, CPYF). Its test source binds
# RPGUNIT/IRPGUNIT: here RPGUnit has library RPGUNIT, so iRPGUnit's
# binding directory is duplicated there (its entries name IRPGUNIT).
# Its Makefile compiles in the clone (the current directory, where the
# test's include 'src/qrpglesrc/PKGTYPES.rpgleinc' is found): that
# directory is first in INCDIR here, then the Makefile's INC_DIR.
"$bin" import "$work/pkg" --lib PACKAGER 2>/dev/null | tail -1
"$bin" put "$work/pkg" "$work/pkg" >/dev/null
run "CRTDUPOBJ OBJ(IRPGUNIT) FROMLIB(IRPGUNIT) OBJTYPE(*BNDDIR) TOLIB(RPGUNIT)" | tail -1
pk() { "$bin" --libl PACKAGER,IRPGUNIT,QGPL --curlib PACKAGER batch "$1" 2>&1; }
pk "IRPGUNIT/RUCRTRPG TSTPGM(PACKAGER/TPKGTEST) SRCSTMF('$work/pkg/test/qrpglesrc/pkgtest.test.sqlrpgle') INCDIR('$work/pkg' '$work/pkg/src/qrpglesrc') RPGPPOPT(*LVL2) DBGVIEW(*SOURCE) COPTION(*EVENTF *SRCSTMT *NODEBUGIO)" |
  grep -a -E "^(Test program|CPF9897)"
pk "IRPGUNIT/RUCALLTST TSTPGM(PACKAGER/TPKGTEST) TSTPRC(*ALL) DETAIL(*ALL)" | grep -a -E "^ *(Success|FAILURE|ERROR)\.|CPF9897: FAILURE|CPF124[01]"

step "CommonLogger (its makefile's steps as CL commands, then its unit tests with iRPGUnit)"
# Damian-Pietron/CommonLogger (Apache-2.0): a logging service program
# (SQLRPGLE modules, a binder source, a log table it creates, the caller
# found with QSYS2.STACK_INFO). Its makefile runs `system` in Qshell: its
# commands are run here as they are, with the clone's absolute paths.
"$bin" put "$work/clog" "$work/clog" >/dev/null
cl() { "$bin" --libl COMLOG,IRPGUNIT,QGPL --curlib COMLOG system "$1" 2>&1 | grep -E "^(CP[CDF]|RNS|SQL[0-9])" | tail -1; }
cl "CRTLIB LIB(COMLOG) TYPE(*PROD) TEXT('Common Logger Library')"
cl "CRTBNDDIR BNDDIR(COMLOG/COMLOG) TEXT('Common Logger Binding Directory')"
cl "ADDBNDDIRE BNDDIR(COMLOG/COMLOG) OBJ((COMLOG/COMLOG))"
for m in comLog exLog; do
  cl "CRTSQLRPGI OBJ(COMLOG/$m) SRCSTMF('$work/clog/src/$m.sqlrpgle') OBJTYPE(*MODULE) REPLACE(*YES) DBGVIEW(*SOURCE) CVTCCSID(*JOB) COMPILEOPT('TGTCCSID(*JOB)') RPGPPOPT(*LVL2) TGTRLS(*PRV)"
done
cl "CRTSRVPGM SRVPGM(COMLOG/COMLOG) MODULE(COMLOG/COMLOG) SRCSTMF('$work/clog/src/comLog.BND') EXPORT(*SRCFILE) TGTRLS(*PRV) REPLACE(*YES)"
cl "CRTPGM PGM(COMLOG/EXLOG) MODULE(COMLOG/EXLOG) ENTMOD(COMLOG/EXLOG) ACTGRP(QILE) BNDSRVPGM(COMLOG/COMLOG) TGTRLS(*PRV) REPLACE(*YES)"
lg() { "$bin" --libl COMLOG,IRPGUNIT,QGPL --curlib COMLOG batch "$1" 2>&1; }
lg "IRPGUNIT/RUCRTRPG TSTPGM(COMLOG/COMLOGT) SRCSTMF('$work/clog/test/comLog.test.sqlrpgle') RPGPPOPT(*LVL2) DBGVIEW(*SOURCE)" |
  grep -a -E "^(Test program|CPF9897)"
lg "IRPGUNIT/RUCALLTST TSTPGM(COMLOG/COMLOGT) DETAIL(*ALL)" | grep -a -E "^ *(Success|FAILURE|ERROR)\.|CPF9897: FAILURE|CPF124[01]"
# The log: each test's message with the program and procedure that
# logged it (STACK_INFO's entry before COMLOG's).
"$bin" sql "SELECT LOGSEVERITY, CAST(LOGBODY AS VARCHAR(30)), LOGPROGRAM, LOGPROCEDURE FROM COMLOG.COMLOG WHERE LOGSEVERITY <> 50 ORDER BY LOGSEVERITY"

step "IUNIT (its build.sh's steps, then RUNONE and RUNALL as its README says)"
# i-unit/iunit (MIT): a CL unit test framework. build.sh (bash) makes a
# source file for each folder, copies each file to a member (its
# extension the source type) and calls I_BUILD, which compiles the rest;
# its `system` calls are run here as they are.
"$bin" put "$work/iunit" /home/iunit >/dev/null
iu1() { "$bin" --libl QGPL system "$1" 2>&1 | grep -E "^(CP[CDF])" | tail -1; }
iu1 "CRTLIB LIB(IUNIT) TEXT('IBM i Unit Testing Framework')"
for d in QCLLESRC QCMDSRC; do
  iu1 "CRTSRCPF FILE(IUNIT/$d) RCDLEN(240) TEXT('IBM i Unit Testing Framework $d Sources')" >/dev/null
  for f in "$work/iunit/$d"/*; do
    e=$(basename "$f"); atr=${e##*.}; mbr=${e%.*}
    iu1 "CPYFRMSTMF FROMSTMF('/home/iunit/$d/$e') TOMBR('/QSYS.LIB/IUNIT.LIB/$d.FILE/$mbr.MBR') MBROPT(*ADD) CVTDTA(*AUTO) STMFCCSID(*STMF) DBFCCSID(*FILE)" >/dev/null
    iu1 "CHGPFM FILE(IUNIT/$d) MBR($mbr) SRCTYPE($atr) TEXT('Created by Remain Software')" >/dev/null
  done
done
iu1 "CRTBNDCL PGM(IUNIT/I_BUILD) SRCFILE(IUNIT/QCLLESRC) SRCMBR(I_BUILD)"
"$bin" --libl QGPL system "CALL IUNIT/I_BUILD PARM(IUNIT)" >/dev/null 2>&1
"$bin" sql "SELECT OBJNAME, OBJTYPE FROM TABLE(QSYS2.OBJECT_STATISTICS('IUNIT','*PGM *CMD *MSGF')) ORDER BY 2, 1"
iut() { "$bin" --libl IUNIT,QGPL batch "$1" 2>&1 | grep -E "^(UTE|CPF9898|CPF124)" | sed -E 's/[0-9]{8}/HHMMSSmm/; s/^(CPF124[01]: Job )[0-9]{6}/\1nnnnnn/'; }
iut "RUNONE IUNIT PASS"
iut "RUNALL IUNIT"

step "IBM i Unit (its framework and samples as the extension installs them, then its sample suites)"
# Its library setting (ibmiUnit.library) is IBMIUT here: IUNIT is the
# IUNIT framework's library above; the user makes both libraries, as its
# README says. The extension (TypeScript) runs these CL commands over
# SSH; they are run here as they are. Test names: the EXPORT procedures whose names start
# with TEST (discovery.ts), the suite's object 'T' + its file name.
zs="$work/zunit/ibmi-unit-source-0.1.3"
"$bin" put "$zs/framework" /home/ibmiunit/framework >/dev/null
"$bin" put "$zs/samples/qrpglesrc" /home/ibmiunit/samples >/dev/null
zu() { "$bin" --libl IUNITSAMP,IBMIUT,QGPL --curlib IBMIUT system "$1" 2>&1 | grep -E "^(CP[CDF]|RNS|SQL[0-9])" | tail -1; }
zu "CRTLIB IBMIUT"
zu "CRTLIB IUNITSAMP"
zu "RUNSQL SQL('$(grep -v '^\s*--' "$zs/framework/iunitres.sql" | tr '\n' ' ' | sed 's/{LIB}/IBMIUT/g; s/  */ /g; s/^ //; s/ $//')') COMMIT(*NONE) NAMING(*SQL)"
zu "RUNSQL SQL('CREATE INDEX IBMIUT.IUNITRES1 ON IBMIUT.IUNITRES (RUNID, TESTOBJ, SEQ)') COMMIT(*NONE) NAMING(*SQL)"
zf=/home/ibmiunit/framework; zp=/home/ibmiunit/samples
zu "CRTRPGMOD MODULE(IBMIUT/TESTASSERT) SRCSTMF('$zf/testassert.rpgle') INCDIR('$zf') DBGVIEW(*SOURCE) TGTCCSID(*JOB) REPLACE(*YES)"
zu "CRTSRVPGM SRVPGM(IBMIUT/TESTASSERT) MODULE(IBMIUT/TESTASSERT) EXPORT(*ALL) ACTGRP(*CALLER) REPLACE(*YES) TEXT('IBM i Unit assertions')"
zu "CRTSQLRPGI OBJ(IBMIUT/RUNTEST) SRCSTMF('$zf/runtest.sqlrpgle') OBJTYPE(*MODULE) COMMIT(*NONE) DBGVIEW(*SOURCE) RPGPPOPT(*LVL2) REPLACE(*YES) COMPILEOPT('INCDIR(''$zf'') TGTCCSID(*JOB)')"
zu "CRTPGM PGM(IBMIUT/RUNTEST) MODULE(IBMIUT/RUNTEST) BNDSRVPGM((IBMIUT/TESTASSERT)) BNDDIR(QC2LE) ACTGRP(*NEW) REPLACE(*YES) TEXT('IBM i Unit test runner')"
zu "CRTRPGMOD MODULE(IUNITSAMP/MATHUTIL) SRCSTMF('$zp/mathutil.rpgle') INCDIR('$zp') DBGVIEW(*SOURCE) TGTCCSID(*JOB) REPLACE(*YES)"
zu "CRTSRVPGM SRVPGM(IUNITSAMP/MATHUTIL) MODULE(IUNITSAMP/MATHUTIL) EXPORT(*ALL) REPLACE(*YES) TEXT('Sample math utilities')"
zu "CRTRPGMOD MODULE(IUNITSAMP/STRUTIL) SRCSTMF('$zp/strutil.rpgle') INCDIR('$zp') DBGVIEW(*SOURCE) TGTCCSID(*JOB) REPLACE(*YES)"
zu "CRTSRVPGM SRVPGM(IUNITSAMP/STRUTIL) MODULE(IUNITSAMP/STRUTIL) EXPORT(*ALL) REPLACE(*YES) TEXT('Sample string utilities')"
zu "CRTBNDRPG PGM(IUNITSAMP/CALCTAX) SRCSTMF('$zp/calctax.rpgle') DBGVIEW(*SOURCE) TGTCCSID(*JOB) REPLACE(*YES) TEXT('Sample sales tax program')"
for t in mathutil strutil calctax demo_failures; do
  f="$zs/samples/qrpglesrc/$t.test.rpgle"
  obj=$(echo "T${t^^}" | tr -cd 'A-Z0-9_' | cut -c1-10)
  bnd=$(grep -i '^//#BNDSRVPGM:' "$f" | sed 's/^[^:]*: *//' | tr 'a-z' 'A-Z')
  tests=$(grep -i -E '^\s*dcl-proc\s+test\w*\s.*export' "$f" | sed -E 's/^\s*dcl-proc\s+(\w+).*/\1/I' | tr 'a-z' 'A-Z' | paste -sd,)
  srvs="(IBMIUT/TESTASSERT)"; [ -n "$bnd" ] && srvs="$srvs ($bnd)"
  zu "CRTRPGMOD MODULE(IBMIUT/$obj) SRCSTMF('$zp/$t.test.rpgle') INCDIR('$zf' '$zp') OPTION(*EVENTF *SRCSTMT) DBGVIEW(*SOURCE) TGTCCSID(*JOB) REPLACE(*YES)"
  zu "CRTSRVPGM SRVPGM(IBMIUT/$obj) MODULE(IBMIUT/$obj) EXPORT(*ALL) BNDSRVPGM($srvs) ACTGRP(*CALLER) REPLACE(*YES) TEXT('IBM i Unit test suite')"
  "$bin" --libl IUNITSAMP,IBMIUT,QGPL --curlib IBMIUT system "CALL PGM(IBMIUT/RUNTEST) PARM('20261031000000000LEGACY001' 'IBMIUT' '$obj' '$tests')" >/dev/null 2>&1
done
"$bin" sql "SELECT TESTOBJ, TESTNAME, STATUS, ASSERTS, CAST(MESSAGE AS VARCHAR(60)) FROM IBMIUT.IUNITRES ORDER BY TESTOBJ, SEQ"

step "XMLSERVICE (its Makefile's build-procedure and build-test targets, a command through iPLUG4K, then its PHP tests' XML)"
# configure (Python) only puts its defaults into three .in files: done
# here with sed, as it does. The Makefile's rules are run as they are.
xd="$work/xmlservice"
for f in src/plugconf.rpgle src/xmlstoredp.sql; do
  sed 's/@LIBRARY@/XMLSERVICE/g; s/@DEBUG@/*ALL/g; s/@TARGET_RELEASE@/*CURRENT/g' "$xd/$f.in" > "$xd/$f"
done
"$bin" put "$xd" /home/xmlservice >/dev/null
xs() { "$bin" --libl XMLSERVICE,QGPL --curlib XMLSERVICE system "$1" 2>&1; }
xs "CRTLIB XMLSERVICE" | tail -1
xmods="plugconf plugbug plugipc plugrun plugperf plugcach plugerr plugsql plugdb2 plugpase pluglic plugsig plugconv plugxml plugile"
for m in $xmods; do
  xs "CRTRPGMOD MODULE(XMLSERVICE/$m) SRCSTMF('/home/xmlservice/src/$m.rpgle') DBGVIEW(*ALL) REPLACE(*YES) TGTCCSID(37) TGTRLS(*CURRENT)" | grep -E "^RNS93" | tail -1
done
xs "CRTSQLRPGI OBJ(XMLSERVICE/XMLSTOREDP) SRCSTMF('/home/xmlservice/src/xmlstoredp.sqlrpgle') OBJTYPE(*MODULE) REPLACE(*YES) RPGPPOPT(*LVL2) COMPILEOPT('INCDIR(''/home/xmlservice/src/'') TGTCCSID(37)') TGTRLS(*CURRENT)" | grep -E "^RNS93" | tail -1
xs "CRTSRCPF XMLSERVICE/QSRVSRC" | tail -1
xs "CPYFRMSTMF FROMSTMF('/home/xmlservice/src/xmlstoredp.bnd') TOMBR('/qsys.lib/XMLSERVICE.lib/qsrvsrc.file/xmlstoredp.mbr') MBROPT(*REPLACE)" | tail -1
xlist=""
for m in xmlstoredp $xmods; do xlist="$xlist XMLSERVICE/$(echo $m | tr 'a-z' 'A-Z')"; done
xs "CRTSRVPGM SRVPGM(XMLSERVICE/XMLSTOREDP) MODULE($xlist) EXPORT(*SRCFILE) SRCFILE(XMLSERVICE/QSRVSRC) ACTGRP(*CALLER) TGTRLS(*CURRENT)" | tail -1
xs "RUNSQLSTM SRCSTMF('/home/xmlservice/src/xmlstoredp.sql')" | grep -E "^SQL79" | tail -1
"$bin" --libl XMLSERVICE,QGPL sql "CALL XMLSERVICE.IPLUG4K('*NA', '*here', '<?xml version=''1.0''?><xmlservice><cmd>CHGLIBL LIBL(QGPL)</cmd></xmlservice>', ?)" 2>&1 | tr -d '\n'; echo
# The Makefile's build-test target: the ZZ test programs and service
# programs its tests call (zzjava needs Java: not built).
for p in zzbigboy zzcall zzcust zzdeep zzerich zzmore zznone zzsimp zzvlad zzvlad2 zzvlad3; do
  xs "CRTRPGMOD MODULE(XMLSERVICE/$p) SRCSTMF('/home/xmlservice/src/$p.rpgle') DBGVIEW(*ALL) REPLACE(*YES) TGTCCSID(37)" | grep -E "^RNS93" | grep -v RNS9305
  xs "CRTPGM PGM(XMLSERVICE/$p) MODULE(XMLSERVICE/$p)" | grep -v CPC5D07
done
for p in zzsrv zzsrv6; do
  xs "CRTRPGMOD MODULE(XMLSERVICE/$p) SRCSTMF('/home/xmlservice/src/$p.rpgle') DBGVIEW(*ALL) REPLACE(*YES) TGTCCSID(37)" | grep -E "^RNS93" | grep -v RNS9305
  xs "CRTSRVPGM SRVPGM(XMLSERVICE/$p) MODULE(XMLSERVICE/$p) EXPORT(*ALL) ACTGRP(*CALLER)" | grep -v CPC5D0B
done
xs "CRTSRCPF XMLSERVICE/QCLSRC" >/dev/null
xs "CPYFRMSTMF FROMSTMF('/home/xmlservice/src/zzshlomo.clp') TOMBR('/qsys.lib/XMLSERVICE.lib/qclsrc.file/zzshlomo.mbr') MBROPT(*REPLACE)" >/dev/null
xs "CRTCLPGM PGM(XMLSERVICE/ZZSHLOMO) SRCFILE(XMLSERVICE/QCLSRC)" | grep -E "^CPF"
# Its PHP tests' XML, through the iPLUG procedures (tools/xmlservice_tests.py;
# about 15 minutes with a release binary; XMLSERVICE_TESTS=0 leaves it out).
if [ "${XMLSERVICE_TESTS:-1}" != 0 ]; then
  # AS4TAINR_QSH=1: its <sh> and <qsh> tests run shell commands (ls).
  AS4TAINR_QSH=1 AS4TAINR_BIN="$bin" python3 tools/xmlservice_tests.py "$xd" | tail -1
fi

step "IBMi-topics (import, then SPLFDLT *LOGONLY and *BOTH in batch jobs)"
"$bin" import "$work/topics" --lib TOPICS 2>/dev/null | tail -1
"$bin" batch "DSPLIBL OUTPUT(*PRINT)" >/dev/null 2>&1
topics() { "$bin" --libl TOPICS,QGPL --curlib TOPICS batch "$1" 2>&1 | grep -E "spool files|USP0|CPF1240"; }
topics "SPLFDLT ACTION(*LOGONLY)"
topics "SPLFDLT"
