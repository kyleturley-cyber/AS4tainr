**FREE
// verify48: open a file another job holds *EXCL; the status and the
// message the open gives, written to GLD48O.
ctl-opt dftactgrp(*no) actgrp(*caller);
dcl-f GLD48F usage(*input) usropn rename(GLD48F:GLD48FR);
dcl-f GLD48O usage(*output) rename(GLD48O:GLD48OR);
dcl-ds psds psds;
  msgid char(7) pos(40);
end-ds;
monitor;
  open GLD48F;
  res = 'OPENED';
on-error;
  res = 'ST ' + %char(%status) + ' ' + msgid;
endmon;
write GLD48OR;
*inlr = *on;
