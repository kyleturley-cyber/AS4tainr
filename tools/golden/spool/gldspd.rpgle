**FREE
// Golden spooled file APIs for as4tainr (tools/golden/spool.txt): the
// spooled file GLDSPL printed by GLDSPL, as QGYOLSPL (OSPL0300), QGYGTLE
// and QSPGETSP (SPFR0300, into a user space) return it. Each piece is
// written as hexadecimal to GLDSPO (ID, VAL: 50 bytes a record), printed
// with CPYF.
ctl-opt dftactgrp(*no) actgrp(*new);
// The status filter is *ALL: the file may still be open when listed.

dcl-f GLDSPO usage(*output) rename(GLDSPO: OUTR);

// Errors come back here (bytes provided 16), not as exceptions: an
// exception left unhandled would wait for a reply to its inquiry.
dcl-ds err qualified;
  prov int(10) inz(16);
  avail int(10) inz(0);
  msgid char(7);
  *n char(1);
end-ds;

dcl-pr qgyolspl extpgm('QGYOLSPL');
  rcv char(32767) options(*varsize);
  rcvlen int(10) const;
  info char(80);
  count int(10) const;
  sort char(256) const options(*varsize);
  filter char(256) const options(*varsize);
  job char(26) const;
  fmt char(8) const;
  e likeds(err);
end-pr;
dcl-pr qcmdexc extpgm('QCMDEXC');
  cmd char(200) const;
  len packed(15 : 5) const;
end-pr;
dcl-pr qgygtle extpgm('QGYGTLE');
  rcv char(32767) options(*varsize);
  rcvlen int(10) const;
  handle char(4) const;
  info char(80);
  count int(10) const;
  start int(10) const;
  e likeds(err);
end-pr;
dcl-pr qgyclst extpgm('QGYCLST');
  handle char(4) const;
  e likeds(err);
end-pr;
dcl-pr quscrtus extpgm('QUSCRTUS');
  name char(20) const;
  attr char(10) const;
  size int(10) const;
  init char(1) const;
  aut char(10) const;
  text char(50) const;
  replace char(10) const;
  e likeds(err);
end-pr;
dcl-pr qusptrus extpgm('QUSPTRUS');
  name char(20) const;
  ptr pointer;
  e likeds(err);
end-pr;
dcl-pr qspopnsp extpgm('QSPOPNSP');
  handle int(10);
  job char(26) const;
  intjob char(16) const;
  intsplf char(16) const;
  splf char(10) const;
  number int(10) const;
  buffers int(10) const;
  e likeds(err);
end-pr;
dcl-pr qspgetsp extpgm('QSPGETSP');
  handle int(10) const;
  space char(20) const;
  fmt char(8) const;
  buffer int(10) const;
  atend char(10) const;
  e likeds(err);
end-pr;
dcl-pr qspclosp extpgm('QSPCLOSP');
  handle int(10);
  e likeds(err);
end-pr;

dcl-ds filter qualified;
  usercount int(10) inz(1);
  user char(10) inz('*CURRENT');
  *n char(2) inz(x'0000');
  outqcount int(10) inz(1);
  outq char(10) inz('*ALL');
  outqlib char(10) inz(*blank);
  formtype char(10) inz('*ALL');
  userdata char(10) inz('GLDSPL');
  statuscount int(10) inz(1);
  status char(10) inz('*ALL');
  *n char(2) inz(x'0000');
  devcount int(10) inz(1);
  dev char(10) inz('*ALL');
  *n char(2) inz(x'0000');
end-ds;
dcl-ds info len(80) qualified;
  total int(10) pos(1);
  returned int(10) pos(5);
  handle char(4) pos(9);
end-ds;
dcl-ds rec len(200) qualified;
  job char(10) pos(1);
  user char(10) pos(11);
  number char(6) pos(21);
  splf char(10) pos(27);
  splfnbr int(10) pos(37);
end-ds;
dcl-s sort char(256) inz(*allx'00');
dcl-s handle int(10);
dcl-s p pointer;
dcl-s used int(10) based(pused);
dcl-s pused pointer;
dcl-s dummy char(1);

// The spooled file is made here, in this job (submitted with SBMJOB:
// PASE `system` writes a command's spooled output to standard output and
// keeps none).
qcmdexc('CALL PGM(AS4TRAINR1/GLDSPL)' : 28);
qgyolspl(dummy : 0 : info : -1 : sort : filter : ' ' : 'OSPL0300' : err);
note('1');
hexout('L' : %addr(info) : 80);
if info.total > 0;
  qgygtle(rec : %size(rec) : info.handle : info : 1 : info.total : err);
endif;
note('2');
hexout('R' : %addr(rec) : %size(rec));
hexout('I' : %addr(info) : 80);
qgyclst(info.handle : err);
note('3');

quscrtus('GLDSPC    QTEMP' : 'GLDSPC' : 65536 : x'00' : '*ALL' :
         'golden spooled file data' : '*YES' : err);
note('4');
qspopnsp(handle : rec.job + rec.user + rec.number : ' ' : ' ' : rec.splf :
         rec.splfnbr : -1 : err);
note('5');
qspgetsp(handle : 'GLDSPC    QTEMP' : 'SPFR0300' : -1 : '*ERROR' : err);
note('6');
qspclosp(handle : err);
note('7');
qusptrus('GLDSPC    QTEMP' : p : err);
note('8');
// The general header: the size used is at offset 88 (after 64 bytes of
// user area, header size, structure level, spooled file level, format,
// completion indicator and a reserved byte).
if p <> *null;
  pused = p + 88;
  if used > 0 and used <= 65536;
    hexout('S' : p : used);
  else;
    hexout('S' : p : 512);
  endif;
endif;
// The spooled file is not needed any more.
qcmdexc('DLTSPLF FILE(GLDSPL) SPLNBR(*LAST)' : 34);
*inlr = *on;

// An API's error (its message ID), or OK: record E<step>.
dcl-proc note;
  dcl-pi *n;
    step char(1) const;
  end-pi;
  ID = 'E' + step;
  if err.avail > 0;
    VAL = err.msgid;
  else;
    VAL = 'OK';
  endif;
  write OUTR;
  err.avail = 0;
end-proc;

// Bytes as hexadecimal (a procedure named DUMP would be the DUMP
// operation), 50 bytes a record: ID is the prefix and the
// offset in the piece.
dcl-proc hexout;
  dcl-pi *n;
    prefix char(1) const;
    at pointer const;
    len int(10) const;
  end-pi;
  dcl-s b char(1) based(pb);
  dcl-s pb pointer;
  dcl-s i int(10);
  dcl-s n int(10);
  dcl-s hex char(16) inz('0123456789ABCDEF');
  dcl-ds byte;
    c char(1);
    u uns(3) overlay(c);
  end-ds;
  dcl-s line varchar(100);
  for i = 0 to len - 1;
    if %rem(i : 50) = 0;
      line = '';
    endif;
    pb = at + i;
    c = b;
    n = u;
    line += %subst(hex : %div(n : 16) + 1 : 1) + %subst(hex : %rem(n : 16) + 1 : 1);
    if %rem(i : 50) = 49 or i = len - 1;
      ID = prefix + %editc(%dec(i - %rem(i : 50) : 6 : 0) : 'X');
      VAL = line;
      write OUTR;
    endif;
  endfor;
end-proc;
