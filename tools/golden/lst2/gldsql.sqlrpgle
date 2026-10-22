**FREE
// Golden listing of embedded SQL (tools/golden/listings2.txt).
ctl-opt dftactgrp(*no);
dcl-s n int(10);
exec sql select count(*) into :n from GLDLSTF;
dsply %char(n);
*inlr = *on;
