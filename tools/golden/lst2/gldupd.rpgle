**FREE
// Golden listing of an update file (tools/golden/listings2.txt): input
// and output specifications are generated for its record format.
ctl-opt dftactgrp(*no);
/copy GLDLSRC,GLDCPY

dcl-f GLDLSTF usage(*update);

dcl-s n packed(5: 0);

read GLDLSTR;
dow not %eof(GLDLSTF);
  n += 1;
  AMT += 1;
  update GLDLSTR;
  read GLDLSTR;
enddo;
dsply %char(n);
*inlr = *on;
