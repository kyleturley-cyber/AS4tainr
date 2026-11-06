**FREE
// verify58: IN *LOCK of data area GLD58A; the status and message ID
// written to GLD58O; with parameter 'H' it holds the lock 40 seconds.
ctl-opt dftactgrp(*no);
dcl-f GLD58O usage(*output) rename(GLD58O:GLD58R);
dcl-pi *n;
  mode char(1);
end-pi;
dcl-ds psds psds;
  msgid char(7) pos(40);
end-ds;
dcl-s val char(10) dtaara('GLD58A');
dcl-pr qcmdexc extpgm('QCMDEXC');
  cmd char(32) const;
  len packed(15:5) const;
end-pr;
monitor;
  in *lock val;
  res = 'LOCK GOT ' + %trim(val);
on-error;
  res = 'LOCK ST ' + %char(%status) + ' ' + msgid;
endmon;
write GLD58R;
if mode = 'H';
  qcmdexc('DLYJOB DLY(40)': 14);
endif;
*inlr = *on;
