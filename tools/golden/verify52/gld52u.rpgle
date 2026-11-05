**FREE
// verify52: keep GLD52F open for update 60 s, a record read.
ctl-opt dftactgrp(*no) actgrp(*caller);
dcl-f GLD52F usage(*update) rename(GLD52F:GLD52FR);
dcl-pr qcmdexc extpgm('QCMDEXC');
  cmd char(32) const;
  len packed(15:5) const;
end-pr;
read GLD52FR;
qcmdexc('DLYJOB DLY(60)': 14);
*inlr = *on;
