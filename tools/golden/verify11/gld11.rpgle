**FREE
// Golden values (tools/golden/verify11.txt), each a record of
// AS4TRAINR1/GLD11OUT (ID, VAL), printed with CPYF:
// - the PSDS after a MONITORed %SUBST error (status, message ID);
// - the event numbers XML-SAX passes to a handler, with each event's data;
// - QUSRJOBI JOBI0400 and QWDRJOBD JOBD0100 receivers, as hexadecimal.
ctl-opt dftactgrp(*no) actgrp(*new);

dcl-f GLD11OUT usage(*output) rename(GLD11OUT: OUTR);

dcl-ds psds psds;
  status zoned(5: 0) pos(11);
  msgid char(7) pos(40);
end-ds;

dcl-ds errc qualified;
  provided int(10) inz(0);
  available int(10) inz(0);
end-ds;

dcl-pr QUSRJOBI extpgm('QUSRJOBI');
  rcv char(2000) options(*varsize);
  len int(10) const;
  fmt char(8) const;
  job char(26) const;
  intid char(16) const;
end-pr;

dcl-pr QWDRJOBD extpgm('QWDRJOBD');
  rcv char(2000) options(*varsize);
  len int(10) const;
  fmt char(8) const;
  jobd char(20) const;
  err likeds(errc);
end-pr;

dcl-s s varchar(10) inz('abc');
dcl-s r varchar(10);
dcl-s start int(10) inz(10);
dcl-s buf char(2000);
dcl-s seq int(10) inz(0);
dcl-s doc varchar(200);

// PSDS after a MONITORed %SUBST that is out of range.
monitor;
  r = %subst(s: start: 2);
on-error;
  res('P01': %char(status) + ' ' + msgid);
endmon;
res('P02': %char(%status) + ' ' + msgid);

// XML-SAX: the event numbers, in order.
doc = '<?xml version="1.0"?><a x="1">t<b/><!-- c --><![CDATA[d]]>&amp;</a>';
xml-sax %handler(saxh: seq) %xml(doc);

// QUSRJOBI JOBI0400 of this job, and QWDRJOBD JOBD0100 of GLD11JD.
buf = *blanks;
QUSRJOBI(buf: %size(buf): 'JOBI0400': '*': ' ');
dumphex('J': buf);
buf = *blanks;
QWDRJOBD(buf: %size(buf): 'JOBD0100': 'GLD11JD   AS4TRAINR1': errc);
dumphex('D': buf);

*inlr = *on;

dcl-proc saxh;
  dcl-pi *n int(10);
    n int(10);
    event int(10) value;
    str pointer value;
    len int(20) value;
    exc int(10) value;
  end-pi;
  dcl-s text varchar(40);
  n += 1;
  text = '';
  if str <> *null and len > 0;
    text = %str(str: %min(len: 40));
  endif;
  res('X' + %editc(%dec(n: 3: 0): 'X'):
      %char(event) + ' ' + %char(len) + ' ' + %char(exc) + ' ' + text);
  return 0;
end-proc;

// The bytes returned (the first 4 bytes say how many) as hexadecimal, 25
// bytes a record.
dcl-proc dumphex;
  dcl-pi *n;
    tag char(1) const;
    data char(2000);
  end-pi;
  dcl-ds lens qualified;
    returned int(10);
  end-ds;
  dcl-s i int(10);
  dcl-s k int(10);
  dcl-s n int(10);
  dcl-s hex varchar(60);
  dcl-s digits char(16) inz('0123456789ABCDEF');
  dcl-s b int(10);
  dcl-ds one;
    ch char(1);
    u uns(3) overlay(ch);
  end-ds;
  lens = %subst(data: 1: 4);
  n = lens.returned;
  if n < 4 or n > 2000;
    n = 8;
  endif;
  res(tag + '000': %char(n));
  i = 1;
  dow i <= n;
    hex = '';
    for k = i to %min(i + 24: n);
      ch = %subst(data: k: 1);
      b = u;
      hex += %subst(digits: %div(b: 16) + 1: 1) + %subst(digits: %rem(b: 16) + 1: 1);
    endfor;
    res(tag + %editc(%dec(i: 4: 0): 'X'): hex);
    i += 25;
  enddo;
end-proc;

dcl-proc res;
  dcl-pi *n;
    id char(10) const;
    val varchar(200) const;
  end-pi;
  dcl-ds outr likerec(OUTR: *output);
  outr.ID = id;
  outr.VAL = val;
  write OUTR outr;
end-proc;
