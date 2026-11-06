**FREE
// verify55: an *ISO date literal after /SET DATFMT(*MDY/).
ctl-opt dftactgrp(*no);
/SET DATFMT(*MDY/)
dcl-s d date inz(D'2026-11-05');
*inlr = *on;
