**FREE
// verify59: IN *LOCK of GLD59A, then end: with LR on (mode 'L') or with
// RETURN and LR off (mode 'R'); no OUT, no UNLOCK.
ctl-opt dftactgrp(*no) actgrp(*caller);
dcl-pi *n;
  mode char(1);
end-pi;
dcl-s val char(10) dtaara('GLD59A');
in *lock val;
if mode = 'L';
  *inlr = *on;
endif;
return;
