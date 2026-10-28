**FREE
// Golden (tools/golden/verify12.txt): the PSDS after a MONITORed
// %SUBST error (status, message ID), as records of AS4TRAINR1/GLD12OUT.
ctl-opt dftactgrp(*no) actgrp(*new);

dcl-f GLD12OUT usage(*output) rename(GLD12OUT: OUTR);

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
