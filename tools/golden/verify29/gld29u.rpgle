**FREE
// verify29: a program bound to GLD29M using the export only the first
// *CURRENT block lists (no control options: verify28's GLD28U with
// ctl-opt dftactgrp(*no) did not compile as a module)
dcl-pr gld29two int(10) extproc('GLD29TWO');
end-pr;
dcl-pi *n;
  r char(10);
end-pi;
r = %char(gld29two());
*inlr = *on;
