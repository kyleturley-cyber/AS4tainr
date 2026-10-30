**FREE
// verify28: a program bound to GLD28M using the export only the first
// *CURRENT block lists
ctl-opt dftactgrp(*no);
dcl-pr gld28two int(10) extproc('GLD28TWO');
end-pr;
dcl-pi *n;
  r char(10);
end-pi;
r = %char(gld28two());
*inlr = *on;
