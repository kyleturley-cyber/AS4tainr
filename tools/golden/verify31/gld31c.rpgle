**FREE
// verify31: a program run in its caller's activation group: a count in
// its global storage, written to AS4TRAINR1/GLD31OUT each call.
ctl-opt dftactgrp(*no) actgrp(*caller);
dcl-pr QCMDEXC extpgm('QCMDEXC');
  cmd char(300) const;
  len packed(15:5) const;
end-pr;
dcl-s n int(10) inz(0);
n += 1;
QCMDEXC('RUNSQL SQL(''INSERT INTO AS4TRAINR1.GLD31OUT VALUES (900, ''''count '
  + %char(n) + ''''')'') COMMIT(*NONE)' : 300);
return;
