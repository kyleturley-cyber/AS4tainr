**FREE
// Golden (tools/golden/verify13.txt): QUSRJOBI JOBI0400 of this job and
// QWDRJOBD JOBD0100 of QGPL/QDFTJOBD, as hexadecimal records.
ctl-opt dftactgrp(*no) actgrp(*new);

dcl-f GLD13OUT usage(*output) rename(GLD13OUT: OUTR);

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

dcl-s buf char(2000);

buf = *blanks;
QUSRJOBI(buf: %size(buf): 'JOBI0400': '*': ' ');
dumphex('J': buf);
buf = *blanks;
QWDRJOBD(buf: %size(buf): 'JOBD0100': 'QDFTJOBD  QGPL': errc);
dumphex('D': buf);

*inlr = *on;

// The bytes returned (the first 4 bytes say how many) as hexadecimal, 25
// bytes a record.
dcl-proc dumphex;
  dcl-pi *n;
    tag char(1) const;
    data char(2000);
  end-pi;
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
  dcl-ds four;
    c4 char(4);
    returned int(10) overlay(c4);
  end-ds;
  c4 = %subst(data: 1: 4);
  n = returned;
  if n < 4 or n > 2000;
    n = 8;
  endif;
  res(tag + '000': %char(n));
  i = 1;
  dow i <= n;
    hex = '';
    k = i;
    dow k <= i + 24 and k <= n;
      ch = %subst(data: k: 1);
      b = u;
      hex += %subst(digits: %div(b: 16) + 1: 1) + %subst(digits: %rem(b: 16) + 1: 1);
      k += 1;
    enddo;
    res(tag + %editc(%dec(i: 4: 0): 'X'): hex);
    i += 25;
  enddo;
end-proc;

dcl-proc res;
  dcl-pi *n;
    id char(10) const;
    val varchar(200) const;
  end-pi;
  dcl-ds rec likerec(OUTR: *output);
  rec.ID = id;
  rec.VAL = val;
  write OUTR rec;
end-proc;
