**FREE
// Golden (tools/golden/verify22.txt): a run-time error MONITOR takes
// (%SUBST out of range, RNX0100), for the printed job log.
ctl-opt dftactgrp(*no) actgrp(*caller);
dcl-s s varchar(10) inz('abc');
dcl-s r varchar(10);
dcl-s n int(10) inz(8);
monitor;
  r = %subst(s: n: 2);
on-error;
  r = 'caught';
endmon;
*inlr = *on;
return;
