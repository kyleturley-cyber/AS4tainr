**FREE
// verify46: DSPPGMREF of an RPG program with a file and EXTPGM calls.
ctl-opt dftactgrp(*no) actgrp(*caller);
dcl-f GLD46P keyed usage(*input);
dcl-pr qcmdexc extpgm('QCMDEXC');
  cmd char(100) const;
  len packed(15: 5) const;
end-pr;
dcl-pr callc extpgm('GLD46C') end-pr;
setll *loval GLD46P;
read GLD46P;
qcmdexc('DLYJOB DLY(1)': 13);
callc();
*inlr = *on;
