**FREE
// verify47: open a file another job holds *EXCL; the status and the
// message the open gives, written to GLD47O.
ctl-opt dftactgrp(*no) actgrp(*caller);
dcl-f GLD47F usage(*input) usropn;
dcl-f GLD47O usage(*output) rename(GLD47O:GLD47OR);
dcl-ds psds psds;
  msgid char(7) pos(40);
end-ds;
monitor;
  open GLD47F;
  out = 'OPENED';
on-error;
  out = 'ST ' + %char(%status) + ' ' + msgid;
endmon;
write GLD47OR;
*inlr = *on;
