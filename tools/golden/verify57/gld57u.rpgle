**FREE
// verify57: open for update a file another job holds *EXCL; the status
// and message ID written to GLD57O.
ctl-opt dftactgrp(*no);
dcl-f GLD57F usage(*update) usropn rename(GLD57F:GLD57FR);
dcl-f GLD57O usage(*output) rename(GLD57O:GLD57R);
dcl-ds psds psds;
  msgid char(7) pos(40);
end-ds;
monitor;
  open GLD57F;
  res = 'UPD OPENED';
on-error;
  res = 'UPD ST ' + %char(%status) + ' ' + msgid;
endmon;
write GLD57R;
*inlr = *on;
