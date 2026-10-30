**FREE
// verify29: verify28's GLD28U as it was, for its listing (CRTRPGMOD
// stopped with severity 20 errors)
ctl-opt dftactgrp(*no);
dcl-pr gld29two int(10) extproc('GLD29TWO');
end-pr;
dcl-pi *n;
  r char(10);
end-pi;
r = %char(gld29two());
*inlr = *on;
