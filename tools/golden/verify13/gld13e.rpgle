**FREE
// Golden (tools/golden/verify13.txt): a data structure named as the
// record format its LIKEREC names (RNF3546): the whole listing.
ctl-opt dftactgrp(*no) actgrp(*new);

dcl-f GLD13OUT usage(*output) rename(GLD13OUT: OUTR);

dcl-ds psds psds;
  status zoned(5: 0) pos(11);
  msgid char(7) pos(40);
end-ds;

dcl-s s varchar(10) inz('abc');
dcl-s r varchar(10);
dcl-s start int(10) inz(10);

monitor;
  r = %subst(s: start: 2);
on-error;
  res('P01': %char(status) + ' ' + msgid);
endmon;
res('P02': %char(%status) + ' ' + msgid);

*inlr = *on;

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
