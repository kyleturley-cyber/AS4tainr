**FREE
// verify55: an *MDY/ date literal after /SET DATFMT(*MDY/).
ctl-opt dftactgrp(*no);
/SET DATFMT(*MDY/)
dcl-s d date inz(D'11/05/26');
*inlr = *on;
